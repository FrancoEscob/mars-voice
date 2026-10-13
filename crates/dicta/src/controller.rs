use crate::history_ops::{self, Playback};
use crate::reader::{self, Reader};
use crate::session::{self, SessionEvent};
use anyhow::Result;
use dicta_core::{
    AppConfig, AppPaths, AppSnapshot, Command, LoginPrompt, NoteStatus, Phase, ProviderMode,
    SessionArtifacts, store::Store,
};
use dicta_platform::{audio, desktop, shortcuts, tray};
use dicta_provider::{CancellationToken, Credentials, OAuthClient, Secret, TokenSet};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};
use tokio::{runtime::Runtime, sync::watch};

// Match the requested 20ms meter cadence without publishing unchanged idle
// state. Large history collections are shared between these snapshots.
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub demo: bool,
    pub portal: bool,
    pub allow_env_key: bool,
    pub no_tray: bool,
}
struct Job {
    id: u64,
    cancel: CancellationToken,
    stop: watch::Sender<bool>,
    started: Instant,
    created_at_ms: u64,
    provider: ProviderMode,
    history_enabled: bool,
    artifacts: SessionArtifacts,
}
enum Event {
    Session(u64, SessionEvent),
    Devices(Result<Vec<dicta_core::AudioDevice>, String>),
    Login(u64, LoginPrompt),
    Auth(u64, ProviderMode, Result<bool, String>),
    Presence(u64, ProviderMode, Result<bool, String>),
    Notice(String),
    Output(u64, u64, u64, Result<&'static str, String>),
}
/// Generation counters make a cancelled dictation or login unable to resurrect
/// itself when its last network event arrives. Output also carries the text revision.
#[derive(Default)]
struct Generations {
    session: u64,
    auth: u64,
    text: u64,
    notice: u64,
}
impl Generations {
    fn new_session(&mut self) -> u64 {
        self.session += 1;
        self.text += 1;
        self.session
    }
    fn accepts_output(&self, session: u64, text: u64) -> bool {
        self.session == session && self.text == text
    }
}

pub fn run(
    paths: AppPaths,
    mut snapshot: AppSnapshot,
    commands: Receiver<Command>,
    command_tx: Sender<Command>,
    snapshots: Sender<AppSnapshot>,
    options: Options,
) -> Result<()> {
    desktop::set_overlay_placement_enabled(!snapshot.config.overlay_centered);
    desktop::set_reader_placement_enabled(!snapshot.config.tts.window_centered);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let store = if options.demo {
        Store::in_memory()?
    } else {
        Store::open(&paths.data)?
    };
    snapshot.corrections = store.list()?;
    if let Err(e) = history_ops::prune(&store, &paths, &mut snapshot, options.demo) {
        snapshot.error = Some(e.to_string());
    }
    let mut playback = Playback::default();
    let mut last_note_id = None;
    let mut last_prune = Instant::now();
    let credentials = Credentials::new();
    let mut reader = Reader::new(
        runtime.handle().clone(),
        options.demo,
        options.allow_env_key,
        credentials.clone(),
    )?;
    let (events_tx, events) = mpsc::channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut login_cancel = CancellationToken::new();
    let (auth_tx, auth_rx) = tokio::sync::mpsc::unbounded_channel();
    runtime.spawn(credential_writer(
        credentials.clone(),
        auth_rx,
        events_tx.clone(),
    ));
    let output_control = OutputControl::default();
    let mut gen_ids = Generations::default();
    let mut job: Option<Job> = None;
    let mut shortcut_state = crate::shortcut_state::ShortcutState::default();
    let mut remembered_target: Option<desktop::PasteTarget> = None;
    let mut dirty = true;
    let mut last_publish = Instant::now() - Duration::from_secs(1);
    let tray_handle = if !options.demo && !options.no_tray {
        match runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), tray::spawn(command_tx.clone())).await
        }) {
            Ok(Ok(handle)) => Some(handle),
            _ => {
                snapshot.status =
                    "Sin bandeja disponible; podés usar el launcher y dicta ctl".into();
                None
            }
        }
    } else {
        None
    };
    if options.portal && !options.demo {
        let tx = events_tx.clone();
        let commands = command_tx.clone();
        let config = snapshot.config.clone();
        runtime.spawn(async move {
            if let Err(e) = shortcuts::run(&config, commands, shutdown_rx).await {
                let _ = tx.send(Event::Notice(e.to_string()));
            }
        });
    }
    if !options.demo {
        refresh_devices(&runtime, &events_tx);
    }
    // Inspect only Dicta's own entries when settings are visible. Presence is
    // not a network authentication check or a claim about subscription coverage.
    if !options.demo && snapshot.show_settings {
        let _ = auth_tx.send(CredentialWrite::Check(
            gen_ids.auth,
            snapshot.config.provider,
            options.allow_env_key,
        ));
        if snapshot.config.tts.provider != snapshot.config.provider {
            let _ = auth_tx.send(CredentialWrite::Check(
                gen_ids.auth,
                snapshot.config.tts.provider,
                options.allow_env_key,
            ));
        }
    }
    loop {
        match commands.recv_timeout(Duration::from_millis(20)) {
            Ok(Command::Quit) => break,
            Ok(command) => {
                // Translate key edges here so portal and manual binds use the
                // current mode without restarting either transport.
                let command = match command {
                    Command::ShortcutPressed { at, token } => shortcut_state.press(
                        at,
                        token,
                        snapshot.config.shortcut_mode,
                        snapshot.phase.busy(),
                        gen_ids.session + 1,
                    ),
                    Command::ShortcutReleased { at, token } => {
                        shortcut_state.release(at, token, job.as_ref().map(|j| j.id))
                    }
                    other => Some(other),
                };
                let Some(command) = command else {
                    continue;
                };
                dirty = true;
                // Async copy may finish after a newer command's status was
                // published. This only orders success notices, not clipboard
                // operations or their actionable errors.
                gen_ids.notice = gen_ids.notice.wrapping_add(1);
                match command {
                    command if reader::is_reader_command(&command) => {
                        let busy = snapshot.phase.busy();
                        if let Ok(outcome) =
                            reader.handle(command, &mut snapshot, &store, &paths, busy)
                        {
                            if outcome.stop_note_audio {
                                playback.stop();
                                snapshot.audio_playing_note_id = None;
                            }
                            if outcome.save_config {
                                let config = snapshot.config.clone();
                                save_config(&paths, &mut snapshot, config, options.demo);
                            }
                        }
                    }
                    Command::Start | Command::Toggle if !snapshot.phase.busy() => {
                        playback.stop();
                        // El micrófono no debe captar la lectura en voz alta.
                        reader.on_dictation_start(&mut snapshot, &store, &paths);
                        snapshot.audio_playing_note_id = None;
                        last_note_id = None;
                        output_control.invalidate();
                        // Capture before the new snapshot can focus/show our overlay.
                        // Starting from our own UI clears a stale previous destination.
                        remembered_target = if options.demo {
                            None
                        } else {
                            desktop::active_target().ok()
                        };
                        let id = gen_ids.new_session();
                        snapshot.phase = Phase::Connecting;
                        snapshot.confirmed.clear();
                        snapshot.partial.clear();
                        snapshot.original_text.clear();
                        snapshot.final_text.clear();
                        snapshot.error = None;
                        snapshot.level = 0.0;
                        snapshot.elapsed_ms = 0;
                        snapshot.show_overlay = true;
                        snapshot.status = if options.demo {
                            "Demo: audio y red simulados"
                        } else {
                            "Conectando con Grok…"
                        }
                        .into();
                        let cancel = CancellationToken::new();
                        let (stop_tx, stop_rx) = watch::channel(false);
                        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
                        let output = events_tx.clone();
                        runtime.spawn(async move {
                            while let Some(event) = rx.recv().await {
                                if output.send(Event::Session(id, event)).is_err() {
                                    break;
                                }
                            }
                        });
                        if options.demo {
                            runtime.spawn(demo_session(stop_rx, cancel.clone(), tx));
                        } else {
                            runtime.spawn(session::run(
                                snapshot.config.clone(),
                                paths.data.clone(),
                                snapshot.corrections.clone(),
                                credentials.clone(),
                                options.allow_env_key,
                                stop_rx,
                                cancel.clone(),
                                tx,
                            ));
                        }
                        job = Some(Job {
                            id,
                            cancel,
                            stop: stop_tx,
                            started: Instant::now(),
                            created_at_ms: history_ops::now_ms(),
                            provider: snapshot.config.provider,
                            history_enabled: snapshot.config.history_enabled,
                            artifacts: SessionArtifacts::default(),
                        });
                    }
                    Command::Toggle | Command::Stop => {
                        if snapshot.phase != Phase::Polishing {
                            if let Some(active) = &job {
                                let _ = active.stop.send(true);
                                snapshot.phase = Phase::Finalizing;
                                snapshot.status = "Terminando la transcripción…".into();
                            }
                        }
                    }
                    Command::Cancel => {
                        shortcut_state.detach();
                        output_control.invalidate();
                        remembered_target = None;
                        let was_busy = job.is_some();
                        if let Some(active) = job.take() {
                            active.cancel.cancel();
                        }
                        gen_ids.new_session();
                        snapshot.phase = Phase::Idle;
                        snapshot.partial.clear();
                        snapshot.level = 0.0;
                        snapshot.status = if was_busy {
                            "Dictado cancelado"
                        } else {
                            "En espera"
                        }
                        .into();
                        snapshot.error = None;
                    }
                    Command::SetTheme(theme) => {
                        let mut config = snapshot.config.clone();
                        config.theme = theme;
                        save_config(&paths, &mut snapshot, config, options.demo);
                    }
                    Command::SetColorMode(mode) => {
                        let mut config = snapshot.config.clone();
                        config.color_mode = mode;
                        save_config(&paths, &mut snapshot, config, options.demo);
                    }
                    Command::SetUsagePeriod(period) => {
                        snapshot.usage_period = period;
                        if let Err(e) = history_ops::refresh_metrics(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::SearchNotes(query) => {
                        snapshot.history_query = query.chars().take(500).collect();
                        snapshot.notes_limit = 100;
                        if let Err(e) = history_ops::refresh_notes(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::LoadMoreNotes => {
                        snapshot.notes_limit = snapshot.notes_limit.saturating_add(100);
                        if let Err(e) = history_ops::refresh_notes(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::OpenNote(id) => {
                        snapshot.active_note_id = Some(id);
                        if let Err(e) = history_ops::refresh_selection(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::CopyNote(id) => match store.note(id) {
                        Ok(Some(note)) => {
                            let mut output = snapshot.clone();
                            output.final_text = note.text;
                            output_text(
                                &runtime,
                                &events_tx,
                                &output,
                                &gen_ids,
                                &output_control,
                                None,
                                false,
                                options.demo,
                            );
                        }
                        Ok(None) => snapshot.error = Some("La nota ya no está disponible".into()),
                        Err(e) => snapshot.error = Some(e.to_string()),
                    },
                    Command::PlayNoteAudio(id) => {
                        if snapshot.phase.busy() {
                            snapshot.error =
                                Some("Terminá el dictado antes de reproducir audio".into());
                        } else if options.demo {
                            snapshot.status =
                                "Demo: la reproducción de audio está desactivada".into();
                        } else {
                            reader.pause();
                            let result = store
                                .note(id)
                                .and_then(|n| {
                                    n.and_then(|n| n.audio_filename).ok_or_else(|| {
                                        anyhow::anyhow!("La nota no tiene audio guardado")
                                    })
                                })
                                .and_then(|name| {
                                    playback.play(&paths, &snapshot.config.tts, id, &name)
                                });
                            match result {
                                Ok(()) => snapshot.audio_playing_note_id = Some(id),
                                Err(e) => snapshot.error = Some(e.to_string()),
                            }
                        }
                    }
                    Command::StopAudioPlayback => {
                        playback.stop();
                        snapshot.audio_playing_note_id = None;
                    }
                    Command::DeleteNote(id) | Command::DeleteNoteAudio(id) => {
                        if snapshot.audio_playing_note_id == Some(id) {
                            playback.stop();
                            snapshot.audio_playing_note_id = None;
                        }
                        let delete_note = matches!(command, Command::DeleteNote(_));
                        let result =
                            history_ops::delete(&store, &paths, id, delete_note, options.demo);
                        if delete_note && last_note_id == Some(id) {
                            last_note_id = None;
                        }
                        if let Err(e) = result {
                            snapshot.error = Some(e.to_string());
                        }
                        if let Err(e) = history_ops::refresh(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::SetHistoryKind(kind) => snapshot.history_kind = kind,
                    Command::OpenReading(id) => {
                        snapshot.active_reading_id = Some(id);
                        if let Err(e) = history_ops::refresh_selection(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::CopyReading(id) => match store.reading(id) {
                        Ok(Some(reading)) => {
                            let mut output = snapshot.clone();
                            output.final_text = reading.text;
                            output_text(
                                &runtime,
                                &events_tx,
                                &output,
                                &gen_ids,
                                &output_control,
                                None,
                                false,
                                options.demo,
                            );
                        }
                        Ok(None) => {
                            snapshot.error = Some("La lectura ya no está disponible".into())
                        }
                        Err(e) => snapshot.error = Some(e.to_string()),
                    },
                    Command::DeleteReading(id) | Command::DeleteReadingAudio(id) => {
                        let whole = matches!(command, Command::DeleteReading(_));
                        if let Err(e) =
                            history_ops::delete_reading(&store, &paths, id, whole, options.demo)
                        {
                            snapshot.error = Some(e.to_string());
                        }
                        if let Err(e) = history_ops::refresh(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::ClearReadings => {
                        if let Err(e) = history_ops::clear_readings(&store, &paths, options.demo) {
                            snapshot.error = Some(e.to_string());
                        }
                        if let Err(e) = history_ops::refresh(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::ClearHistory => {
                        playback.stop();
                        snapshot.audio_playing_note_id = None;
                        last_note_id = None;
                        let result = history_ops::clear(&store, &paths, options.demo);
                        if let Err(e) = result {
                            snapshot.error = Some(e.to_string());
                        }
                        if let Err(e) = history_ops::refresh(&store, &mut snapshot) {
                            snapshot.error = Some(e.to_string());
                        }
                    }
                    Command::UpdateConfig(config) => {
                        let changed_auth = config.provider != snapshot.config.provider
                            || config.oauth_client_id != snapshot.config.oauth_client_id;
                        let changed_tts_mode = config.tts.provider != snapshot.config.tts.provider;
                        if changed_auth && snapshot.phase.busy() {
                            snapshot.error =
                                Some("Terminá el dictado antes de cambiar el proveedor".into());
                        } else {
                            let changed_shortcut_mode =
                                config.shortcut_mode != snapshot.config.shortcut_mode;
                            if save_config(&paths, &mut snapshot, config, options.demo) {
                                playback.stop();
                                snapshot.audio_playing_note_id = None;
                                if let Err(e) =
                                    history_ops::prune(&store, &paths, &mut snapshot, options.demo)
                                {
                                    snapshot.error = Some(e.to_string());
                                }
                                if changed_shortcut_mode
                                    && shortcut_state
                                        .finish_owned(job.as_ref().map(|j| j.id))
                                        .is_some()
                                {
                                    if let Some(active) = &job {
                                        active.stop.send_replace(true);
                                        if snapshot.phase != Phase::Polishing {
                                            snapshot.phase = Phase::Finalizing;
                                            snapshot.status = "Terminando la transcripción…".into();
                                        }
                                    }
                                }
                                if changed_auth {
                                    login_cancel.cancel();
                                    gen_ids.auth += 1;
                                    snapshot.login = None;
                                    snapshot.authenticated = false;
                                }
                                reader.on_config(&snapshot.config);
                                if (changed_tts_mode || changed_auth) && !options.demo {
                                    let _ = auth_tx.send(CredentialWrite::Check(
                                        gen_ids.auth,
                                        snapshot.config.tts.provider,
                                        options.allow_env_key,
                                    ));
                                }
                            }
                        }
                    }
                    Command::EditText(text) if !snapshot.phase.busy() => {
                        output_control.invalidate();
                        snapshot.final_text = text;
                        if let Some(id) = last_note_id {
                            if let Err(e) = store
                                .edit_note_text(id, &snapshot.final_text)
                                .and_then(|_| history_ops::refresh(&store, &mut snapshot))
                            {
                                snapshot.error = Some(e.to_string());
                            }
                        }
                        gen_ids.text += 1;
                        snapshot.status =
                            "Texto editado; usá Copiar para actualizar el portapapeles".into();
                    }
                    Command::RememberCorrection { from, to } => {
                        match store.remember(&from, &to).and_then(|_| store.list()) {
                            Ok(list) => {
                                snapshot.corrections = list;
                                snapshot.status =
                                    "Corrección guardada para próximos dictados".into();
                            }
                            Err(e) => snapshot.error = Some(e.to_string()),
                        }
                    }
                    Command::ForgetCorrection(id) => {
                        match store.forget(id).and_then(|_| store.list()) {
                            Ok(list) => snapshot.corrections = list,
                            Err(e) => snapshot.error = Some(e.to_string()),
                        }
                    }
                    Command::Copy | Command::Paste => {
                        if snapshot.phase.busy() {
                            snapshot.status = "Terminá el dictado antes de copiar o pegar".into();
                        } else if !snapshot.final_text.is_empty() {
                            output_text(
                                &runtime,
                                &events_tx,
                                &snapshot,
                                &gen_ids,
                                &output_control,
                                remembered_target.as_ref(),
                                matches!(command, Command::Paste),
                                options.demo,
                            );
                        }
                    }
                    Command::ShowSettings => {
                        snapshot.show_settings = true;
                        reader.refresh_outputs();
                        reader.ensure_voices(&snapshot.config);
                        if !options.demo {
                            let _ = auth_tx.send(CredentialWrite::Check(
                                gen_ids.auth,
                                snapshot.config.provider,
                                options.allow_env_key,
                            ));
                            if snapshot.config.tts.provider != snapshot.config.provider {
                                let _ = auth_tx.send(CredentialWrite::Check(
                                    gen_ids.auth,
                                    snapshot.config.tts.provider,
                                    options.allow_env_key,
                                ));
                            }
                        }
                    }
                    Command::HideSettings => {
                        snapshot.show_settings = false;
                        // Visibility is an edge, not just a coalescible value:
                        // a subsequent ShowSettings must reopen a dismissed UI.
                        snapshot.revision += 1;
                        let _ = snapshots.send(snapshot.clone());
                        last_publish = Instant::now();
                    }
                    Command::ShowOverlay => snapshot.show_overlay = true,
                    Command::HideOverlay => snapshot.show_overlay = false,
                    Command::StartLogin
                        if !options.demo
                            && snapshot.config.provider != ProviderMode::Subscription
                            && snapshot.config.tts.provider != ProviderMode::Subscription =>
                    {
                        snapshot.error =
                            Some("Seleccioná Suscripción para iniciar sesión OAuth".into());
                    }
                    Command::StartLogin if !options.demo => {
                        login_cancel.cancel();
                        login_cancel = CancellationToken::new();
                        gen_ids.auth += 1;
                        snapshot.error = None;
                        snapshot.login = None;
                        snapshot.status = "Solicitando acceso a Grok…".into();
                        let id = gen_ids.auth;
                        let config = snapshot.config.clone();
                        let tx = events_tx.clone();
                        let cancel = login_cancel.clone();
                        let writer = auth_tx.clone();
                        runtime.spawn(async move {
                            let result = async {
                                let oauth = OAuthClient::from_config(&config)?;
                                let device = oauth.begin_device_login(&cancel).await?;
                                let _ = tx.send(Event::Login(
                                    id,
                                    LoginPrompt {
                                        verification_uri: device.verification_uri.clone(),
                                        user_code: device.user_code.clone(),
                                    },
                                ));
                                let tokens = oauth.poll_device_login(&device, &cancel).await?;
                                if !cancel.is_cancelled() {
                                    let _ = writer.send(CredentialWrite::Tokens(
                                        id,
                                        tokens,
                                        cancel.clone(),
                                    ));
                                }
                                Ok::<(), dicta_provider::ProviderError>(())
                            }
                            .await;
                            if let Err(error) = result {
                                let _ = tx.send(Event::Auth(
                                    id,
                                    ProviderMode::Subscription,
                                    Err(error.to_string()),
                                ));
                            }
                        });
                    }
                    Command::CancelLogin => {
                        login_cancel.cancel();
                        gen_ids.auth += 1;
                        snapshot.login = None;
                        snapshot.status = "Inicio de sesión cancelado".into();
                    }
                    Command::SaveApiKey(key) if !options.demo => {
                        login_cancel.cancel();
                        login_cancel = CancellationToken::new();
                        gen_ids.auth += 1;
                        snapshot.login = None;
                        snapshot.error = None;
                        snapshot.status = "Guardando API key en el llavero…".into();
                        let id = gen_ids.auth;
                        let _ = auth_tx.send(CredentialWrite::ApiKey(
                            id,
                            Secret::new(key),
                            login_cancel.clone(),
                        ));
                    }
                    Command::Disconnect if !options.demo => {
                        login_cancel.cancel();
                        login_cancel = CancellationToken::new();
                        gen_ids.auth += 1;
                        let id = gen_ids.auth;
                        snapshot.login = None;
                        snapshot.authenticated = false;
                        output_control.invalidate();
                        remembered_target = None;
                        if let Some(active) = job.take() {
                            active.cancel.cancel();
                        }
                        gen_ids.new_session();
                        snapshot.phase = Phase::Idle;
                        snapshot.level = 0.0;
                        snapshot.partial.clear();
                        snapshot.status = "Quitando la credencial del llavero…".into();
                        let _ = auth_tx.send(CredentialWrite::Clear(id, snapshot.config.provider));
                    }
                    Command::RefreshDevices if !options.demo => {
                        refresh_devices(&runtime, &events_tx)
                    }
                    Command::StartLogin
                    | Command::SaveApiKey(_)
                    | Command::Disconnect
                    | Command::RefreshDevices => {
                        snapshot.status =
                            "Demo: las cuentas y el micrófono están desactivados".into()
                    }
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        while let Ok(event) = events.try_recv() {
            dirty = true;
            match event {
                Event::Session(id, event)
                    if id == gen_ids.session && job.as_ref().is_some_and(|j| j.id == id) =>
                {
                    match event {
                        SessionEvent::Started => {
                            if let Some(active) = job.as_mut() {
                                active.started = Instant::now();
                            }
                            if snapshot.phase == Phase::Connecting {
                                snapshot.phase = Phase::Recording;
                            }
                            snapshot.authenticated = !options.demo;
                            if snapshot.phase == Phase::Recording {
                                snapshot.status = if options.demo {
                                    "Demo: dictando un ejemplo"
                                } else {
                                    "Te escucho…"
                                }
                                .into();
                            }
                        }
                        SessionEvent::Partial { confirmed, partial } => {
                            snapshot.confirmed = confirmed;
                            snapshot.partial = partial;
                        }
                        SessionEvent::Level(level) => {
                            snapshot.level = level;
                            snapshot.level_seq = snapshot.level_seq.wrapping_add(1);
                        }
                        SessionEvent::Telemetry(artifacts) => {
                            snapshot.elapsed_ms = artifacts.captured_ms;
                            if let Some(active) = job.as_mut() {
                                active.artifacts = artifacts;
                            }
                        }
                        SessionEvent::Polishing => {
                            snapshot.phase = Phase::Polishing;
                            snapshot.status = "Aplicando tus instrucciones…".into();
                        }
                        SessionEvent::Finished {
                            original,
                            text,
                            warning,
                            artifacts,
                        } => {
                            snapshot.elapsed_ms = artifacts.captured_ms;
                            if let Some(active) = job.take() {
                                match history_ops::save_note(
                                    &store,
                                    &paths,
                                    &mut snapshot,
                                    active.created_at_ms,
                                    active.provider,
                                    active.history_enabled,
                                    original.clone(),
                                    text.clone(),
                                    NoteStatus::Completed,
                                    None,
                                    artifacts,
                                    options.demo,
                                ) {
                                    Ok(id) => last_note_id = id,
                                    Err(e) => snapshot.error = Some(e.to_string()),
                                }
                            }
                            gen_ids.text += 1;
                            snapshot.phase = Phase::Ready;
                            snapshot.level = 0.0;
                            snapshot.partial.clear();
                            snapshot.original_text = original;
                            snapshot.confirmed = text.clone();
                            snapshot.final_text = text;
                            snapshot.status = warning.unwrap_or_else(|| "Dictado listo".into());
                            if snapshot.config.copy_on_finish && !snapshot.final_text.is_empty() {
                                output_text(
                                    &runtime,
                                    &events_tx,
                                    &snapshot,
                                    &gen_ids,
                                    &output_control,
                                    None,
                                    false,
                                    options.demo,
                                );
                            }
                        }
                        SessionEvent::Failed {
                            mut error,
                            artifacts,
                        } => {
                            if !options.demo
                                && (error.contains("micrófono") || error.contains("PipeWire"))
                            {
                                refresh_devices(&runtime, &events_tx);
                            }
                            snapshot.elapsed_ms = artifacts.captured_ms;
                            let partial = format!("{} {}", snapshot.confirmed, snapshot.partial)
                                .trim()
                                .to_string();
                            if let Some(active) = job.take() {
                                if let Err(e) = history_ops::save_note(
                                    &store,
                                    &paths,
                                    &mut snapshot,
                                    active.created_at_ms,
                                    active.provider,
                                    active.history_enabled,
                                    partial.clone(),
                                    partial,
                                    NoteStatus::Failed,
                                    Some(error.clone()),
                                    artifacts,
                                    options.demo,
                                ) {
                                    error = format!("{error}. {}", e);
                                }
                            }
                            snapshot.phase = Phase::Error;
                            snapshot.level = 0.0;
                            snapshot.error = Some(error);
                            snapshot.status =
                                "No se completó el dictado; los fragmentos quedan visibles".into();
                        }
                    }
                }
                Event::Session(_, SessionEvent::Finished { artifacts, .. })
                | Event::Session(_, SessionEvent::Failed { artifacts, .. }) => {
                    // A cancellation can race the final event; discard any committed orphan.
                    if !options.demo {
                        if let Some(name) = artifacts.audio_filename {
                            let _ = history_ops::remove_audio(&paths, &name);
                        }
                    }
                }
                Event::Devices(Ok(devices)) => snapshot.devices = devices,
                Event::Devices(Err(e)) => snapshot.status = e,
                Event::Login(id, prompt) if id == gen_ids.auth => {
                    snapshot.login = Some(prompt);
                    snapshot.status = "Autorizá el dispositivo en el sitio de Grok".into();
                }
                Event::Auth(id, mode, result) if id == gen_ids.auth => {
                    apply_auth_result(&mut snapshot, mode, result)
                }
                Event::Output(id, revision, notice, result)
                    if gen_ids.accepts_output(id, revision) =>
                {
                    apply_output_result(&mut snapshot, &gen_ids, notice, result);
                }
                Event::Presence(id, mode, result) if id == gen_ids.auth => {
                    if let Ok(present) = result {
                        if mode == snapshot.config.provider {
                            snapshot.authenticated = present;
                        }
                        if mode == snapshot.config.tts.provider {
                            snapshot.tts.account_ready = present;
                        }
                    }
                }
                Event::Notice(message) => snapshot.status = message,
                _ => {}
            }
        }
        if reader.poll(&mut snapshot, &store, &paths) {
            dirty = true;
        }
        match playback.poll() {
            Ok(id) if id != snapshot.audio_playing_note_id => {
                snapshot.audio_playing_note_id = id;
                dirty = true;
            }
            Err(e) => {
                snapshot.audio_playing_note_id = None;
                snapshot.error = Some(e.to_string());
                dirty = true;
            }
            _ => {}
        }
        if last_prune.elapsed() >= Duration::from_secs(60) {
            // Do not expire a file currently being listened to.
            if snapshot.audio_playing_note_id.is_none() {
                if let Err(e) = history_ops::prune(&store, &paths, &mut snapshot, options.demo) {
                    snapshot.error = Some(e.to_string());
                }
                dirty = true;
            }
            last_prune = Instant::now();
        }
        if options.demo {
            if let Some(active) = &job {
                snapshot.elapsed_ms = active.started.elapsed().as_millis() as u64;
                dirty = true;
            }
        }
        if dirty && last_publish.elapsed() >= SNAPSHOT_INTERVAL {
            snapshot.revision += 1;
            let _ = snapshots.send(snapshot.clone());
            dirty = false;
            last_publish = Instant::now();
        }
    }
    reader.shutdown(&mut snapshot, &store, &paths);
    playback.stop();
    output_control.invalidate();
    desktop::set_overlay_placement_enabled(false);
    desktop::set_reader_placement_enabled(false);
    if let Some(active) = job {
        active.cancel.cancel();
    }
    login_cancel.cancel();
    drop(auth_tx);
    let _ = shutdown_tx.send(true);
    drop(tray_handle);
    // Give capture owners and sockets time to close, then shut down workers.
    runtime.block_on(async { tokio::time::sleep(Duration::from_millis(150)).await });
    runtime.shutdown_timeout(Duration::from_secs(3));
    // Terminal events already queued before Quit still own their WAVs.
    // No consumer remains to turn them into notes; discard them explicitly.
    while let Ok(event) = events.try_recv() {
        if let Event::Session(
            _,
            SessionEvent::Finished { artifacts, .. } | SessionEvent::Failed { artifacts, .. },
        ) = event
        {
            if !options.demo {
                if let Some(name) = artifacts.audio_filename {
                    let _ = history_ops::remove_audio(&paths, &name);
                }
            }
        }
    }
    Ok(())
}
fn save_config(
    paths: &AppPaths,
    snapshot: &mut AppSnapshot,
    mut config: AppConfig,
    demo: bool,
) -> bool {
    config.normalize();
    let result = config
        .validate()
        .and_then(|_| if demo { Ok(()) } else { paths.save(&config) });
    match result {
        Ok(()) => {
            desktop::set_overlay_placement_enabled(!config.overlay_centered);
            desktop::set_reader_placement_enabled(!config.tts.window_centered);
            snapshot.config = config;
            snapshot.error = None;
            true
        }
        Err(e) => {
            snapshot.error = Some(e.to_string());
            false
        }
    }
}
fn refresh_devices(runtime: &Runtime, tx: &Sender<Event>) {
    let tx = tx.clone();
    runtime.spawn_blocking(move || {
        let _ = tx.send(Event::Devices(
            audio::enumerate_devices().map_err(|e| e.to_string()),
        ));
    });
}
// One writer preserves command order even when Tokio schedules auth tasks out
// of order. Explicit deletes are never discarded; later writes run after them.
enum CredentialWrite {
    Tokens(u64, TokenSet, CancellationToken),
    ApiKey(u64, Secret, CancellationToken),
    Clear(u64, ProviderMode),
    Check(u64, ProviderMode, bool),
}
async fn credential_writer(
    credentials: Credentials,
    mut requests: tokio::sync::mpsc::UnboundedReceiver<CredentialWrite>,
    events: Sender<Event>,
) {
    while let Some(request) = requests.recv().await {
        let (id, mode, result) = match request {
            CredentialWrite::Check(id, mode, allow_env) => {
                let result = credentials
                    .has_credentials(mode, allow_env)
                    .await
                    .map_err(|e| e.to_string());
                let _ = events.send(Event::Presence(id, mode, result));
                continue;
            }
            CredentialWrite::Tokens(id, tokens, cancel) => {
                if cancel.is_cancelled() {
                    continue;
                }
                (
                    id,
                    ProviderMode::Subscription,
                    credentials.store_tokens(tokens).await.map(|_| true),
                )
            }
            CredentialWrite::ApiKey(id, key, cancel) => {
                if cancel.is_cancelled() {
                    continue;
                }
                (
                    id,
                    ProviderMode::ApiKey,
                    credentials.save_api_key(key).await.map(|_| true),
                )
            }
            CredentialWrite::Clear(id, mode) => {
                (id, mode, credentials.clear(mode).await.map(|_| false))
            }
        };
        let _ = events.send(Event::Auth(id, mode, result.map_err(|e| e.to_string())));
    }
}
fn apply_auth_result(snapshot: &mut AppSnapshot, mode: ProviderMode, result: Result<bool, String>) {
    snapshot.login = None;
    match result {
        Ok(present) => {
            if snapshot.config.provider == mode {
                snapshot.authenticated = present;
            }
            if snapshot.config.tts.provider == mode {
                snapshot.tts.account_ready = present;
            }
            snapshot.error = None;
            snapshot.status = if !present {
                "Cuenta desconectada"
            } else if snapshot.config.provider != mode && snapshot.config.tts.provider == mode {
                "Credencial guardada para la lectura en voz alta"
            } else if snapshot.config.provider != mode {
                "API key guardada; seleccioná API para usarla"
            } else {
                "Credencial guardada. La cobertura de STT se comprueba al dictar"
            }
            .into();
        }
        Err(error) => {
            snapshot.error = Some(error);
            snapshot.status = "No se pudo actualizar la credencial".into();
        }
    }
}
#[derive(Clone, Default)]
struct OutputControl {
    epoch: Arc<AtomicU64>,
    gate: Arc<Mutex<()>>,
}
impl OutputControl {
    fn invalidate(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::AcqRel) + 1
    }
    fn current(&self, id: u64) -> bool {
        self.epoch.load(Ordering::Acquire) == id
    }
}
fn output_text(
    runtime: &Runtime,
    tx: &Sender<Event>,
    snapshot: &AppSnapshot,
    ids: &Generations,
    control: &OutputControl,
    remembered: Option<&desktop::PasteTarget>,
    paste: bool,
    demo: bool,
) {
    // External active windows win over the remembered target. Only a request
    // from our own UI may restore the target captured at dictation start.
    // Invalid/missing destinations still fall back to copying, without input.
    let target = if paste && !demo {
        Some(desktop::prepare_paste(remembered))
    } else {
        None
    };
    let tx = tx.clone();
    let text = snapshot.final_text.clone();
    let keys = snapshot.config.paste_keys.clone();
    let id = ids.session;
    let revision = ids.text;
    let notice = ids.notice;
    let operation = control.invalidate();
    let control = control.clone();
    runtime.spawn_blocking(move || {
        let Ok(_guard) = control.gate.lock() else {
            return;
        };
        if !control.current(operation) {
            return;
        }
        let result = if demo {
            Ok(if paste {
                "Demo: pegado simulado"
            } else {
                "Demo: copiado simulado"
            })
        } else if let Some(target) = target {
            match target {
                Ok(plan) => {
                    desktop::paste_prepared(&text, &plan, &keys, || control.current(operation))
                        .map(|_| "Texto pegado")
                }
                Err(error) => desktop::copy(&text).and_then(|_| Err(error)),
            }
        } else {
            desktop::copy(&text).map(|_| "Texto copiado al portapapeles")
        };
        if control.current(operation) {
            let _ = tx.send(Event::Output(
                id,
                revision,
                notice,
                result.map_err(|e| e.to_string()),
            ));
        }
    });
}
fn apply_output_result(
    snapshot: &mut AppSnapshot,
    ids: &Generations,
    notice: u64,
    result: Result<&'static str, String>,
) {
    match result {
        Ok(status) if notice == ids.notice => snapshot.status = status.into(),
        Err(error) => snapshot.error = Some(error),
        _ => {}
    }
}
async fn demo_session(
    mut stop: watch::Receiver<bool>,
    cancel: CancellationToken,
    tx: tokio::sync::mpsc::Sender<SessionEvent>,
) {
    let _ = tx.send(SessionEvent::Started).await;
    let words = [
        "Hola,",
        "esto",
        "es",
        "Mars voice.",
        "Una",
        "idea",
        "dicha",
        "en",
        "voz",
        "alta,",
    ];
    let mut text = String::new();
    for (i, word) in words.iter().enumerate() {
        if *stop.borrow() {
            break;
        }
        tokio::select! {_ = cancel.cancelled()=>return, _ = stop.changed()=>break,_ = tokio::time::sleep(Duration::from_millis(350))=>{}}
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(word);
        let _ = tx
            .send(SessionEvent::Level(0.25 + (i % 4) as f32 * 0.15))
            .await;
        let _ = tx
            .send(SessionEvent::Partial {
                confirmed: String::new(),
                partial: text.clone(),
            })
            .await;
    }
    if cancel.is_cancelled() {
        return;
    }
    if text.is_empty() {
        text = "Hola, esto es Mars voice.".into();
    }
    let _ = tx
        .send(SessionEvent::Finished {
            original: text.clone(),
            text,
            warning: Some("Ejemplo simulado: no se usó micrófono ni API".into()),
            artifacts: SessionArtifacts {
                captured_ms: 3500,
                sent_ms: 3500,
                ..Default::default()
            },
        })
        .await;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_copy_notice_preserves_newer_command_status_without_hiding_errors() {
        let mut snapshot = AppSnapshot::new(Default::default());
        snapshot.status = "Demo: la reproducción de audio está desactivada".into();
        let ids = Generations {
            notice: 2,
            ..Default::default()
        };
        apply_output_result(&mut snapshot, &ids, 1, Ok("Demo: copiado simulado"));
        assert!(snapshot.status.contains("reproducción de audio"));
        apply_output_result(&mut snapshot, &ids, 1, Err("No se pudo copiar".into()));
        assert_eq!(snapshot.error.as_deref(), Some("No se pudo copiar"));
        apply_output_result(&mut snapshot, &ids, 2, Ok("Demo: copiado simulado"));
        assert_eq!(snapshot.status, "Demo: copiado simulado");
    }
    #[test]
    fn stale_output_is_rejected_after_cancel_or_edit() {
        let mut g = Generations::default();
        let id = g.new_session();
        let rev = g.text;
        assert!(g.accepts_output(id, rev));
        g.text += 1;
        assert!(!g.accepts_output(id, rev));
        g.new_session();
        assert!(!g.accepts_output(id, g.text));
    }
    #[test]
    fn api_key_saved_for_inactive_provider_does_not_authenticate_subscription() {
        let mut config = AppConfig::default();
        config.provider = ProviderMode::Subscription;
        config.tts.provider = ProviderMode::Subscription;
        let mut snapshot = AppSnapshot::new(config);
        apply_auth_result(&mut snapshot, ProviderMode::ApiKey, Ok(true));
        assert!(!snapshot.authenticated);
        assert!(snapshot.status.contains("seleccioná API"));
        snapshot.config.provider = ProviderMode::ApiKey;
        apply_auth_result(&mut snapshot, ProviderMode::ApiKey, Ok(true));
        assert!(snapshot.authenticated);
        apply_auth_result(&mut snapshot, ProviderMode::ApiKey, Ok(false));
        assert!(!snapshot.authenticated);
    }
    #[derive(Default)]
    struct TestSecretStore {
        calls: Mutex<Vec<&'static str>>,
    }
    impl dicta_provider::SecretStore for TestSecretStore {
        fn get(&self, _: &str) -> dicta_provider::Result<Option<Secret>> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &Secret) -> dicta_provider::Result<()> {
            self.calls.lock().unwrap().push("save");
            Ok(())
        }
        fn delete(&self, _: &str) -> dicta_provider::Result<()> {
            self.calls.lock().unwrap().push("delete");
            Ok(())
        }
    }
    #[tokio::test]
    async fn credential_writes_preserve_clear_order_and_skip_cancelled_login() {
        let store = Arc::new(TestSecretStore::default());
        let credentials = Credentials::with_store(store.clone());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (events, _) = mpsc::channel();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        tx.send(CredentialWrite::ApiKey(
            1,
            Secret::new("cancelled-test-key"),
            cancelled,
        ))
        .unwrap();
        tx.send(CredentialWrite::ApiKey(
            2,
            Secret::new("test-key"),
            CancellationToken::new(),
        ))
        .unwrap();
        tx.send(CredentialWrite::Clear(3, ProviderMode::ApiKey))
            .unwrap();
        tx.send(CredentialWrite::ApiKey(
            4,
            Secret::new("replacement-test-key"),
            CancellationToken::new(),
        ))
        .unwrap();
        drop(tx);
        credential_writer(credentials, rx, events).await;
        assert_eq!(*store.calls.lock().unwrap(), ["save", "delete", "save"]);
    }
    #[test]
    fn queued_output_is_discarded_after_cancellation() {
        let runtime = Runtime::new().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut snapshot = AppSnapshot::new(AppConfig::default());
        snapshot.final_text = "demo".into();
        let control = OutputControl::default();
        let guard = control.gate.lock().unwrap();
        output_text(
            &runtime,
            &tx,
            &snapshot,
            &Generations::default(),
            &control,
            None,
            false,
            true,
        );
        control.invalidate();
        drop(guard);
        drop(tx);
        runtime.shutdown_timeout(Duration::from_secs(1));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn demo_controller_handles_cancel_ready_edit_and_settings_without_io() {
        let (tx, commands) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let paths = AppPaths {
            config: "/nonexistent/dicta-test-config".into(),
            data: "/nonexistent/dicta-test-data".into(),
            runtime: "/nonexistent/dicta-test-runtime".into(),
        };
        let mut initial = AppSnapshot::new(AppConfig::default());
        initial.demo = true;
        let controller_tx = tx.clone();
        let worker = std::thread::spawn(move || {
            run(
                paths,
                initial,
                commands,
                controller_tx,
                snapshot_tx,
                Options {
                    demo: true,
                    ..Default::default()
                },
            )
            .unwrap()
        });
        struct Stopper {
            tx: Sender<Command>,
            worker: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for Stopper {
            fn drop(&mut self) {
                let _ = self.tx.send(Command::Quit);
                if let Some(worker) = self.worker.take() {
                    worker.join().unwrap();
                }
            }
        }
        let _stopper = Stopper {
            tx: tx.clone(),
            worker: Some(worker),
        };
        let until = |predicate: &dyn Fn(&AppSnapshot) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let next = snapshot_rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("snapshot esperado");
                if predicate(&next) {
                    break next;
                }
            }
        };
        tx.send(Command::Start).unwrap();
        until(&|s| s.phase == Phase::Recording);
        tx.send(Command::Paste).unwrap();
        until(&|s| s.status.contains("Terminá el dictado"));
        tx.send(Command::Cancel).unwrap();
        let idle = until(&|s| s.phase == Phase::Idle);
        assert!(idle.final_text.is_empty());
        tx.send(Command::Start).unwrap();
        until(&|s| s.phase == Phase::Recording);
        tx.send(Command::Stop).unwrap();
        let ready = until(&|s| s.phase == Phase::Ready);
        assert!(!ready.final_text.is_empty());
        assert!(!ready.authenticated);
        tx.send(Command::EditText("Corrección de demo".into()))
            .unwrap();
        until(&|s| s.final_text == "Corrección de demo");
        tx.send(Command::Copy).unwrap();
        until(&|s| s.status == "Demo: copiado simulado");
        // Queue both commands immediately after the previous publication. A
        // coalesced snapshot must not swallow the hidden edge the UI relies on.
        tx.send(Command::HideSettings).unwrap();
        tx.send(Command::ShowSettings).unwrap();
        let hidden = until(&|s| !s.show_settings);
        let shown = until(&|s| s.show_settings);
        assert!(shown.revision > hidden.revision);
        tx.send(Command::SetTheme(dicta_core::Theme::Night))
            .unwrap();
        until(&|s| {
            s.config.theme == dicta_core::Theme::Signal
                && s.config.color_mode == dicta_core::ColorMode::Dark
        });
        let mut config = AppConfig {
            shortcut_mode: dicta_core::ShortcutMode::Hold,
            ..Default::default()
        };
        tx.send(Command::UpdateConfig(config.clone())).unwrap();
        until(&|s| s.config.shortcut_mode == dicta_core::ShortcutMode::Hold);
        tx.send(Command::ShortcutPressed {
            at: 100,
            token: Some(1),
        })
        .unwrap();
        tx.send(Command::ShortcutReleased {
            at: 101,
            token: Some(1),
        })
        .unwrap();
        until(&|s| s.phase == Phase::Ready && s.status == "Demo: copiado simulado");
        // A release from an unrelated gesture never stops a UI-started job.
        tx.send(Command::Start).unwrap();
        until(&|s| s.phase == Phase::Recording);
        tx.send(Command::ShortcutReleased {
            at: 102,
            token: Some(1),
        })
        .unwrap();
        until(&|s| s.phase == Phase::Recording && s.elapsed_ms > 100);
        tx.send(Command::Cancel).unwrap();
        until(&|s| s.phase == Phase::Idle);
        config.copy_on_finish = false;
        tx.send(Command::UpdateConfig(config.clone())).unwrap();
        until(&|s| !s.config.copy_on_finish);
        tx.send(Command::ShortcutPressed {
            at: 103,
            token: Some(2),
        })
        .unwrap();
        until(&|s| s.phase == Phase::Recording);
        config.shortcut_mode = dicta_core::ShortcutMode::Toggle;
        tx.send(Command::UpdateConfig(config)).unwrap();
        let ready = until(&|s| s.phase == Phase::Ready);
        assert_ne!(ready.status, "Demo: copiado simulado");
        // Mode change ends the owned hold even before its physical release.
        tx.send(Command::ShortcutReleased {
            at: 104,
            token: Some(2),
        })
        .unwrap();
    }
    #[tokio::test]
    async fn cancelled_demo_cannot_finish() {
        let c = CancellationToken::new();
        c.cancel();
        let (_stop, rx) = watch::channel(false);
        let (tx, mut events) = tokio::sync::mpsc::channel(32);
        demo_session(rx, c, tx).await;
        while let Some(event) = events.recv().await {
            assert!(!matches!(event, SessionEvent::Finished { .. }));
        }
    }
}
