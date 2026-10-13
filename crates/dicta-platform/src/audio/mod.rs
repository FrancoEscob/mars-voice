//! Microphone capture for dictation: PCM signed 16-bit little endian / 16 kHz /
//! mono, in 100 ms packets. The bounded queue holds at most 2 seconds; overflow
//! fails capture instead of silently dropping speech.
//!
//! Backends, chosen per capture on the capture thread:
//! - Linux with PipeWire as the audio server (feature `pipewire-native`, on by
//!   default): native PipeWire stream ([`native`]). PipeWire performs rate
//!   conversion and downmixing.
//! - Everything else (Windows, macOS, PulseAudio-only or ALSA-only Linux):
//!   cpal ([`cpal_backend`]), which downmixes and resamples to 16 kHz itself.
//!
//! Both feed the same [`PcmBuffer`], so packets, levels, `try_send` overflow
//! and the [`AudioStatus`] lifecycle look identical to the controller.
use anyhow::{Context, Result, anyhow, ensure};
use dicta_core::AudioDevice;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::sync::{mpsc, watch};

mod cpal_backend;
#[cfg(all(target_os = "linux", feature = "pipewire-native"))]
pub(crate) mod native;

pub(crate) use cpal_backend::{OPEN_TIMEOUT, default_host, describe, is_pulse, sorted, stable_id};

pub const SAMPLE_RATE: u32 = 16_000;
pub const CHUNK_BYTES: usize = 3_200;
const QUEUE_CHUNKS: usize = 20;
/// Capture fails when no audio at all arrives within this time.
const NO_AUDIO_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, Default)]
pub struct AudioStatus {
    pub recording: bool,
    /// Linear RMS, normalized to 0..=1.
    pub level: f32,
    pub error: Option<String>,
}

pub struct AudioCapture {
    pub handle: AudioHandle,
    pub audio_rx: mpsc::Receiver<Vec<u8>>,
    pub status_rx: watch::Receiver<AudioStatus>,
}

pub struct AudioHandle {
    stopping: Arc<AtomicBool>,
    stop_tx: StopSender,
    thread: Option<thread::JoinHandle<()>>,
}
impl AudioHandle {
    /// Stop capture and send the trailing sub-100ms PCM chunk, then close audio_rx.
    /// This joins the dedicated thread and should run off the UI thread.
    pub fn stop(&mut self) {
        self.finish(true);
    }
    /// Cancel without sending the trailing chunk. Already queued chunks remain;
    /// the provider cancellation signal must also be set by the controller.
    pub fn cancel(&mut self) {
        self.finish(false);
    }
    fn finish(&mut self, flush: bool) {
        if let Some(thread) = self.thread.take() {
            self.stopping.store(true, Ordering::Release);
            self.stop_tx.send(flush);
            let _ = thread.join();
        }
    }
}
impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The backend is only known on the capture thread, so the stop request goes
/// to both channels; the unused receiver is dropped and that send is a no-op.
struct StopSender {
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    native: pipewire::channel::Sender<bool>,
    portable: std::sync::mpsc::Sender<bool>,
}
struct StopReceiver {
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    native: pipewire::channel::Receiver<bool>,
    portable: std::sync::mpsc::Receiver<bool>,
}
impl StopSender {
    fn send(&self, flush: bool) {
        #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
        let _ = self.native.send(flush);
        let _ = self.portable.send(flush);
    }
}
fn stop_channel() -> (StopSender, StopReceiver) {
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    let (native_tx, native_rx) = pipewire::channel::channel();
    let (portable_tx, portable_rx) = std::sync::mpsc::channel();
    (
        StopSender {
            #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
            native: native_tx,
            portable: portable_tx,
        },
        StopReceiver {
            #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
            native: native_rx,
            portable: portable_rx,
        },
    )
}

/// Only this function starts a microphone stream. Initialization errors arrive in
/// status_rx; returning a handle does not yet mean the system granted capture.
pub fn start(device: Option<String>) -> Result<AudioCapture> {
    let (audio_tx, audio_rx) = mpsc::channel(QUEUE_CHUNKS);
    let (status_tx, status_rx) = watch::channel(AudioStatus::default());
    let (stop_tx, stop_rx) = stop_channel();
    let stopping = Arc::new(AtomicBool::new(false));
    let thread_stopping = stopping.clone();
    let thread = thread::Builder::new()
        .name("dicta-pipewire".into())
        .spawn(move || {
            if let Err(error) = capture(
                device,
                audio_tx,
                status_tx.clone(),
                stop_rx,
                thread_stopping,
            ) {
                status_tx.send_replace(AudioStatus {
                    error: Some(format!("No se pudo capturar audio: {error:#}")),
                    ..Default::default()
                });
            }
        })?;
    Ok(AudioCapture {
        handle: AudioHandle {
            stopping,
            stop_tx,
            thread: Some(thread),
        },
        audio_rx,
        status_rx,
    })
}

fn capture(
    device: Option<String>,
    tx: mpsc::Sender<Vec<u8>>,
    status: watch::Sender<AudioStatus>,
    stop: StopReceiver,
    stopping: Arc<AtomicBool>,
) -> Result<()> {
    let StopReceiver {
        #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
            native: native_stop,
        portable: portable_stop,
    } = stop;
    // Registry/metadata inspection creates no stream and opens no microphone.
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    match native::audio_server_nodes(&native::SOURCES)? {
        Some(inventory) => {
            drop(portable_stop);
            return native::capture_loop(&inventory, device, tx, status, native_stop, stopping);
        }
        None => drop(native_stop),
    }
    cpal_backend::capture_loop(device, tx, status, portable_stop, stopping)
}

#[derive(Default)]
struct PcmBuffer {
    pending: Vec<u8>,
}
impl PcmBuffer {
    fn push(&mut self, bytes: &[u8], tx: &mpsc::Sender<Vec<u8>>) -> Result<f32> {
        ensure!(
            bytes.len().is_multiple_of(2),
            "PipeWire entregó una muestra PCM incompleta"
        );
        let level = rms(bytes);
        // Process incrementally so an unexpectedly large server buffer never
        // expands our memory proportional to its size.
        for part in bytes.chunks(CHUNK_BYTES) {
            self.pending.extend_from_slice(part);
            while self.pending.len() >= CHUNK_BYTES {
                let tail = self.pending.split_off(CHUNK_BYTES);
                let packet = std::mem::replace(&mut self.pending, tail);
                send_packet(tx, packet)?;
            }
        }
        Ok(level)
    }
    fn flush(&mut self, tx: &mpsc::Sender<Vec<u8>>) -> Result<()> {
        if !self.pending.is_empty() {
            send_packet(tx, std::mem::take(&mut self.pending))?;
        }
        Ok(())
    }
}
fn send_packet(tx: &mpsc::Sender<Vec<u8>>, packet: Vec<u8>) -> Result<()> {
    tx.try_send(packet).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => anyhow!(
            "La conexión no consume audio a tiempo; se detuvo el micrófono para no perder palabras"
        ),
        mpsc::error::TrySendError::Closed(_) => {
            anyhow!("La sesión de transcripción cerró el canal de audio")
        }
    })
}
fn rms(bytes: &[u8]) -> f32 {
    if bytes.is_empty() {
        return 0.0;
    }
    let sum: f64 = bytes
        .chunks_exact(2)
        .map(|x| {
            let sample = i16::from_le_bytes([x[0], x[1]]) as f64 / 32768.;
            sample * sample
        })
        .sum();
    (sum / (bytes.len() / 2) as f64).sqrt() as f32
}

/// Read-only snapshot. Defaults are WirePlumber's `default` metadata node names;
/// devices are live Audio/Source nodes, never ephemeral global IDs. With cpal,
/// `default_source` is the host's current default input and
/// `configured_default_source` stays empty.
#[derive(Clone, Debug, Default)]
pub struct AudioInventory {
    pub devices: Vec<AudioDevice>,
    pub default_source: Option<String>,
    pub configured_default_source: Option<String>,
}

fn select_source(inventory: &AudioInventory, requested: Option<&str>) -> Result<String> {
    let live = |name: &str| {
        inventory
            .devices
            .iter()
            .find(|device| device.id == name)
            .map(|device| device.id.clone())
    };
    if let Some(requested) = requested.filter(|name| !name.trim().is_empty()) {
        return live(requested).context("El micrófono seleccionado está desconectado o ya no está disponible. Reconectalo o elegí uno disponible en Ajustes > Micrófono");
    }
    // Respect WirePlumber's effective default even when its remembered USB
    // preference is absent. The app itself never chooses replacement hardware.
    if let Some(target) = inventory.default_source.as_deref().and_then(live) {
        return Ok(target);
    }
    if let Some(configured) = inventory.configured_default_source.as_deref() {
        return live(configured).context("El micrófono predeterminado está desconectado o ya no está disponible. Reconectalo o elegí uno disponible en Ajustes > Micrófono");
    }
    if inventory.devices.is_empty() {
        anyhow::bail!(
            "No hay micrófonos disponibles. Conectá un micrófono y actualizá Ajustes > Micrófono"
        );
    }
    anyhow::bail!(
        "No hay un micrófono predeterminado disponible. Elegí uno de los micrófonos disponibles en Ajustes > Micrófono"
    )
}

/// Enumerate sources and effective/configured defaults without audio capture.
/// PipeWire: two server roundtrips include metadata events from proxies bound
/// while the registry is enumerated; returns after at most 3 seconds if
/// PipeWire stalls. cpal: the host's input devices and default input.
pub fn inspect_sources() -> Result<AudioInventory> {
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    if let Some(inventory) = native::audio_server_nodes(&native::SOURCES)? {
        return Ok(inventory);
    }
    cpal_backend::inspect_sources()
}

/// Snapshot of live sources. Does not create a capture stream.
/// IDs are stable strings: PipeWire/PulseAudio node names (identical on
/// PipeWire systems), WASAPI endpoint IDs or CoreAudio device UIDs.
pub fn enumerate_devices() -> Result<Vec<AudioDevice>> {
    Ok(inspect_sources()?.devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inventory() -> AudioInventory {
        AudioInventory {
            devices: vec![AudioDevice {
                id: "alsa_input.analog".into(),
                name: "Micrófono analógico".into(),
            }],
            default_source: None,
            configured_default_source: Some("alsa_input.usb_missing".into()),
        }
    }
    #[test]
    fn missing_configured_default_never_falls_back_to_the_only_live_input() {
        let error = select_source(&inventory(), None).unwrap_err().to_string();
        assert!(error.contains("predeterminado"));
        assert!(error.contains("desconectado"));
        assert!(error.contains("Ajustes > Micrófono"));
    }
    #[test]
    fn effective_live_default_wins_over_absent_remembered_usb_preference() {
        let mut devices = inventory();
        devices.default_source = Some("alsa_input.analog".into());
        assert_eq!(select_source(&devices, None).unwrap(), "alsa_input.analog");
    }
    #[test]
    fn explicit_input_uses_stable_name_and_never_another_default_or_global_id() {
        let mut devices = inventory();
        devices.default_source = Some("alsa_input.analog".into());
        assert_eq!(
            select_source(&devices, Some("alsa_input.analog")).unwrap(),
            "alsa_input.analog"
        );
        for missing in ["alsa_input.usb_missing", "56"] {
            let error = select_source(&devices, Some(missing))
                .unwrap_err()
                .to_string();
            assert!(error.contains("seleccionado"));
            assert!(error.contains("desconectado"));
        }
    }
    #[test]
    fn configured_live_input_can_resolve_before_effective_metadata_arrives() {
        let mut devices = inventory();
        devices.configured_default_source = Some("alsa_input.analog".into());
        assert_eq!(select_source(&devices, None).unwrap(), "alsa_input.analog");
        devices.configured_default_source = None;
        assert!(
            select_source(&devices, None)
                .unwrap_err()
                .to_string()
                .contains("predeterminado")
        );
        devices.devices.clear();
        assert!(
            select_source(&devices, None)
                .unwrap_err()
                .to_string()
                .contains("No hay micrófonos")
        );
    }
    #[test]
    fn chunks_preserve_pcm_and_flush_tail() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut buffer = PcmBuffer::default();
        let pcm: Vec<u8> = (0..3500i16).flat_map(i16::to_le_bytes).collect();
        for part in pcm.chunks(222) {
            buffer.push(part, &tx).unwrap();
        }
        buffer.flush(&tx).unwrap();
        drop(tx);
        let mut output = Vec::new();
        let mut sizes = Vec::new();
        while let Ok(packet) = rx.try_recv() {
            sizes.push(packet.len());
            output.extend(packet);
        }
        assert_eq!(sizes, vec![3200, 3200, 600]);
        assert_eq!(output, pcm);
    }
    #[test]
    fn twenty_ms_rms_updates_preserve_hundred_ms_pcm_packets() {
        let (tx, mut rx) = mpsc::channel(2);
        let mut buffer = PcmBuffer::default();
        let mut original = Vec::new();
        for step in 1..=5i16 {
            let sample = step * 3000;
            let frame: Vec<u8> = (0..320).flat_map(|_| sample.to_le_bytes()).collect();
            original.extend_from_slice(&frame);
            let level = buffer.push(&frame, &tx).unwrap();
            assert!((level - sample as f32 / 32768.0).abs() < 0.0001);
            if step < 5 {
                assert!(matches!(
                    rx.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            }
        }
        let packet = rx.try_recv().unwrap();
        assert_eq!(packet.len(), CHUNK_BYTES);
        assert_eq!(packet, original);
    }
    #[test]
    fn overflow_stops_instead_of_losing_speech() {
        let (tx, _rx) = mpsc::channel(1);
        let mut buffer = PcmBuffer::default();
        buffer.push(&vec![0; CHUNK_BYTES], &tx).unwrap();
        assert!(
            buffer
                .push(&vec![0; CHUNK_BYTES], &tx)
                .unwrap_err()
                .to_string()
                .contains("no perder palabras")
        );
    }
    #[test]
    fn rms_and_alignment() {
        let pcm: Vec<_> = [16384i16, -16384]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        assert!((rms(&pcm) - 0.5).abs() < 0.0001);
        assert_eq!(rms(&[]), 0.0);
        let (tx, _) = mpsc::channel(1);
        assert!(PcmBuffer::default().push(&[0], &tx).is_err());
    }
}
