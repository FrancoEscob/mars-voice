//! End-to-end controller commands using its public demo mode. No microphone,
//! provider, keyring requests, clipboard or real audio files are used.
use dicta::controller::{self, Options};
use dicta_core::{
    AppConfig, AppPaths, AppSnapshot, AudioRetention, Command, NoteStatus, Phase, UsagePeriod,
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Demo {
    root: PathBuf,
    commands: Sender<Command>,
    snapshots: Receiver<AppSnapshot>,
    worker: Option<JoinHandle<anyhow::Result<()>>>,
    current: AppSnapshot,
}
impl Demo {
    fn new(config: AppConfig, notes_limit: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "mars-history-controller-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let paths = AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            runtime: root.join("runtime"),
        };
        let (tx, rx) = mpsc::channel();
        let (snap_tx, snap_rx) = mpsc::channel();
        let mut initial = AppSnapshot::new(config);
        initial.demo = true;
        initial.notes_limit = notes_limit;
        let current = initial.clone();
        let thread_tx = tx.clone();
        let worker = std::thread::spawn(move || {
            controller::run(
                paths,
                initial,
                rx,
                thread_tx,
                snap_tx,
                Options {
                    demo: true,
                    portal: false,
                    allow_env_key: false,
                    no_tray: true,
                },
            )
        });
        let mut demo = Self {
            root,
            commands: tx,
            snapshots: snap_rx,
            worker: Some(worker),
            current,
        };
        demo.until(|s| s.revision > 0);
        demo
    }
    fn until(&mut self, predicate: impl Fn(&AppSnapshot) -> bool) -> AppSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = self
                .snapshots
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("expected demo snapshot before timeout");
            let matches = predicate(&snapshot);
            self.current = snapshot;
            if matches {
                return self.current.clone();
            }
        }
    }
    fn command(
        &mut self,
        command: Command,
        predicate: impl Fn(&AppSnapshot) -> bool,
    ) -> AppSnapshot {
        let before = self.current.revision;
        self.commands.send(command).unwrap();
        self.until(|s| s.revision > before && predicate(s))
    }
    fn dictate(&mut self) -> AppSnapshot {
        self.command(Command::Start, |s| s.phase == Phase::Recording);
        self.command(Command::Stop, |s| s.phase == Phase::Ready)
    }
    fn finish(mut self) {
        self.commands.send(Command::Quit).unwrap();
        self.worker.take().unwrap().join().unwrap().unwrap();
        assert_eq!(
            fs::read_dir(&self.root).unwrap().count(),
            0,
            "demo must not write config, history, audio or runtime files"
        );
    }
}
impl Drop for Demo {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn completed_note_metrics_edit_copy_and_clear_preserve_the_right_state() {
    let mut app = Demo::new(AppConfig::default(), 100);
    let ready = app.dictate();
    assert_eq!(ready.notes.len(), 1);
    let first = ready.notes[0].clone();
    assert_eq!(first.status, NoteStatus::Completed);
    assert_eq!(first.text, ready.final_text);
    assert_eq!(first.original, ready.original_text);
    assert_eq!(ready.metrics.dictations, 1);
    assert!(ready.metrics.words > 0);
    assert!(ready.metrics.captured_ms > 0 && ready.metrics.sent_ms > 0);
    assert!(
        (ready.metrics.estimated_stt_usd - ready.metrics.sent_ms as f64 / 3_600_000.0 * 0.20).abs()
            < 1e-12
    );
    assert_eq!(ready.metrics.input_tokens, None);
    assert_eq!(ready.metrics.output_tokens, None);
    assert_eq!(first.audio_filename, None);
    assert_eq!(first.audio_bytes, 0);
    assert_eq!(ready.metrics.audio_bytes, 0);

    let edited = app.command(
        Command::EditText("Una corrección para Mars voice.".into()),
        |s| s.final_text == "Una corrección para Mars voice.",
    );
    assert_eq!(edited.notes[0].text, edited.final_text);
    assert_eq!(edited.notes[0].original, first.original);
    assert_eq!(edited.notes[0].word_count, 5);
    assert_eq!(edited.metrics.words, 5);
    app.command(
        Command::RememberCorrection {
            from: "hiper land".into(),
            to: "Hyprland".into(),
        },
        |s| s.corrections.len() == 1,
    );
    let second = app.dictate();
    assert_eq!(second.notes.len(), 2);
    let current_overlay_text = second.final_text;
    app.command(Command::OpenNote(first.id), |s| {
        s.active_note_id == Some(first.id)
    });
    let copied = app.command(Command::CopyNote(first.id), |s| {
        s.status == "Demo: copiado simulado"
    });
    assert_eq!(
        copied.final_text, current_overlay_text,
        "copying history must not replace current overlay text"
    );
    assert_eq!(
        copied.selected_note.as_ref().unwrap().text,
        "Una corrección para Mars voice."
    );
    let no_audio = app.command(Command::DeleteNoteAudio(first.id), |s| s.notes.len() == 2);
    assert!(
        no_audio
            .notes
            .iter()
            .all(|n| n.audio_filename.is_none() && n.audio_bytes == 0)
    );
    let deleted = app.command(Command::DeleteNote(first.id), |s| s.notes.len() == 1);
    assert_eq!(deleted.active_note_id, None);
    assert!(deleted.selected_note.is_none());
    assert_eq!(deleted.metrics.dictations, 1);
    let cleared = app.command(Command::ClearHistory, |s| s.notes.is_empty());
    assert_eq!(cleared.metrics.dictations, 0);
    assert_eq!(cleared.metrics.input_tokens, None);
    assert_eq!(cleared.corrections.len(), 1);
    app.finish();
}

#[test]
fn pagination_filter_and_period_do_not_hide_the_selected_note_or_filter_metrics() {
    let mut app = Demo::new(AppConfig::default(), 1);
    let first = app.dictate().notes[0].id;
    app.command(Command::EditText("Órbita, dato único.".into()), |s| {
        s.final_text.starts_with("Órbita")
    });
    let second = app.dictate();
    assert_eq!(second.notes.len(), 1);
    assert_ne!(second.notes[0].id, first);
    assert!(second.notes_has_more);
    let more = app.command(Command::LoadMoreNotes, |s| s.notes_limit == 101);
    assert_eq!(more.notes.len(), 2);
    assert!(!more.notes_has_more);
    let opened = app.command(Command::OpenNote(first), |s| {
        s.selected_note.as_ref().is_some_and(|n| n.id == first)
    });
    assert!(
        Arc::ptr_eq(&more.notes, &opened.notes),
        "opening a note must not reload the list"
    );
    let found = app.command(Command::SearchNotes("ÓRBITA".into()), |s| {
        s.history_query == "ÓRBITA"
    });
    assert_eq!(found.notes_limit, 100);
    assert_eq!(found.notes.len(), 1);
    assert_eq!(found.notes[0].id, first);
    assert_eq!(found.metrics.dictations, 2);
    let filtered = app.command(Command::SearchNotes("z".repeat(600)), |s| {
        s.history_query.len() == 500
    });
    assert!(filtered.notes.is_empty());
    assert_eq!(filtered.selected_note.as_ref().unwrap().id, first);
    assert_eq!(filtered.metrics.dictations, 2);
    for period in [UsagePeriod::Today, UsagePeriod::All, UsagePeriod::Week] {
        let before_notes = app.current.notes.clone();
        let selected = app.command(Command::SetUsagePeriod(period), |s| {
            s.usage_period == period
        });
        assert_eq!(selected.metrics.dictations, 2);
        assert!(selected.notes.is_empty());
        assert!(
            Arc::ptr_eq(&before_notes, &selected.notes),
            "changing usage period must not rerun a search"
        );
        assert_eq!(selected.selected_note.as_ref().unwrap().id, first);
    }
    let missing = app.command(Command::OpenNote(i64::MAX), |s| s.selected_note.is_none());
    assert_eq!(missing.active_note_id, None);
    app.finish();
}

#[test]
fn disabled_history_and_disabling_during_recording_do_not_store_new_notes() {
    let config = AppConfig {
        history_enabled: false,
        audio_retention: AudioRetention::Forever,
        ..Default::default()
    };
    let mut app = Demo::new(config, 100);
    let first = app.dictate();
    assert!(first.notes.is_empty());
    assert_eq!(first.metrics.dictations, 0);
    assert!(!first.final_text.is_empty());
    let mut enabled = first.config.clone();
    enabled.history_enabled = true;
    enabled.audio_retention = AudioRetention::Off;
    app.command(Command::UpdateConfig(enabled), |s| s.config.history_enabled);
    let saved = app.dictate();
    assert_eq!(saved.notes.len(), 1);
    app.command(Command::Start, |s| s.phase == Phase::Recording);
    let mut disabled = saved.config.clone();
    disabled.history_enabled = false;
    app.command(Command::UpdateConfig(disabled), |s| {
        !s.config.history_enabled
    });
    let stopped = app.command(Command::Stop, |s| s.phase == Phase::Ready);
    assert_eq!(
        stopped.notes.len(),
        1,
        "previous notes remain; current recording is not saved"
    );
    assert_eq!(stopped.metrics.dictations, 1);
    assert!(
        stopped
            .notes
            .iter()
            .all(|n| n.audio_filename.is_none() && n.audio_bytes == 0)
    );
    app.finish();
}

#[test]
fn cancelled_recording_cannot_create_a_late_note_and_demo_playback_stays_disabled() {
    let mut app = Demo::new(AppConfig::default(), 100);
    app.command(Command::Start, |s| s.phase == Phase::Recording);
    app.until(|s| !s.partial.is_empty());
    let cancelled = app.command(Command::Cancel, |s| s.phase == Phase::Idle);
    assert!(cancelled.notes.is_empty());
    let deadline = Instant::now() + Duration::from_millis(450);
    while let Ok(snapshot) = app
        .snapshots
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        assert!(snapshot.notes.is_empty());
        assert_ne!(snapshot.phase, Phase::Ready);
        app.current = snapshot;
    }
    let ready = app.dictate();
    assert_eq!(ready.notes.len(), 1);
    let id = ready.notes[0].id;
    let playback = app.command(Command::PlayNoteAudio(id), |s| {
        s.status.contains("reproducción de audio está desactivada")
    });
    assert_eq!(playback.audio_playing_note_id, None);
    let stopped = app.command(Command::StopAudioPlayback, |s| {
        s.audio_playing_note_id.is_none()
    });
    assert_eq!(stopped.notes.len(), 1);
    app.finish();
}
