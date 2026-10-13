//! PipeWire shell of the read-aloud player. Callbacks run on this dedicated
//! mainloop thread (no RT_PROCESS), like capture.
use super::{
    CHOSEN_OUTPUT_LOST, Control, DEFAULT_OUTPUT_LOST, Engine, FALLBACK_FRAMES, IDLE_TICK,
    IDLE_TICKS_STARVED, IDLE_TICKS_STOPPED, PlayerConfig, Signal,
};
use crate::audio::native::{has_requested_field, requested_frames};
use anyhow::{Context, Result, anyhow};
use pipewire::{self as pw, spa};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc,
};

pub(super) fn output_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("no target node available") || lower.contains("target node not found") {
        CHOSEN_OUTPUT_LOST.into()
    } else {
        format!("PipeWire: {message}")
    }
}

/// A dequeued buffer, queued back on drop. Replaces `pw::buffer::Buffer` so
/// `requested` can be read without the crate's `v0_3_49` feature.
struct RawBuffer<'s> {
    stream: &'s pw::stream::Stream,
    raw: std::ptr::NonNull<pw::sys::pw_buffer>,
}
impl<'s> RawBuffer<'s> {
    fn dequeue(stream: &'s pw::stream::Stream) -> Option<Self> {
        // SAFETY: the pointer is owned by the stream until queued back in Drop.
        let raw = unsafe { stream.dequeue_raw_buffer() };
        std::ptr::NonNull::new(raw).map(|raw| Self { stream, raw })
    }
    /// Frames the graph wants (`pw_buffer.requested`), 0 when unknown.
    fn requested(&self, supported: bool) -> u64 {
        if supported {
            // SAFETY: live dequeued buffer; `supported` comes from the runtime
            // library version check.
            unsafe { requested_frames(self.raw.as_ptr()) }
        } else {
            0
        }
    }
    fn datas_mut(&mut self) -> &mut [spa::buffer::Data] {
        // SAFETY: same layout and checks as pw::buffer::Buffer::datas_mut;
        // spa::buffer::Data is a transparent wrapper of spa_data.
        unsafe {
            let buffer = self.raw.as_ref().buffer;
            if buffer.is_null() || (*buffer).n_datas == 0 || (*buffer).datas.is_null() {
                return &mut [];
            }
            std::slice::from_raw_parts_mut(
                (*buffer).datas.cast::<spa::buffer::Data>(),
                (*buffer).n_datas as usize,
            )
        }
    }
}
impl Drop for RawBuffer<'_> {
    fn drop(&mut self) {
        // SAFETY: obtained from this stream's dequeue_raw_buffer, queued once.
        unsafe { self.stream.queue_raw_buffer(self.raw.as_ptr()) };
    }
}

pub(super) fn playback_loop(
    config: PlayerConfig,
    control: Control,
    signals: pw::channel::Receiver<Signal>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context
        .connect_rc(None)
        .context("No se pudo conectar con PipeWire; revisá que esté en ejecución")?;
    let error = Rc::new(RefCell::new(None::<String>));
    let active = Rc::new(Cell::new(false));
    let quiet = Rc::new(Cell::new(true));
    let idle_ticks = Rc::new(Cell::new(0u32));
    let format_ok = Rc::new(Cell::new(false));
    let connected = Rc::new(Cell::new(false));
    let sample_rate = config.sample_rate;
    let requested_supported = has_requested_field();
    let lost = if config.device.is_some() {
        CHOSEN_OUTPUT_LOST
    } else {
        DEFAULT_OUTPUT_LOST
    };
    let _core_listener = core
        .add_listener_local()
        .error({
            let mainloop = mainloop.clone();
            let error = error.clone();
            move |_, _, _, message| {
                *error.borrow_mut() = Some(output_error(message));
                mainloop.quit();
            }
        })
        .register();
    let mut props = pw::properties::properties! {
        "media.type" => "Audio", "media.category" => "Playback", "media.role" => "Accessibility",
        "application.name" => "Dicta", "node.name" => "dicta-reader",
        "node.description" => "Dicta lectura",
    };
    // ≈20 ms buffers: responsive seek/pause without waking needlessly often.
    props.insert(
        "node.latency",
        format!("{}/{}", (sample_rate / 50).max(64), sample_rate),
    );
    if let Some(device) = config.device {
        // A chosen output never silently moves to the speakers.
        props.insert("target.object", device);
        props.insert("node.dont-fallback", "true");
        props.insert("node.dont-reconnect", "true");
    }
    let stream = pw::stream::StreamRc::new(core.clone(), "Dicta lectura", props)?;
    let engine = Engine::new(control.clone(), sample_rate);
    let _listener = stream
        .add_local_listener_with_user_data(engine)
        .state_changed({
            let mainloop = mainloop.clone();
            let error = error.clone();
            let connected = connected.clone();
            move |_, _, _, state| match state {
                pw::stream::StreamState::Error(message) => {
                    *error.borrow_mut() = Some(output_error(&message));
                    mainloop.quit();
                }
                pw::stream::StreamState::Unconnected if connected.get() => {
                    *error.borrow_mut() = Some(lost.into());
                    mainloop.quit();
                }
                pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming => {
                    connected.set(true);
                }
                _ => {}
            }
        })
        .param_changed({
            let mainloop = mainloop.clone();
            let error = error.clone();
            let format_ok = format_ok.clone();
            move |_, _, id, param| {
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                format_ok.set(false);
                let Some(param) = param else {
                    return;
                };
                let mut format = spa::param::audio::AudioInfoRaw::new();
                let valid = format.parse(param).is_ok()
                    && format.format() == spa::param::audio::AudioFormat::F32LE
                    && format.rate() == sample_rate
                    && format.channels() == 1;
                if valid {
                    format_ok.set(true);
                } else {
                    *error.borrow_mut() = Some(format!(
                        "La salida de audio no aceptó audio mono a {sample_rate} Hz"
                    ));
                    mainloop.quit();
                }
            }
        })
        .process({
            let quiet = quiet.clone();
            let format_ok = format_ok.clone();
            move |stream, engine| {
                if !format_ok.get() {
                    return;
                }
                let Some(mut buffer) = RawBuffer::dequeue(stream) else {
                    return;
                };
                let requested =
                    usize::try_from(buffer.requested(requested_supported)).unwrap_or(usize::MAX);
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let written = match data.data() {
                    Some(bytes) => {
                        let capacity = bytes.len() / 4;
                        let frames = if requested > 0 {
                            requested.min(capacity)
                        } else {
                            capacity.min(FALLBACK_FRAMES)
                        };
                        engine.render_bytes(&mut bytes[..frames * 4]);
                        frames * 4
                    }
                    None => 0,
                };
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = 4;
                *chunk.size_mut() = u32::try_from(written).unwrap_or(0);
                quiet.set(engine.quiet());
            }
        })
        .register()?;
    let mut format = spa::param::audio::AudioInfoRaw::new();
    format.set_format(spa::param::audio::AudioFormat::F32LE);
    format.set_rate(sample_rate);
    format.set_channels(1);
    let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    format.set_position(position);
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
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::INACTIVE,
        &mut params,
    )?;
    let _signals = signals.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        let stream = stream.clone();
        let active = active.clone();
        let idle_ticks = idle_ticks.clone();
        let control = control.clone();
        move |signal| match signal {
            Signal::Quit => mainloop.quit(),
            Signal::Wake => {
                idle_ticks.set(0);
                if !active.get() && control.lock().playing && stream.set_active(true).is_ok() {
                    active.set(true);
                }
            }
        }
    });
    // Release the device while nothing is audible; play/append wake it up.
    let idle = mainloop.loop_().add_timer({
        let stream = stream.clone();
        let active = active.clone();
        let quiet = quiet.clone();
        let idle_ticks = idle_ticks.clone();
        let control = control.clone();
        move |_| {
            if !active.get() || !quiet.get() {
                idle_ticks.set(0);
                return;
            }
            let waiting = {
                let s = control.lock();
                s.playing && !s.ended
            };
            let limit = if waiting {
                IDLE_TICKS_STARVED
            } else {
                IDLE_TICKS_STOPPED
            };
            idle_ticks.set(idle_ticks.get() + 1);
            if idle_ticks.get() >= limit {
                idle_ticks.set(0);
                if stream.set_active(false).is_ok() {
                    active.set(false);
                }
            }
        }
    });
    idle.update_timer(Some(IDLE_TICK), Some(IDLE_TICK))
        .into_result()?;
    let _ = ready.send(Ok(()));
    mainloop.run();
    // Read before disconnecting: our own disconnect reports Unconnected.
    let failure = error.borrow_mut().take();
    let _ = stream.disconnect();
    match failure {
        Some(error) => Err(anyhow!(error)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a PipeWire session with pipewire-pulse; read-only, opens no stream"]
    fn pulse_output_ids_match_pipewire_sink_names() {
        let native = crate::audio::native::audio_server_nodes(&crate::audio::native::SINKS)
            .unwrap()
            .expect("PipeWire serves audio here");
        let pulse = super::super::cpal_backend::enumerate_outputs().unwrap();
        assert!(!native.devices.is_empty());
        for device in &native.devices {
            assert!(
                pulse.iter().any(|p| p.id == device.id),
                "{} missing from {pulse:?}",
                device.id
            );
        }
    }
    #[test]
    fn lost_targets_map_to_the_settings_hint() {
        assert!(output_error("no target node available").contains("Ajustes > Lectura"));
        assert_eq!(output_error("busy"), "PipeWire: busy");
    }
}
