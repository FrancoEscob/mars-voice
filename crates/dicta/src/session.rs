//! One dictation, including audio lifetime, provider cancellation and final text.
//! This module never touches GPUI, clipboard, input injection or token files.
use dicta_core::{
    AppConfig, Correction, SessionArtifacts,
    text::{apply_rules, protected_content_matches},
    transcript::Transcript,
};
use dicta_platform::audio::{self, AudioStatus};
use dicta_provider::{
    CancellationToken, Credentials, Endpoints, PolishResult, Secret, StreamEvent, Timeouts,
    XaiProvider,
};
use std::{
    future::{Future, pending},
    path::PathBuf,
    pin::Pin,
    sync::mpsc as std_mpsc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

const SESSION_LIMIT: Duration = Duration::from_secs(30 * 60);
const AUDIO_CLEANUP_LIMIT: Duration = Duration::from_secs(5);
type Outcome<T> = Result<T, String>;
type Job<T> = Pin<Box<dyn Future<Output = Outcome<T>> + Send>>;

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Started,
    Partial {
        confirmed: String,
        partial: String,
    },
    Level(f32),
    Telemetry(SessionArtifacts),
    Polishing,
    Finished {
        original: String,
        text: String,
        warning: Option<String>,
        artifacts: SessionArtifacts,
    },
    Failed {
        error: String,
        artifacts: SessionArtifacts,
    },
}

/// Cancellation emits no final/failure event. The controller resets its own UI
/// when cancelling, and should ignore events from an obsolete session generation.
/// Closing stop's sender is treated as a normal Stop, not a recording forever.
pub async fn run(
    config: AppConfig,
    data_dir: PathBuf,
    corrections: Vec<Correction>,
    credentials: Credentials,
    allow_env: bool,
    stop: watch::Receiver<bool>,
    cancel: CancellationToken,
    tx: mpsc::Sender<SessionEvent>,
) {
    let provider = XaiProvider::with_endpoints(
        Endpoints::default(),
        Timeouts {
            session: SESSION_LIMIT,
            ..Default::default()
        },
    );
    let backend = match provider {
        Ok(provider) => LiveBackend {
            credentials,
            provider,
        },
        Err(error) => {
            publish_failure(&tx, &cancel, error.to_string(), SessionArtifacts::default()).await;
            return;
        }
    };
    run_with(
        backend,
        config,
        data_dir,
        corrections,
        allow_env,
        stop,
        cancel,
        tx,
    )
    .await;
}

// Injectable I/O keeps the state-machine tests entirely on local synthetic data.
trait Backend: Send + Sync {
    fn credential(
        &self,
        config: AppConfig,
        allow_env: bool,
        cancel: CancellationToken,
    ) -> Job<Secret>;
    fn stream(
        &self,
        config: AppConfig,
        bearer: Secret,
        audio: mpsc::Receiver<Vec<u8>>,
        events: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Job<()>;
    fn capture(&self, device: Option<String>) -> Job<Capture>;
    fn polish(
        &self,
        config: AppConfig,
        bearer: Secret,
        text: String,
        cancel: CancellationToken,
    ) -> Job<PolishResult>;
}
struct LiveBackend {
    credentials: Credentials,
    provider: XaiProvider,
}
impl Backend for LiveBackend {
    fn credential(
        &self,
        config: AppConfig,
        allow_env: bool,
        cancel: CancellationToken,
    ) -> Job<Secret> {
        let credentials = self.credentials.clone();
        Box::pin(async move {
            credentials
                .bearer(&config, allow_env, &cancel)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn stream(
        &self,
        config: AppConfig,
        bearer: Secret,
        audio: mpsc::Receiver<Vec<u8>>,
        events: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Job<()> {
        let provider = self.provider.clone();
        Box::pin(async move {
            provider
                .stream(&config, &bearer, audio, events, cancel)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn capture(&self, device: Option<String>) -> Job<Capture> {
        Box::pin(open_capture(device))
    }
    fn polish(
        &self,
        config: AppConfig,
        bearer: Secret,
        text: String,
        cancel: CancellationToken,
    ) -> Job<PolishResult> {
        let provider = self.provider.clone();
        Box::pin(async move {
            provider
                .polish_with_usage(&config, &bearer, &text, &cancel)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

struct Capture {
    control: CaptureControl,
    audio: mpsc::Receiver<Vec<u8>>,
    status: watch::Receiver<AudioStatus>,
    finished: oneshot::Receiver<AudioStatus>,
}
struct CaptureControl {
    command: Option<std_mpsc::Sender<bool>>,
}
impl CaptureControl {
    fn request_stop(&mut self) {
        if let Some(command) = self.command.take() {
            let _ = command.send(true);
        }
    }
}
impl Drop for CaptureControl {
    fn drop(&mut self) {
        // Dropping the last sender without true also cancels the owner thread.
        if let Some(command) = self.command.take() {
            let _ = command.send(false);
        }
    }
}
async fn open_capture(device: Option<String>) -> Outcome<Capture> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (command_tx, command_rx) = std_mpsc::channel();
    let (finished_tx, finished_rx) = oneshot::channel();
    // AudioHandle can stay !Send. Both creation and destruction happen here;
    // PipeWire itself has its own dedicated main-loop thread.
    std::thread::Builder::new()
        .name("dicta-audio-owner".into())
        .spawn(move || {
            match audio::start(device) {
                Ok(capture) => {
                    let audio::AudioCapture {
                        mut handle,
                        audio_rx,
                        status_rx,
                    } = capture;
                    let final_status = status_rx.clone();
                    if ready_tx.send(Ok((audio_rx, status_rx))).is_err() {
                        handle.cancel();
                        return;
                    }
                    if command_rx.recv().unwrap_or(false) {
                        handle.stop();
                    } else {
                        handle.cancel();
                    }
                    // AudioHandle::stop joins PipeWire. Initialization/flush errors can
                    // arrive after the last PCM channel closes; check after the join.
                    let status = final_status.borrow().clone();
                    let _ = finished_tx.send(status);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("No se pudo iniciar el micrófono: {error}")));
                }
            }
        })
        .map_err(|e| format!("No se pudo crear el hilo de audio: {e}"))?;
    let control = CaptureControl {
        command: Some(command_tx),
    };
    let (audio, status) = ready_rx
        .await
        .map_err(|_| "El hilo de audio no respondió".to_owned())??;
    Ok(Capture {
        control,
        audio,
        status,
        finished: finished_rx,
    })
}

async fn run_with<B: Backend>(
    backend: B,
    config: AppConfig,
    data_dir: PathBuf,
    corrections: Vec<Correction>,
    allow_env: bool,
    mut stop: watch::Receiver<bool>,
    cancel: CancellationToken,
    tx: mpsc::Sender<SessionEvent>,
) {
    let deadline = Instant::now() + SESSION_LIMIT;
    let bearer = tokio::select! { biased;
        _ = cancel.cancelled() => return,
        result = backend.credential(config.clone(), allow_env, cancel.child_token()) => match result {
            Ok(secret) => secret,
            Err(error) => { publish_failure(&tx, &cancel, error, SessionArtifacts::default()).await; return; }
        }
    };
    // The microphone is still closed, including throughout OAuth/keyring waits.
    let stream_cancel = cancel.child_token();
    let (audio_tx, audio_rx) = mpsc::channel(20);
    let mut audio_tx = Some(audio_tx);
    let (wire_tx, mut wire_rx) = mpsc::channel(32);
    let mut stream = tokio::spawn(backend.stream(
        config.clone(),
        bearer.clone(),
        audio_rx,
        wire_tx,
        stream_cancel.clone(),
    ));
    let mut control: Option<CaptureControl> = None;
    let mut audio: Option<mpsc::Receiver<Vec<u8>>> = None;
    let mut status: Option<watch::Receiver<AudioStatus>> = None;
    let mut capture_finished: Option<oneshot::Receiver<AudioStatus>> = None;
    let mut capture_done = false;
    let mut audio_drained = false;
    let mut wire_open = true;
    let mut started = false;
    let mut stopping = *stop.borrow_and_update();
    let mut stop_open = true;
    let mut transcript = Transcript::default();
    let mut final_text: Option<String> = None;
    let mut archive: Option<crate::recording::Recording> = None;
    let mut archive_warning: Option<String> = None;
    let mut artifacts = SessionArtifacts::default();
    let mut captured_bytes = 0u64;
    let mut sent_bytes = 0u64;
    // Default is cancellation. Only the successful stream path assigns Ok.
    let mut result: Outcome<()> = loop {
        tokio::select! { biased;
            _ = cancel.cancelled() => break Err(String::new()),
            _ = tokio::time::sleep_until(deadline) => break Err("El dictado alcanzó el límite de 30 minutos. Se detuvo el micrófono; el texto recibido sigue visible".into()),
            changed = stop.changed(), if stop_open => {
                if changed.is_err() { stop_open=false; stopping=true; }
                else { stopping |= *stop.borrow_and_update(); }
                if stopping {
                    if let Some(control)=control.as_mut() {control.request_stop();}
                    else if started { audio_tx.take(); }
                }
            }
            final_status = wait_capture(&mut capture_finished), if capture_finished.is_some() => {
                capture_finished = None;
                capture_done = true;
                match final_status {
                    Ok(final_status) => {
                        if let Some(error)=final_status.error { break Err(error); }
                        if !stopping { break Err("El micrófono se detuvo inesperadamente".into()); }
                    }
                    Err(error) => break Err(error),
                }
                if audio_drained { audio_tx.take(); }
            }
            status_update = wait_status(&mut status), if status.is_some() => {
                match status_update {
                    Some(update) => {
                        if let Some(error)=update.error { break Err(error); }
                        if update.recording && !publish_level(&tx, update.level) {break Err(String::new());}
                    }
                    None => status=None,
                }
            }
            packet = next_audio(&mut audio), if audio.is_some() => {
                match packet {
                    Some(bytes) => if let Some(sender)=audio_tx.as_ref() {
                        captured_bytes += bytes.len() as u64;
                        artifacts.captured_ms = captured_bytes / 32;
                        if let Some(archive) = archive.as_mut() {
                            if !archive.push(&bytes) { archive_warning=Some(archive_failure()); }
                        }
                        let packet_bytes=bytes.len() as u64;
                        // Keep this PCM packet until a slot is reserved. Visual
                        // levels and Stop must remain responsive while the
                        // provider applies backpressure. Stop requests capture
                        // flush, then the existing drain/final path continues.
                        let send_deadline = Instant::now() + Duration::from_secs(2);
                        let sent: Outcome<()> = loop { tokio::select! {biased;
                            _=cancel.cancelled()=>break Err(String::new()),
                            _=tokio::time::sleep_until(send_deadline)=>break Err("La transcripción dejó de consumir audio; se detuvo el micrófono".into()),
                            changed=stop.changed(), if stop_open => {
                                if changed.is_err() {stop_open=false;stopping=true;}
                                else {stopping |= *stop.borrow_and_update();}
                                if stopping {if let Some(control)=control.as_mut() {control.request_stop();}}
                            }
                            update=wait_status(&mut status), if status.is_some() => {
                                match update {
                                    Some(update)=> {
                                        if let Some(error)=update.error {break Err(error);}
                                        if update.recording && !publish_level(&tx,update.level) {break Err(String::new());}
                                    }
                                    None=>status=None,
                                }
                            }
                            permit=sender.reserve()=>match permit {
                                Ok(permit)=>{permit.send(bytes);break Ok(());}
                                Err(_)=>break Err("La transcripción dejó de consumir audio; se detuvo el micrófono".into()),
                            },
                        }};
                        if let Err(error)=sent {break Err(error);}
                        sent_bytes += packet_bytes;
                        artifacts.sent_ms = sent_bytes / 32;
                        // Best-effort UI telemetry must never delay PCM forwarding.
                        let _=tx.try_send(SessionEvent::Telemetry(artifacts.clone()));
                    },
                    None => { audio=None; audio_drained=true; if capture_done {audio_tx.take();} }
                }
            }
            event = wire_rx.recv(), if wire_open => {
                match event {
                    Some(StreamEvent::Started) => {
                        started=true;
                        // Stop may have arrived while the provider was opening.
                        stopping |= *stop.borrow_and_update();
                        if stopping { audio_tx.take(); }
                        else {
                            let capture=tokio::select! {biased;
                                _=cancel.cancelled()=>break Err(String::new()),
                                capture=backend.capture(config.microphone.clone())=>capture,
                            };
                            match capture {
                                Ok(capture)=> {
                                    if config.saves_audio() {
                                        match crate::recording::Recording::start(data_dir.clone()) {
                                            Ok(recording)=>archive=Some(recording),
                                            Err(_)=>archive_warning=Some(archive_failure()),
                                        }
                                    }
                                    control=Some(capture.control);audio=Some(capture.audio);status=Some(capture.status);capture_finished=Some(capture.finished);
                                    if !emit(&tx,&cancel,SessionEvent::Started).await {break Err(String::new());}
                                },
                                Err(error)=>break Err(error),
                            }
                        }
                    }
                    Some(StreamEvent::Partial {text,is_final,speech_final,start,duration})=> {
                        transcript.update(&text,is_final,speech_final,start,duration);
                        if !emit(&tx,&cancel,SessionEvent::Partial {confirmed:transcript.confirmed(),partial:transcript.partial.clone()}).await {break Err(String::new());}
                    }
                    Some(StreamEvent::Final {text,..})=>final_text=Some(transcript.finish(&text)),
                    None=>wire_open=false,
                }
            }
            result = &mut stream => {
                let result = result.unwrap_or_else(|_|Err("El proceso de transcripción terminó inesperadamente".into()));
                match result {
                    Ok(()) if final_text.is_some() && audio_tx.is_none()=>break Ok(()),
                    Ok(())=>break Err("La transcripción terminó sin confirmar el cierre del audio y el texto final".into()),
                    Err(error)=>break Err(error),
                }
            }
        }
    };
    stream_cancel.cancel();
    stream.abort();
    drop(stream);
    // Cleanup precedes either Failed or Finished. All blocking joins stay in the
    // owner thread. Dropping control requests Cancel unless Stop was already sent.
    drop(control);
    drop(audio_tx);
    drop(audio);
    if let Some(done) = capture_finished.take() {
        match tokio::time::timeout(AUDIO_CLEANUP_LIMIT, done).await {
            Ok(Ok(status)) if !cancel.is_cancelled() => {
                if let Some(error) = status.error {
                    result = Err(error);
                }
            }
            Err(_) if !cancel.is_cancelled() => {
                result = Err("El hilo de audio no confirmó el cierre del micrófono".into());
            }
            _ => {}
        }
    }
    if cancel.is_cancelled() {
        return;
    }
    if let Err(error) = result {
        if !error.is_empty() {
            let saved = finish_archive(
                archive.take(),
                &cancel,
                &mut artifacts,
                &mut archive_warning,
            )
            .await;
            let error = match archive_warning {
                Some(warning) => format!("{error}. {warning}"),
                None => error,
            };
            if publish_failure(&tx, &cancel, error, artifacts).await {
                if let Some(saved) = saved {
                    saved.commit();
                }
            }
        }
        return;
    }
    let original = final_text.unwrap_or_default();
    let local = apply_rules(&original, &corrections);
    let (text, mut warning) = if config.polish && !local.is_empty() {
        if !emit(&tx, &cancel, SessionEvent::Polishing).await {
            return;
        }
        let polished = tokio::select! {biased;
            _=cancel.cancelled()=>return,
            result=backend.polish(config,bearer,local.clone(),cancel.child_token())=>result,
        };
        let polished = polished.map(|result| {
            artifacts.input_tokens = result.input_tokens;
            artifacts.output_tokens = result.output_tokens;
            result.text
        });
        choose_polished(local, polished)
    } else {
        (local, None)
    };
    let saved = finish_archive(
        archive.take(),
        &cancel,
        &mut artifacts,
        &mut archive_warning,
    )
    .await;
    if let Some(archive_warning) = archive_warning {
        warning = Some(match warning {
            Some(other) => format!("{other}. {archive_warning}"),
            None => archive_warning,
        });
    }
    let delivered = emit(
        &tx,
        &cancel,
        SessionEvent::Finished {
            original,
            text,
            warning,
            artifacts,
        },
    )
    .await;
    if delivered {
        if let Some(saved) = saved {
            saved.commit();
        }
    }
}

fn choose_polished(local: String, result: Outcome<String>) -> (String, Option<String>) {
    match result {
        Ok(proposed) if !proposed.trim().is_empty() && protected_content_matches(&local,&proposed)=>(proposed,None),
        Ok(_)=>(local,Some("Se conservó el texto con reglas locales porque Grok cambió cifras o negaciones, o devolvió texto vacío".into())),
        Err(error)=>(local,Some(format!("Se conservó el texto con reglas locales: {error}"))),
    }
}
async fn next_audio(audio: &mut Option<mpsc::Receiver<Vec<u8>>>) -> Option<Vec<u8>> {
    match audio {
        Some(rx) => rx.recv().await,
        None => pending().await,
    }
}
async fn wait_status(status: &mut Option<watch::Receiver<AudioStatus>>) -> Option<AudioStatus> {
    match status {
        Some(rx) => match rx.changed().await {
            Ok(()) => Some(rx.borrow_and_update().clone()),
            Err(_) => None,
        },
        None => pending().await,
    }
}
async fn wait_capture(done: &mut Option<oneshot::Receiver<AudioStatus>>) -> Outcome<AudioStatus> {
    match done {
        Some(rx) => rx
            .await
            .map_err(|_| "El hilo de audio terminó sin confirmar su estado".into()),
        None => pending().await,
    }
}
async fn emit(
    tx: &mpsc::Sender<SessionEvent>,
    cancel: &CancellationToken,
    event: SessionEvent,
) -> bool {
    tokio::select! {biased;_=cancel.cancelled()=>false,result=tokio::time::timeout(Duration::from_secs(2),tx.send(event))=>matches!(result,Ok(Ok(())))}
}
/// Levels are disposable visual telemetry. Never let a full UI queue delay PCM
/// or capture shutdown; terminal and transcript events still use reliable emit.
fn publish_level(tx: &mpsc::Sender<SessionEvent>, level: f32) -> bool {
    !matches!(
        tx.try_send(SessionEvent::Level(level)),
        Err(mpsc::error::TrySendError::Closed(_))
    )
}
async fn publish_failure(
    tx: &mpsc::Sender<SessionEvent>,
    cancel: &CancellationToken,
    error: String,
    artifacts: SessionArtifacts,
) -> bool {
    emit(tx, cancel, SessionEvent::Failed { error, artifacts }).await
}
fn archive_failure() -> String {
    "No se pudo guardar el audio; la transcripción continúa disponible".into()
}
async fn finish_archive(
    archive: Option<crate::recording::Recording>,
    cancel: &CancellationToken,
    artifacts: &mut SessionArtifacts,
    warning: &mut Option<String>,
) -> Option<crate::recording::SavedAudio> {
    if let Some(archive) = archive {
        let result = tokio::select! { biased;
            _=cancel.cancelled()=>return None,
            result=tokio::time::timeout(AUDIO_CLEANUP_LIMIT, archive.finish())=>result,
        };
        match result {
            Ok(Ok(Some(saved))) => {
                if cancel.is_cancelled() {
                    return None;
                }
                artifacts.audio_filename = Some(saved.filename.clone());
                artifacts.audio_bytes = saved.bytes;
                return Some(saved);
            }
            Ok(Ok(None)) => {}
            Ok(Err(_)) | Err(_) => *warning = Some(archive_failure()),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    };

    #[derive(Clone, Default)]
    struct MeterBackend {
        blocked_provider: bool,
        resume_provider_after: Option<Duration>,
        produced_bytes: Arc<AtomicU64>,
        sent_bytes: Arc<AtomicU64>,
        stopped: Arc<Mutex<Option<(bool, std::time::Instant)>>>,
    }
    impl Backend for MeterBackend {
        fn credential(&self, _: AppConfig, _: bool, _: CancellationToken) -> Job<Secret> {
            Box::pin(async { Ok(Secret::new("synthetic")) })
        }
        fn stream(
            &self,
            _: AppConfig,
            _: Secret,
            mut audio: mpsc::Receiver<Vec<u8>>,
            events: mpsc::Sender<StreamEvent>,
            cancel: CancellationToken,
        ) -> Job<()> {
            let this = self.clone();
            Box::pin(async move {
                events.send(StreamEvent::Started).await.unwrap();
                if this.blocked_provider {
                    if let Some(delay) = this.resume_provider_after {
                        tokio::select! {biased;_=cancel.cancelled()=>return Err("cancelled".into()),_=tokio::time::sleep(delay)=>{}}
                    } else {
                        cancel.cancelled().await;
                        return Err("cancelled".into());
                    }
                }
                loop {
                    let chunk = tokio::select! {biased;_=cancel.cancelled()=>return Err("cancelled".into()),packet=audio.recv()=>packet};
                    let Some(chunk) = chunk else { break };
                    this.sent_bytes
                        .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
                events
                    .send(StreamEvent::Final {
                        text: "dato sintético".into(),
                        duration: None,
                    })
                    .await
                    .unwrap();
                Ok(())
            })
        }
        fn capture(&self, _: Option<String>) -> Job<Capture> {
            let this = self.clone();
            Box::pin(async move {
                let (audio_tx, audio) = mpsc::channel(20);
                let (status_tx, status) = watch::channel(AudioStatus::default());
                let (command_tx, command_rx) = std_mpsc::channel();
                let (finished_tx, finished) = oneshot::channel();
                std::thread::spawn(move || {
                    // A synthetic 2.1s backlog fills the provider's 20-packet
                    // queue immediately; subsequent PCM remains at 100ms.
                    if this.blocked_provider {
                        for _ in 0..21 {
                            if audio_tx.blocking_send(vec![0; 3200]).is_err() {
                                break;
                            }
                            this.produced_bytes.fetch_add(3200, Ordering::Relaxed);
                        }
                    }
                    let mut tick = 0;
                    let flush = loop {
                        match command_rx.recv_timeout(Duration::from_millis(20)) {
                            Ok(flush) => break flush,
                            Err(std_mpsc::RecvTimeoutError::Disconnected) => break false,
                            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        tick += 1;
                        status_tx.send_replace(AudioStatus {
                            recording: true,
                            level: (tick % 10) as f32 / 10.0,
                            error: None,
                        });
                        if tick % 5 == 0 {
                            if audio_tx.try_send(vec![0; 3200]).is_err() {
                                break false;
                            }
                            this.produced_bytes.fetch_add(3200, Ordering::Relaxed);
                        }
                    };
                    *this.stopped.lock().unwrap() = Some((flush, std::time::Instant::now()));
                    if flush && audio_tx.try_send(vec![0; 640]).is_ok() {
                        this.produced_bytes.fetch_add(640, Ordering::Relaxed);
                    }
                    drop(audio_tx);
                    let _ = finished_tx.send(AudioStatus::default());
                });
                Ok(Capture {
                    control: CaptureControl {
                        command: Some(command_tx),
                    },
                    audio,
                    status,
                    finished,
                })
            })
        }
        fn polish(
            &self,
            _: AppConfig,
            _: Secret,
            _: String,
            _: CancellationToken,
        ) -> Job<PolishResult> {
            Box::pin(async { panic!("synthetic meter must not polish") })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn meter_backpressure_never_blocks_pcm_or_capture_stop() {
        let mut failures = Vec::new();
        for (label, blocked_ui, blocked_provider, cancel_only) in [
            ("fast", false, false, false),
            ("ui_saturated", true, false, false),
            ("provider_saturated", false, true, false),
            ("ui_saturated_cancel", true, false, true),
            ("provider_saturated_cancel", false, true, true),
        ] {
            let backend = MeterBackend {
                blocked_provider,
                ..Default::default()
            };
            let (stop_tx, stop) = watch::channel(false);
            let cancel = CancellationToken::new();
            let (tx, mut rx) = mpsc::channel(if blocked_ui { 1 } else { 64 });
            let task = tokio::spawn(run_with(
                backend.clone(),
                AppConfig {
                    polish: false,
                    ..Default::default()
                },
                PathBuf::new(),
                vec![],
                false,
                stop,
                cancel.clone(),
                tx,
            ));
            assert!(matches!(rx.recv().await, Some(SessionEvent::Started)));
            let levels = Arc::new(Mutex::new(Vec::new()));
            let terminals = Arc::new(AtomicU64::new(0));
            let observer = if !blocked_ui {
                let times = levels.clone();
                let terminals = terminals.clone();
                Some(tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        match event {
                            SessionEvent::Level(_) => {
                                times.lock().unwrap().push(std::time::Instant::now())
                            }
                            SessionEvent::Finished { .. } | SessionEvent::Failed { .. } => {
                                terminals.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                }))
            } else {
                None
            };
            tokio::time::sleep(Duration::from_millis(600)).await;
            let bytes = backend.sent_bytes.load(Ordering::Relaxed);
            let before = std::time::Instant::now();
            if cancel_only {
                cancel.cancel();
            } else {
                stop_tx.send_replace(true);
            }
            let stopped = tokio::time::timeout(Duration::from_millis(2200), async {
                loop {
                    if let Some(stopped) = *backend.stopped.lock().unwrap() {
                        break stopped;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await;
            let stop_ms = stopped
                .as_ref()
                .map(|(_, at)| at.saturating_duration_since(before).as_secs_f64() * 1000.0)
                .unwrap_or(f64::INFINITY);
            let cancel_at = if cancel_only {
                before
            } else {
                std::time::Instant::now()
            };
            cancel.cancel();
            tokio::time::timeout(Duration::from_millis(500), task)
                .await
                .unwrap()
                .unwrap();
            let cancel_ms = cancel_at.elapsed().as_secs_f64() * 1000.0;
            if let Some(observer) = observer {
                observer.await.unwrap();
            }
            let times = levels.lock().unwrap();
            let mut intervals: Vec<f64> = times
                .windows(2)
                .map(|w| w[1].duration_since(w[0]).as_secs_f64() * 1000.0)
                .collect();
            intervals.sort_by(f64::total_cmp);
            let p95 = intervals
                .get(intervals.len().saturating_sub(1) * 95 / 100)
                .copied()
                .unwrap_or(0.0);
            println!(
                "PERF meter {label} levels={} interval_p95_ms={p95:.2} forwarded_bytes={bytes} stop_ms={stop_ms:.2} cancel_join_ms={cancel_ms:.2}",
                times.len()
            );
            if !blocked_provider && bytes < 4 * 3200 {
                failures.push(format!(
                    "{label}: visual levels blocked PCM ({bytes} bytes)"
                ));
            }
            if stop_ms >= 150.0 {
                failures.push(format!("{label}: Stop delayed capture {stop_ms:.2}ms"));
            }
            if !blocked_ui && times.len() < 20 {
                failures.push(format!("{label}: meter stalled ({} levels)", times.len()));
            }
            if stopped.is_ok() && stopped.unwrap().0 == cancel_only {
                failures.push(format!("{label}: Stop must flush and Cancel must discard"));
            }
            if (cancel_only || blocked_provider) && terminals.load(Ordering::Relaxed) != 0 {
                failures.push(format!(
                    "{label}: cancelled session emitted a terminal event"
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_during_pcm_backpressure_flushes_and_drains_before_final() {
        let backend = MeterBackend {
            blocked_provider: true,
            resume_provider_after: Some(Duration::from_millis(600)),
            ..Default::default()
        };
        let (stop_tx, stop) = watch::channel(false);
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::channel(64);
        let task = tokio::spawn(run_with(
            backend.clone(),
            AppConfig {
                polish: false,
                ..Default::default()
            },
            PathBuf::new(),
            vec![],
            false,
            stop,
            cancel.clone(),
            tx,
        ));
        assert!(matches!(rx.recv().await, Some(SessionEvent::Started)));
        tokio::time::sleep(Duration::from_millis(200)).await;
        stop_tx.send_replace(true);
        let stopped = tokio::time::timeout(Duration::from_millis(150), async {
            loop {
                if let Some((flush, _)) = *backend.stopped.lock().unwrap() {
                    break flush;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        if stopped.is_err() {
            cancel.cancel();
            task.await.unwrap();
            panic!("Stop must reach capture while PCM is backpressured");
        }
        assert!(stopped.unwrap(), "Stop must flush rather than cancel");
        let artifacts = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = rx.recv().await {
                match event {
                    SessionEvent::Finished { artifacts, .. } => return artifacts,
                    SessionEvent::Failed { error, .. } => panic!("unexpected failure: {error}"),
                    _ => {}
                }
            }
            panic!("expected final after drain")
        })
        .await
        .unwrap();
        task.await.unwrap();
        let produced = backend.produced_bytes.load(Ordering::Relaxed);
        assert!(
            produced >= 21 * 3200 + 640,
            "synthetic backlog and trailing flush must be present"
        );
        assert_eq!(
            backend.sent_bytes.load(Ordering::Relaxed),
            produced,
            "pending PCM and flush must reach provider exactly once"
        );
        assert_eq!(artifacts.captured_ms, produced / 32);
        assert_eq!(artifacts.sent_ms, produced / 32);
    }

    #[derive(Clone)]
    struct MockBackend {
        calls: Arc<Mutex<Vec<&'static str>>>,
        auth_failure: bool,
        flush_failure: bool,
        handshake_gate: Option<Arc<tokio::sync::Barrier>>,
        polished: Outcome<String>,
    }
    impl MockBackend {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                auth_failure: false,
                flush_failure: false,
                handshake_gate: None,
                polished: Ok("No enviar 99".into()),
            }
        }
        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }
    }
    impl Backend for MockBackend {
        fn credential(&self, _: AppConfig, _: bool, _: CancellationToken) -> Job<Secret> {
            let this = self.clone();
            Box::pin(async move {
                this.record("credential");
                if this.auth_failure {
                    Err("Conectá Grok".into())
                } else {
                    Ok(Secret::new("fixture"))
                }
            })
        }
        fn stream(
            &self,
            _: AppConfig,
            _: Secret,
            mut audio: mpsc::Receiver<Vec<u8>>,
            events: mpsc::Sender<StreamEvent>,
            cancel: CancellationToken,
        ) -> Job<()> {
            let this = self.clone();
            Box::pin(async move {
                this.record("stream_ready");
                if let Some(gate) = &this.handshake_gate {
                    gate.wait().await;
                    gate.wait().await;
                }
                events.send(StreamEvent::Started).await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let chunk = tokio::select! {biased;_=cancel.cancelled()=>return Err("cancelled".into()),chunk=audio.recv()=>chunk};
                    match chunk {
                        Some(chunk) => bytes.extend(chunk),
                        None => break,
                    }
                    events
                        .send(StreamEvent::Partial {
                            text: "hiper land no 12".into(),
                            is_final: true,
                            speech_final: false,
                            start: Some(0.),
                            duration: Some(0.5),
                        })
                        .await
                        .unwrap();
                }
                this.record("audio_done");
                assert_eq!(
                    bytes,
                    if this.handshake_gate.is_some() {
                        vec![]
                    } else {
                        [vec![1, 2].repeat(800), vec![3, 4].repeat(800)].concat()
                    },
                    "Stop must drain both the initial PCM and trailing flush"
                );
                // Replaces the same time span and confirms an empty final.
                events
                    .send(StreamEvent::Partial {
                        text: "hiper land no 12".into(),
                        is_final: true,
                        speech_final: true,
                        start: Some(0.),
                        duration: Some(0.5),
                    })
                    .await
                    .unwrap();
                events
                    .send(StreamEvent::Final {
                        text: String::new(),
                        duration: Some(0.5),
                    })
                    .await
                    .unwrap();
                Ok(())
            })
        }
        fn capture(&self, _: Option<String>) -> Job<Capture> {
            let this = self.clone();
            Box::pin(async move {
                this.record("capture");
                let (audio_tx, audio) = mpsc::channel(2);
                let (status_tx, status) = watch::channel(AudioStatus::default());
                let (command_tx, command_rx) = std_mpsc::channel();
                let (finished_tx, finished) = oneshot::channel();
                std::thread::spawn(move || {
                    let _ = audio_tx.blocking_send(vec![1, 2].repeat(800));
                    status_tx.send_replace(AudioStatus {
                        recording: true,
                        level: 0.5,
                        error: None,
                    });
                    let flush = command_rx.recv().unwrap_or(false);
                    if flush {
                        this.record("flush");
                        audio_tx.blocking_send(vec![3, 4].repeat(800)).unwrap();
                    } else {
                        this.record("cancel_capture");
                    }
                    // Deliberately close PCM before publishing a flush failure.
                    // The engine must await finished, not trust channel closure.
                    drop(audio_tx);
                    let status = AudioStatus {
                        error: if this.flush_failure {
                            Some("Error tardío al cerrar PipeWire".into())
                        } else {
                            None
                        },
                        ..Default::default()
                    };
                    let _ = finished_tx.send(status);
                });
                Ok(Capture {
                    control: CaptureControl {
                        command: Some(command_tx),
                    },
                    audio,
                    status,
                    finished,
                })
            })
        }
        fn polish(
            &self,
            _: AppConfig,
            _: Secret,
            _: String,
            _: CancellationToken,
        ) -> Job<PolishResult> {
            let this = self.clone();
            Box::pin(async move {
                this.record("polish");
                this.polished.map(|text| PolishResult {
                    text,
                    input_tokens: Some(123),
                    output_tokens: Some(17),
                })
            })
        }
    }
    async fn collect(mut events: mpsc::Receiver<SessionEvent>) -> Vec<SessionEvent> {
        let mut all = Vec::new();
        while let Some(event) = events.recv().await {
            all.push(event);
        }
        all
    }
    async fn start_mock(
        backend: MockBackend,
        polish: bool,
    ) -> (
        watch::Sender<bool>,
        CancellationToken,
        mpsc::Receiver<SessionEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        start_mock_config(
            backend,
            AppConfig {
                polish,
                ..Default::default()
            },
            PathBuf::new(),
        )
        .await
    }
    async fn start_mock_config(
        backend: MockBackend,
        config: AppConfig,
        data_dir: PathBuf,
    ) -> (
        watch::Sender<bool>,
        CancellationToken,
        mpsc::Receiver<SessionEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let (stop_tx, stop) = watch::channel(false);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let (tx, mut rx) = mpsc::channel(32);
        let task = tokio::spawn(run_with(
            backend,
            config,
            data_dir,
            vec![Correction {
                id: 1,
                from: "hiper land".into(),
                to: "Hyprland".into(),
            }],
            false,
            stop,
            task_cancel,
            tx,
        ));
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
            {
                Some(SessionEvent::Started) => break,
                Some(_) => {}
                None => panic!("session ended before capture started"),
            }
        }
        (stop_tx, cancel, rx, task)
    }
    #[tokio::test]
    async fn releasing_during_handshake_never_opens_capture() {
        let mut backend = MockBackend::new();
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        backend.handshake_gate = Some(gate.clone());
        let calls = backend.calls.clone();
        let (stop_tx, stop) = watch::channel(false);
        let (tx, rx) = mpsc::channel(32);
        let task = tokio::spawn(run_with(
            backend,
            AppConfig::default(),
            PathBuf::new(),
            vec![],
            false,
            stop,
            CancellationToken::new(),
            tx,
        ));
        gate.wait().await;
        stop_tx.send_replace(true);
        gate.wait().await;
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let events = collect(rx).await;
        assert!(!calls.lock().unwrap().contains(&"capture"));
        assert!(calls.lock().unwrap().contains(&"audio_done"));
        assert!(!events.iter().any(|e| matches!(e, SessionEvent::Started)));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Finished { .. }))
        );
    }
    #[tokio::test]
    async fn credential_failure_never_opens_microphone() {
        let mut backend = MockBackend::new();
        backend.auth_failure = true;
        let calls = backend.calls.clone();
        let (_stop_tx, stop) = watch::channel(false);
        let (tx, rx) = mpsc::channel(8);
        run_with(
            backend,
            AppConfig::default(),
            PathBuf::new(),
            vec![],
            false,
            stop,
            CancellationToken::new(),
            tx,
        )
        .await;
        assert_eq!(*calls.lock().unwrap(), vec!["credential"]);
        assert!(
            matches!(&collect(rx).await[..],[SessionEvent::Failed { error: message, .. }] if message=="Conectá Grok")
        );
    }
    #[tokio::test]
    async fn stop_flushes_before_final_and_rejects_changed_numbers() {
        let backend = MockBackend::new();
        let calls = backend.calls.clone();
        let (stop, _cancel, rx, task) = start_mock(backend, true).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let events = collect(rx).await;
        assert!(events.iter().any(|e| matches!(e, SessionEvent::Polishing)));
        assert!(events.iter().any(|e|matches!(e,SessionEvent::Finished {original,text,warning:Some(_),..} if original=="hiper land no 12" && text=="Hyprland no 12")));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::Failed { .. }))
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "credential",
                "stream_ready",
                "capture",
                "flush",
                "audio_done",
                "polish"
            ]
        );
    }
    #[tokio::test]
    async fn flush_error_after_audio_closes_never_reports_success() {
        let mut backend = MockBackend::new();
        backend.flush_failure = true;
        let calls = backend.calls.clone();
        let (stop, _cancel, rx, task) = start_mock(backend, false).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let events = collect(rx).await;
        assert!(events.iter().any(
            |e| matches!(e,SessionEvent::Failed { error: message, .. } if message.contains("Error tardío"))
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SessionEvent::Finished { .. }))
        );
        assert!(!calls.lock().unwrap().contains(&"polish"));
    }
    #[tokio::test]
    async fn cancel_stops_capture_and_does_not_finalize_or_polish() {
        let backend = MockBackend::new();
        let calls = backend.calls.clone();
        let (_stop, cancel, rx, task) = start_mock(backend, true).await;
        cancel.cancel();
        task.await.unwrap();
        let events = collect(rx).await;
        assert!(!events.iter().any(|e| matches!(
            e,
            SessionEvent::Finished { .. } | SessionEvent::Failed { .. } | SessionEvent::Polishing
        )));
        let calls = calls.lock().unwrap();
        assert!(calls.contains(&"cancel_capture"));
        assert!(!calls.contains(&"audio_done"));
        assert!(!calls.contains(&"polish"));
    }
    fn archive_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static ID: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "mars-session-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ))
    }
    async fn heard(rx: &mut mpsc::Receiver<SessionEvent>) {
        loop {
            match tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                SessionEvent::Partial { .. } => break,
                _ => {}
            }
        }
    }
    #[tokio::test]
    async fn optional_audio_success_carries_exact_pcm_duration_and_actual_polish_usage() {
        let dir = archive_dir();
        let config = AppConfig {
            polish: true,
            audio_retention: dicta_core::AudioRetention::Days7,
            ..Default::default()
        };
        let (stop, _, mut rx, task) =
            start_mock_config(MockBackend::new(), config, dir.clone()).await;
        heard(&mut rx).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let events = collect(rx).await;
        let artifacts = events
            .iter()
            .find_map(|event| match event {
                SessionEvent::Finished { artifacts, .. } => Some(artifacts),
                _ => None,
            })
            .unwrap();
        assert_eq!((artifacts.captured_ms, artifacts.sent_ms), (100, 100));
        assert_eq!(
            (artifacts.input_tokens, artifacts.output_tokens),
            (Some(123), Some(17))
        );
        let data = std::fs::read(
            dir.join("audio")
                .join(artifacts.audio_filename.as_ref().unwrap()),
        )
        .unwrap();
        assert_eq!(data.len(), 3244);
        assert_eq!(artifacts.audio_bytes, 3244);
        assert_eq!(u32::from_le_bytes(data[40..44].try_into().unwrap()), 3200);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn failed_stt_keeps_a_valid_partial_audio_when_opted_in() {
        let dir = archive_dir();
        let mut backend = MockBackend::new();
        backend.flush_failure = true;
        let config = AppConfig {
            audio_retention: dicta_core::AudioRetention::Forever,
            ..Default::default()
        };
        let (stop, _, mut rx, task) = start_mock_config(backend, config, dir.clone()).await;
        heard(&mut rx).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let events = collect(rx).await;
        let artifacts = events
            .iter()
            .find_map(|event| match event {
                SessionEvent::Failed { artifacts, .. } => Some(artifacts),
                _ => None,
            })
            .unwrap();
        assert!(artifacts.captured_ms >= 50);
        assert_eq!(artifacts.input_tokens, None);
        let data = std::fs::read(
            dir.join("audio")
                .join(artifacts.audio_filename.as_ref().unwrap()),
        )
        .unwrap();
        assert_eq!(data.len() as u64, artifacts.audio_bytes);
        assert_eq!(
            u32::from_le_bytes(data[40..44].try_into().unwrap()) as usize,
            data.len() - 44
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn archive_failure_does_not_fail_dictation() {
        let dir = archive_dir();
        std::fs::write(&dir, b"block directory").unwrap();
        let config = AppConfig {
            audio_retention: dicta_core::AudioRetention::Days7,
            ..Default::default()
        };
        let (stop, _, mut rx, task) =
            start_mock_config(MockBackend::new(), config, dir.clone()).await;
        heard(&mut rx).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let events = collect(rx).await;
        assert!(events.iter().any(|event|matches!(event,SessionEvent::Finished {warning:Some(warning),artifacts,..} if warning.contains("guardar el audio") && artifacts.audio_filename.is_none())));
        std::fs::remove_file(dir).unwrap();
    }
    #[tokio::test]
    async fn disabled_history_prevents_audio_writes_even_with_retention() {
        let dir = archive_dir();
        let config = AppConfig {
            history_enabled: false,
            audio_retention: dicta_core::AudioRetention::Forever,
            ..Default::default()
        };
        let (stop, _, mut rx, task) =
            start_mock_config(MockBackend::new(), config, dir.clone()).await;
        heard(&mut rx).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        assert!(!dir.exists());
        assert!(collect(rx).await.iter().any(|event|matches!(event,SessionEvent::Finished {artifacts,..} if artifacts.audio_filename.is_none())));
    }
    #[test]
    fn polish_failure_preserves_approved_local_text_and_success_is_accepted() {
        let local = "No enviar 12 euros".to_owned();
        let (text, warning) = choose_polished(local.clone(), Err("timeout de corrección".into()));
        assert_eq!(text, local);
        assert!(warning.unwrap().contains("timeout"));
        assert_eq!(
            choose_polished(local.clone(), Ok("No enviar 12 euros.".into())),
            ("No enviar 12 euros.".into(), None)
        );
        assert_eq!(
            choose_polished(local, Ok("Enviar 12 euros".into())).0,
            "No enviar 12 euros"
        );
    }
}
