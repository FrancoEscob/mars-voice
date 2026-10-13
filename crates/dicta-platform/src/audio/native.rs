//! Native PipeWire capture, negotiated PCM signed 16-bit little endian / 16 kHz /
//! mono. PipeWire performs rate conversion and downmixing. Also the registry
//! inspection shared with playback, and the probe that decides whether
//! PipeWire is this session's audio server.
use super::{AudioInventory, AudioStatus, PcmBuffer, SAMPLE_RATE, select_source};
use anyhow::{Context, Result, anyhow};
use dicta_core::AudioDevice;
use pipewire::{self as pw, properties::properties, spa};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

pub(super) fn capture_loop(
    inventory: &AudioInventory,
    device: Option<String>,
    tx: mpsc::Sender<Vec<u8>>,
    status: watch::Sender<AudioStatus>,
    stop: pw::channel::Receiver<bool>,
    stopping: Arc<AtomicBool>,
) -> Result<()> {
    // Registry/metadata inspection creates no stream and opens no microphone.
    let target = select_source(inventory, device.as_deref())?;
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let graceful = Rc::new(Cell::new(false));
    let error = Rc::new(RefCell::new(None::<String>));
    let ready = Rc::new(Cell::new(false));
    let pcm = Rc::new(RefCell::new(PcmBuffer::default()));
    let _stop = stop.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        let graceful = graceful.clone();
        move |flush| {
            graceful.set(flush);
            mainloop.quit();
        }
    });
    let _core_listener = core
        .add_listener_local()
        .error({
            let mainloop = mainloop.clone();
            let error = error.clone();
            move |_, _, _, message| {
                *error.borrow_mut() = Some(capture_error(&message));
                mainloop.quit();
            }
        })
        .register();
    let mut props = properties! {
        "media.type" => "Audio", "media.category" => "Capture", "media.role" => "Communication",
        "application.name" => "Dicta", "node.name" => "dicta-capture",
        // Request 20ms callbacks for responsive RMS. STT still aggregates PCM
        // into CHUNK_BYTES (100ms), independently of this requested latency.
        "node.latency" => "320/16000",
        // Pin this dictation to its resolved input, including hot-unplug races.
        "node.dont-fallback" => "true", "node.dont-reconnect" => "true",
    };
    // PipeWire target.object accepts node.name/object.serial, NOT global IDs.
    // Configuration and selection use the registry's stable node.name only.
    props.insert("target.object", target);
    let stream = pw::stream::StreamBox::new(&core, "Dicta dictado", props)?;
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed({
            let mainloop = mainloop.clone();
            let error = error.clone();
            move |_, _, _, state| {
                if let pw::stream::StreamState::Error(message) = state {
                    *error.borrow_mut() = Some(capture_error(&message));
                    mainloop.quit();
                }
            }
        })
        .param_changed({
            let mainloop = mainloop.clone();
            let error = error.clone();
            let ready = ready.clone();
            move |_, _, id, param| {
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                ready.set(false);
                let Some(param) = param else {
                    return;
                };
                let mut format = spa::param::audio::AudioInfoRaw::new();
                let valid = format.parse(param).is_ok()
                    && format.format() == spa::param::audio::AudioFormat::S16LE
                    && format.rate() == SAMPLE_RATE
                    && format.channels() == 1;
                if valid {
                    ready.set(true);
                } else {
                    *error.borrow_mut() =
                        Some("El dispositivo no negoció PCM16 mono a 16000 Hz".into());
                    mainloop.quit();
                }
            }
        })
        .process({
            let mainloop = mainloop.clone();
            let error = error.clone();
            let ready = ready.clone();
            let pcm = pcm.clone();
            let status = status.clone();
            let tx = tx.clone();
            move |stream, _| {
                if !ready.get() {
                    return;
                }
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let offset = data.chunk().offset() as usize;
                let size = data.chunk().size() as usize;
                let Some(bytes) = data.data() else {
                    return;
                };
                let result = offset
                    .checked_add(size)
                    .and_then(|end| bytes.get(offset..end))
                    .context("Buffer PipeWire inválido")
                    .and_then(|bytes| pcm.borrow_mut().push(bytes, &tx));
                match result {
                    Ok(level) => {
                        status.send_replace(AudioStatus {
                            recording: true,
                            level,
                            error: None,
                        });
                    }
                    Err(e) => {
                        *error.borrow_mut() = Some(e.to_string());
                        mainloop.quit();
                    }
                }
            }
        })
        .register()?;
    let mut format = spa::param::audio::AudioInfoRaw::new();
    format.set_format(spa::param::audio::AudioFormat::S16LE);
    format.set_rate(SAMPLE_RATE);
    format.set_channels(1);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: format.into(),
    };
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|e| anyhow!("Formato PipeWire: {e:?}"))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&bytes).context("Formato PipeWire inválido")?];
    // Do not use RT_PROCESS: callbacks are kept on our dedicated mainloop thread.
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    let timeout = mainloop.loop_().add_timer({
        let mainloop = mainloop.clone();
        let error = error.clone();
        let status = status.clone();
        move |_| {
            if !status.borrow().recording {
                *error.borrow_mut() =
                    Some("No llegó audio en 8 segundos; revisá el micrófono y PipeWire".into());
                mainloop.quit();
            }
        }
    });
    timeout
        .update_timer(Some(super::NO_AUDIO_TIMEOUT), None)
        .into_result()?;
    mainloop.run();
    let _ = stream.disconnect();
    if let Some(error) = error.borrow_mut().take() {
        return Err(anyhow!(error));
    }
    if graceful.get() {
        pcm.borrow_mut().flush(&tx)?;
    }
    status.send_replace(AudioStatus::default());
    Ok(())
}

fn capture_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("no target node available") || lower.contains("target node not found") {
        "El micrófono se desconectó o dejó de estar disponible. Reconectalo o elegí uno disponible en Ajustes > Micrófono".into()
    } else {
        format!("PipeWire: {message}")
    }
}
fn metadata_name(value: Option<&str>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(value?).ok()?;
    value
        .get("name")
        .and_then(|name| name.as_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
}

/// Which PipeWire nodes and `default` metadata keys an inventory reads.
#[derive(Clone, Copy)]
pub(crate) struct NodeKind {
    class: &'static str,
    default_key: &'static str,
    configured_key: &'static str,
    stalled: &'static str,
}
pub(crate) const SOURCES: NodeKind = NodeKind {
    class: "Audio/Source",
    default_key: "default.audio.source",
    configured_key: "default.configured.audio.source",
    stalled: "PipeWire no respondió al consultar micrófonos; revisá PipeWire y volvé a intentar",
};
/// Output devices. Their defaults land in the `*_source` fields of the inventory.
pub(crate) const SINKS: NodeKind = NodeKind {
    class: "Audio/Sink",
    default_key: "default.audio.sink",
    configured_key: "default.configured.audio.sink",
    stalled: "PipeWire no respondió al consultar las salidas de audio; revisá PipeWire y volvé a intentar",
};

/// Set once PipeWire was seen acting as the audio server. From then on this
/// process never falls back to cpal, so the native path keeps its own errors
/// (for example while PipeWire restarts). Negative answers are not cached:
/// PipeWire may come up after Mars voice at login.
static SERVES_AUDIO: AtomicBool = AtomicBool::new(false);

/// Registry snapshot for `kind`, or `None` when PipeWire is not this session's
/// audio server (daemon unreachable, or reachable without any audio node, as
/// on Ubuntu 22.04 where PulseAudio owns the sound cards). A stalled daemon is
/// still an error: the caller reports it instead of switching backends.
pub(crate) fn audio_server_nodes(kind: &NodeKind) -> Result<Option<AudioInventory>> {
    let known = SERVES_AUDIO.load(Ordering::Acquire);
    match scan(kind) {
        Ok(scan) if scan.serves_audio || known => {
            SERVES_AUDIO.store(true, Ordering::Release);
            Ok(Some(scan.inventory))
        }
        Ok(_) => Ok(None),
        Err(Scan::Unreachable(error)) if known => Err(error),
        Err(Scan::Unreachable(_)) => Ok(None),
        Err(Scan::Failed(error)) => Err(error),
    }
}

/// Whether playback should use PipeWire. A stalled daemon counts as PipeWire
/// so the native player reports it, exactly as before the cpal fallback.
pub(crate) fn serves_audio() -> bool {
    SERVES_AUDIO.load(Ordering::Acquire)
        || audio_server_nodes(&SINKS).map_or(true, |nodes| nodes.is_some())
}

struct Snapshot {
    inventory: AudioInventory,
    serves_audio: bool,
}
enum Scan {
    /// No PipeWire daemon to talk to.
    Unreachable(anyhow::Error),
    Failed(anyhow::Error),
}

/// Registry/metadata snapshot of one node class. Creates no stream.
/// Two server roundtrips include metadata events from proxies bound while the
/// registry is enumerated. Returns after at most 3 seconds if PipeWire stalls.
fn scan(kind: &NodeKind) -> Result<Snapshot, Scan> {
    let unreachable = |e: pw::Error| Scan::Unreachable(e.into());
    let failed = |e: pw::Error| Scan::Failed(e.into());
    let kind = *kind;
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(unreachable)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(unreachable)?;
    let core = context.connect_rc(None).map_err(unreachable)?;
    let registry = core.get_registry_rc().map_err(failed)?;
    let devices = Rc::new(RefCell::new(std::collections::BTreeMap::new()));
    let defaults = Rc::new(RefCell::new(AudioInventory::default()));
    let metadata_handles = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(Cell::new(false));
    let serves_audio = Rc::new(Cell::new(false));
    let _registry = registry
        .add_listener_local()
        .global({
            let devices = devices.clone();
            let defaults = defaults.clone();
            let registry = registry.clone();
            let metadata_handles = metadata_handles.clone();
            let serves_audio = serves_audio.clone();
            move |object| {
                let Some(props) = object.props.as_ref() else {
                    return;
                };
                if object.type_ == pw::types::ObjectType::Node
                    && props.get("media.class").is_some_and(is_audio_class)
                {
                    serves_audio.set(true);
                }
                if object.type_ == pw::types::ObjectType::Node
                    && props.get("media.class") == Some(kind.class)
                {
                    let Some(id) = props.get("node.name") else {
                        return;
                    };
                    let name = props
                        .get("node.description")
                        .or_else(|| props.get("node.nick"))
                        .unwrap_or(id);
                    devices.borrow_mut().insert(
                        object.id,
                        AudioDevice {
                            id: id.into(),
                            name: name.into(),
                        },
                    );
                } else if object.type_ == pw::types::ObjectType::Metadata
                    && props.get("metadata.name") == Some("default")
                {
                    if let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(object) {
                        let defaults = defaults.clone();
                        let listener = metadata
                            .add_listener_local()
                            .property(move |subject, key, _, value| {
                                if subject == pw::core::PW_ID_CORE {
                                    let mut defaults = defaults.borrow_mut();
                                    match key {
                                        Some(key) if key == kind.default_key => {
                                            defaults.default_source = metadata_name(value)
                                        }
                                        Some(key) if key == kind.configured_key => {
                                            defaults.configured_default_source =
                                                metadata_name(value)
                                        }
                                        None => {
                                            defaults.default_source = None;
                                            defaults.configured_default_source = None;
                                        }
                                        _ => {}
                                    }
                                }
                                0
                            })
                            .register();
                        // Listener must drop before its metadata proxy.
                        metadata_handles.borrow_mut().push((listener, metadata));
                    }
                }
            }
        })
        .global_remove({
            let devices = devices.clone();
            move |id| {
                devices.borrow_mut().remove(&id);
            }
        })
        .register();
    let pending = Rc::new(Cell::new(core.sync(0).map_err(failed)?));
    let rounds = Rc::new(Cell::new(0));
    let _core = core
        .add_listener_local()
        .done({
            let mainloop = mainloop.clone();
            let done = done.clone();
            let pending = pending.clone();
            let core = core.clone();
            let rounds = rounds.clone();
            move |id, seq| {
                if id != pw::core::PW_ID_CORE || seq != pending.get() {
                    return;
                }
                if rounds.get() == 0 {
                    rounds.set(1);
                    match core.sync(0) {
                        Ok(seq) => pending.set(seq),
                        Err(_) => mainloop.quit(),
                    };
                } else {
                    done.set(true);
                    mainloop.quit();
                }
            }
        })
        .error({
            let mainloop = mainloop.clone();
            move |_, _, _, _| mainloop.quit()
        })
        .register();
    let timer = mainloop.loop_().add_timer({
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });
    timer
        .update_timer(Some(Duration::from_secs(3)), None)
        .into_result()
        .map_err(|e| Scan::Failed(e.into()))?;
    mainloop.run();
    if !done.get() {
        return Err(Scan::Failed(anyhow!("{}", kind.stalled)));
    }
    let mut result = defaults.borrow().clone();
    result.devices = devices.borrow().values().cloned().collect();
    result.devices.sort_by(|a, b| a.name.cmp(&b.name));
    result.devices.dedup_by(|a, b| a.id == b.id);
    Ok(Snapshot {
        inventory: result,
        serves_audio: serves_audio.get(),
    })
}

/// Device and stream nodes that only exist when PipeWire handles audio
/// (`Audio/Sink`, `Audio/Source`, `Audio/Duplex`, `Stream/Output/Audio`…).
/// The always-present Dummy/Freewheel drivers have no media class, and a
/// video-only PipeWire (Ubuntu 22.04 with PulseAudio) exposes neither.
fn is_audio_class(class: &str) -> bool {
    class.starts_with("Audio/") || class.ends_with("/Audio")
}

// ---------------------------------------------------------------------------
// pw_buffer.requested without the pipewire crate's `v0_3_49` feature, so the
// bindings also build against PipeWire 0.3.48 headers (Ubuntu 22.04).

/// True when the running libpipewire has `pw_buffer.requested`. pipewire-rs
/// gates it at 0.3.49 while the header documents 0.3.50; we require the later
/// one (0.3.49 just uses the fallback size). The field is appended after
/// `size`, so reading it from an older library would read past the struct:
/// hence the runtime check.
pub(crate) fn has_requested_field() -> bool {
    // SAFETY: pw_get_library_version returns a static NUL-terminated string.
    let version = unsafe {
        let raw = pw::sys::pw_get_library_version();
        if raw.is_null() {
            return false;
        }
        std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned()
    };
    version_at_least(&version, (0, 3, 50))
}

fn version_at_least(version: &str, minimum: (u32, u32, u32)) -> bool {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let found = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    found >= minimum
}

/// Reads `pw_buffer.requested`. Only call when [`has_requested_field`] is true.
///
/// # Safety
/// `buffer` must point to a live `pw_buffer` dequeued from a stream of a
/// libpipewire ≥ 0.3.50.
pub(crate) unsafe fn requested_frames(buffer: *const pw::sys::pw_buffer) -> u64 {
    let offset = std::mem::offset_of!(pw::sys::pw_buffer, size) + std::mem::size_of::<u64>();
    // SAFETY: per the contract, the struct has the u64 `requested` right after
    // `size` (0.3.50 ABI); `size` is 8-byte aligned, so is this field.
    unsafe { buffer.cast::<u8>().add(offset).cast::<u64>().read() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_parses_only_named_json_sources_and_supports_removal() {
        assert_eq!(
            metadata_name(Some(r#"{"name":"alsa_input.analog"}"#)),
            Some("alsa_input.analog".into())
        );
        for value in [
            None,
            Some("56"),
            Some("invalid"),
            Some(r#"{"name":""}"#),
            Some(r#"{"name":56}"#),
        ] {
            assert_eq!(metadata_name(value), None);
        }
    }
    #[test]
    fn disappearing_target_error_is_actionable_without_hiding_other_pipewire_errors() {
        assert!(capture_error("no target node available").contains("Reconectalo"));
        assert!(capture_error("target node not found").contains("Ajustes > Micrófono"));
        assert_eq!(
            capture_error("permission denied"),
            "PipeWire: permission denied"
        );
    }
    #[test]
    fn only_audio_nodes_mark_pipewire_as_the_audio_server() {
        for class in [
            "Audio/Sink",
            "Audio/Source",
            "Audio/Duplex",
            "Audio/Source/Virtual",
            "Stream/Output/Audio",
            "Stream/Input/Audio",
        ] {
            assert!(is_audio_class(class), "{class}");
        }
        for class in ["Video/Source", "Midi/Bridge", "Stream/Output/Video", ""] {
            assert!(!is_audio_class(class), "{class}");
        }
    }
    #[test]
    fn requested_field_needs_a_recent_pipewire() {
        assert!(!version_at_least("0.3.48", (0, 3, 50)));
        assert!(!version_at_least("0.3.49", (0, 3, 50)));
        assert!(version_at_least("0.3.50-1ubuntu1", (0, 3, 50)));
        assert!(version_at_least("1.6.8", (0, 3, 50)));
        assert!(!version_at_least("", (0, 3, 50)));
    }
    #[test]
    #[ignore = "needs a PipeWire session with pipewire-pulse; read-only, opens no stream"]
    fn pipewire_is_the_audio_server_and_pulse_ids_match_node_names() {
        let native = audio_server_nodes(&SOURCES)
            .unwrap()
            .expect("PipeWire serves audio here");
        assert!(serves_audio());
        let pulse = super::super::cpal_backend::inspect_sources().unwrap();
        assert!(!native.devices.is_empty());
        for device in &native.devices {
            assert!(
                pulse.devices.iter().any(|p| p.id == device.id),
                "{} missing from {:?}",
                device.id,
                pulse.devices
            );
        }
        assert!(pulse.devices.iter().all(|p| !p.id.ends_with(".monitor")));
        assert_eq!(pulse.default_source, native.default_source);
    }
    #[test]
    fn requested_offset_matches_the_bindings_layout() {
        // pw_buffer starts with two pointers and `size: u64`; `requested`
        // follows it on every 0.3.50+ ABI regardless of the headers we built on.
        let offset = std::mem::offset_of!(pw::sys::pw_buffer, size);
        assert_eq!(offset, 2 * std::mem::size_of::<*const u8>());
        let mut words = [0u64; 6];
        words[offset / 8 + 1] = 480;
        let requested = unsafe { requested_frames(words.as_ptr().cast()) };
        assert_eq!(requested, 480);
    }
}
