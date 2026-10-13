//! Private note storage and user-triggered local playback. Never captures audio.
use anyhow::{Context, Result, ensure};
use dicta_core::{
    AppPaths, AppSnapshot, NewNote, NoteStatus, ProviderMode, SessionArtifacts, TtsConfig, fsx,
    store::Store,
};
use dicta_platform::playback::{Player, PlayerConfig};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn refresh(store: &Store, snap: &mut AppSnapshot) -> Result<()> {
    refresh_notes(store, snap)?;
    refresh_metrics(store, snap)?;
    refresh_selection(store, snap)
}
/// Search and pagination do not change stored usage or the selected note.
/// Dictations and readings share the query and page size.
pub fn refresh_notes(store: &Store, snap: &mut AppSnapshot) -> Result<()> {
    let mut notes = store.notes(&snap.history_query, snap.notes_limit.saturating_add(1))?;
    snap.notes_has_more = notes.len() > snap.notes_limit;
    notes.truncate(snap.notes_limit);
    snap.notes = notes.into();
    let mut readings = store.readings(&snap.history_query, snap.notes_limit.saturating_add(1))?;
    snap.readings_has_more = readings.len() > snap.notes_limit;
    readings.truncate(snap.notes_limit);
    snap.readings = readings.into();
    Ok(())
}
pub fn refresh_metrics(store: &Store, snap: &mut AppSnapshot) -> Result<()> {
    snap.metrics = store.metrics(snap.usage_period, now_ms())?;
    snap.reading_metrics = store.reading_metrics(snap.usage_period, now_ms())?;
    Ok(())
}
pub fn refresh_selection(store: &Store, snap: &mut AppSnapshot) -> Result<()> {
    snap.selected_note = match snap.active_note_id {
        Some(id) => store.note(id)?,
        None => None,
    };
    if snap.selected_note.is_none() {
        snap.active_note_id = None;
    }
    snap.selected_reading = match snap.active_reading_id {
        Some(id) => store.reading(id)?,
        None => None,
    };
    if snap.selected_reading.is_none() {
        snap.active_reading_id = None;
    }
    Ok(())
}
fn audio_path(paths: &AppPaths, name: &str) -> Result<PathBuf> {
    ensure!(
        dicta_core::history::valid_audio_filename(name),
        "Nombre de audio inválido"
    );
    let dir = paths.data.join("audio");
    let metadata = fs::symlink_metadata(&dir).context("No se encontró la carpeta de audio")?;
    ensure!(
        metadata.is_dir() && !fsx::is_link(&metadata),
        "La carpeta de audio no es válida"
    );
    Ok(dir.join(name))
}
pub fn remove_audio(paths: &AppPaths, name: &str) -> Result<()> {
    ensure!(
        dicta_core::history::valid_audio_filename(name),
        "Nombre de audio inválido"
    );
    if matches!(fs::symlink_metadata(paths.data.join("audio")),Err(e) if e.kind()==std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    let path = audio_path(paths, name)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("No se pudo borrar el audio local"),
    }
}
pub fn remove_many(paths: &AppPaths, names: impl IntoIterator<Item = String>) -> Result<()> {
    let mut first = None;
    for name in names {
        if let Err(e) = remove_audio(paths, &name) {
            if first.is_none() {
                first = Some(e)
            }
        }
    }
    match first {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
pub fn prune(store: &Store, paths: &AppPaths, snap: &mut AppSnapshot, demo: bool) -> Result<()> {
    let expired = store.expired_audio(snap.config.audio_retention, now_ms())?;
    let mut failure = None;
    // Lecturas: su propia retención; el texto se conserva.
    for (id, name) in store.expired_reading_audio(snap.config.tts.audio_retention, now_ms())? {
        let removed = if demo {
            Ok(())
        } else {
            remove_audio(paths, &name)
        };
        if let Err(e) = removed.and_then(|_| store.delete_reading_audio(id).map(|_| ())) {
            failure.get_or_insert(e);
        }
    }
    for (id, name) in expired {
        let removed = if demo {
            Ok(())
        } else {
            remove_audio(paths, &name)
        };
        match removed.and_then(|_| store.delete_note_audio(id).map(|_| ())) {
            Ok(()) => {}
            Err(e) => {
                if failure.is_none() {
                    failure = Some(e);
                }
            }
        }
    }
    refresh(store, snap)?;
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
pub fn delete(
    store: &Store,
    paths: &AppPaths,
    id: i64,
    whole_note: bool,
    demo: bool,
) -> Result<()> {
    if let Some(note) = store.note(id)? {
        if !demo {
            if let Some(name) = note.audio_filename {
                remove_audio(paths, &name)?;
            }
        }
        if whole_note {
            store.delete_note(id)?;
        } else {
            store.delete_note_audio(id)?;
        }
    }
    Ok(())
}
/// Borra una lectura o solo su audio. El archivo se borra antes que la referencia.
pub fn delete_reading(
    store: &Store,
    paths: &AppPaths,
    id: i64,
    whole: bool,
    demo: bool,
) -> Result<()> {
    if let Some(reading) = store.reading(id)? {
        if !demo {
            if let Some(name) = reading.audio_filename {
                remove_audio(paths, &name)?;
            }
        }
        if whole {
            store.delete_reading(id)?;
        } else {
            store.delete_reading_audio(id)?;
        }
    }
    Ok(())
}
pub fn clear_readings(store: &Store, paths: &AppPaths, demo: bool) -> Result<()> {
    if !demo {
        remove_many(paths, store.reading_audio_files()?)?;
    }
    store.clear_readings()?;
    Ok(())
}
pub fn clear(store: &Store, paths: &AppPaths, demo: bool) -> Result<()> {
    if !demo {
        remove_many(paths, store.audio_files()?)?;
    }
    store.clear_history()?;
    Ok(())
}
pub fn save_note(
    store: &Store,
    paths: &AppPaths,
    snap: &mut AppSnapshot,
    created_at_ms: u64,
    provider: ProviderMode,
    enabled: bool,
    original: String,
    text: String,
    status: NoteStatus,
    error: Option<String>,
    mut artifacts: SessionArtifacts,
    demo: bool,
) -> Result<Option<i64>> {
    // Turning history off during a session also discards that session's archive.
    if !enabled
        || !snap.config.history_enabled
        || (text.trim().is_empty() && artifacts.captured_ms == 0)
    {
        if !demo {
            if let Some(name) = artifacts.audio_filename {
                remove_audio(paths, &name)?;
            }
        }
        return Ok(None);
    }
    // A privacy preference changed mid-session wins over its initial setting.
    if snap.config.audio_retention == dicta_core::AudioRetention::Off {
        if let Some(name) = artifacts.audio_filename.take() {
            if !demo {
                remove_audio(paths, &name)?;
            }
            artifacts.audio_bytes = 0;
        }
    }
    let audio_name = artifacts.audio_filename.clone();
    let result = store.record_note(NewNote {
        created_at_ms,
        original,
        text,
        captured_ms: artifacts.captured_ms,
        sent_ms: artifacts.sent_ms,
        provider,
        status,
        error,
        audio_filename: artifacts.audio_filename,
        audio_bytes: artifacts.audio_bytes,
        input_tokens: artifacts.input_tokens,
        output_tokens: artifacts.output_tokens,
    });
    match result {
        Ok(id) => {
            refresh(store, snap)?;
            Ok(Some(id))
        }
        Err(e) => {
            if !demo {
                if let Some(name) = audio_name {
                    let _ = remove_audio(paths, &name);
                }
            }
            Err(e).context("No se pudo guardar la nota; el texto sigue en el overlay")
        }
    }
}
/// In-process playback of a saved note through the read-aloud output.
#[derive(Default)]
pub struct Playback {
    player: Option<Player>,
    note: Option<i64>,
}
impl Playback {
    pub fn stop(&mut self) {
        // Dropping the player stops the stream and joins its thread.
        self.player = None;
        self.note = None;
    }
    /// Reads the saved WAV on this thread (notes are small; the reader is
    /// bounded by the validated PCM16 mono layout) and plays it at normal speed
    /// on the read-aloud output device and volume.
    pub fn play(&mut self, paths: &AppPaths, tts: &TtsConfig, id: i64, name: &str) -> Result<()> {
        self.stop();
        let (pcm, sample_rate) =
            crate::wav::read_history_wav(&paths.data, name).map_err(anyhow::Error::msg)?;
        let player = Player::new(PlayerConfig {
            sample_rate,
            device: tts.output_device.clone(),
            volume: tts.volume,
            rate: 1.0,
        })
        .context("No se pudo reproducir el audio guardado")?;
        player.append(&pcm);
        player.set_finished(true);
        player.play();
        self.player = Some(player);
        self.note = Some(id);
        Ok(())
    }
    pub fn poll(&mut self) -> Result<Option<i64>> {
        if let Some(player) = &self.player {
            let status = player.status();
            if status.error.is_some() {
                self.stop();
                anyhow::bail!("No se pudo reproducir el audio guardado");
            }
            if status.ended {
                self.stop();
            }
        }
        Ok(self.note)
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "synthetic performance measurement, run alone with --nocapture"]
    fn history_refresh_workload() {
        use std::{hint::black_box, time::Instant};
        let store = Store::in_memory().unwrap();
        let at = now_ms().saturating_sub(1000);
        let text = "Órbita sintética para medir la copia del historial. ".repeat(160);
        let mut last = None;
        for index in 0..10_000 {
            last = Some(
                store
                    .record_note(NewNote {
                        created_at_ms: at + index / 100,
                        original: text.clone(),
                        text: text.clone(),
                        captured_ms: 1000,
                        sent_ms: 1000,
                        provider: ProviderMode::Subscription,
                        status: NoteStatus::Completed,
                        error: None,
                        audio_filename: None,
                        audio_bytes: 0,
                        input_tokens: None,
                        output_tokens: None,
                    })
                    .unwrap(),
            );
        }
        let mut snap = AppSnapshot::new(Default::default());
        snap.active_note_id = last;
        refresh(&store, &mut snap).unwrap();
        for (name, operation) in [
            (
                "history_full_refresh",
                refresh as fn(&Store, &mut AppSnapshot) -> Result<()>,
            ),
            ("history_open_note", refresh_selection),
            ("history_list_refresh", refresh_notes),
            ("history_period_refresh", refresh_metrics),
        ] {
            let start = Instant::now();
            for _ in 0..100 {
                operation(&store, &mut snap).unwrap();
                black_box(&snap);
            }
            println!(
                "PERF {name} iterations=100 us_each={:.2}",
                start.elapsed().as_micros() as f64 / 100.0
            );
        }
    }

    #[test]
    fn audio_names_cannot_escape_private_directory() {
        let paths = AppPaths {
            config: "/unused".into(),
            data: "/unused".into(),
            runtime: "/unused".into(),
        };
        for name in [
            "../secret.wav",
            "/tmp/file.wav",
            "a/b.wav",
            ".hidden.wav",
            "a.wav;echo",
            "",
        ] {
            assert!(audio_path(&paths, name).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn symlink_audio_directory_is_rejected() {
        let root = std::env::temp_dir().join(format!("mars-history-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink("/tmp", root.join("audio")).unwrap();
        let paths = AppPaths {
            config: root.clone(),
            data: root.clone(),
            runtime: root.clone(),
        };
        assert!(audio_path(&paths, "note.wav").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn playback_rejects_invalid_audio_before_opening_an_output() {
        let (root, paths, _store) = fixture();
        let tts = TtsConfig::default();
        let mut playback = Playback::default();
        fs::write(root.join("audio/audio-test.wav"), b"not a wav").unwrap();
        for name in ["audio-test.wav", "missing.wav", "../audio-test.wav"] {
            assert!(playback.play(&paths, &tts, 1, name).is_err(), "{name}");
            assert!(playback.player.is_none());
            assert_eq!(playback.poll().unwrap(), None);
        }
        #[cfg(unix)]
        {
            crate::wav::write_wav_file(
                &root.join("real.wav"),
                &[std::sync::Arc::new(vec![1i16; 16])],
                16_000,
                0o600,
            )
            .unwrap();
            std::os::unix::fs::symlink(root.join("real.wav"), root.join("audio/linked.wav"))
                .unwrap();
            assert!(playback.play(&paths, &tts, 1, "linked.wav").is_err());
            assert!(playback.player.is_none());
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture() -> (std::path::PathBuf, AppPaths, Store) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "mars-history-fs-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("audio")).unwrap();
        let paths = AppPaths {
            config: root.clone(),
            data: root.clone(),
            runtime: root.clone(),
        };
        (root, paths, Store::in_memory().unwrap())
    }
    fn audio_note(store: &Store) -> i64 {
        store
            .record_note(NewNote {
                created_at_ms: 1,
                original: "Prueba".into(),
                text: "Prueba".into(),
                captured_ms: 1000,
                sent_ms: 1000,
                provider: ProviderMode::Subscription,
                status: NoteStatus::Completed,
                error: None,
                audio_filename: Some("audio-test.wav".into()),
                audio_bytes: 32044,
                input_tokens: None,
                output_tokens: None,
            })
            .unwrap()
    }
    #[test]
    fn file_delete_failure_preserves_note_and_audio_reference_for_retry() {
        let (root, paths, store) = fixture();
        let id = audio_note(&store);
        fs::create_dir(root.join("audio/audio-test.wav")).unwrap();
        assert!(delete(&store, &paths, id, true, false).is_err());
        assert_eq!(
            store.note(id).unwrap().unwrap().audio_filename.as_deref(),
            Some("audio-test.wav")
        );
        assert!(clear(&store, &paths, false).is_err());
        assert!(store.note(id).unwrap().is_some());
        fs::remove_dir(root.join("audio/audio-test.wav")).unwrap();
        delete(&store, &paths, id, true, false).unwrap();
        assert!(store.note(id).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn expiration_retries_file_failures_without_losing_reference() {
        let (root, paths, store) = fixture();
        let id = audio_note(&store);
        fs::create_dir(root.join("audio/audio-test.wav")).unwrap();
        let mut snap = AppSnapshot::new(dicta_core::AppConfig {
            audio_retention: dicta_core::AudioRetention::Days7,
            ..Default::default()
        });
        assert!(prune(&store, &paths, &mut snap, false).is_err());
        assert!(store.note(id).unwrap().unwrap().audio_filename.is_some());
        fs::remove_dir(root.join("audio/audio-test.wav")).unwrap();
        prune(&store, &paths, &mut snap, false).unwrap();
        assert!(store.note(id).unwrap().unwrap().audio_filename.is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn disabled_history_discards_completed_archive() {
        let (root, paths, store) = fixture();
        fs::write(root.join("audio/audio-test.wav"), b"synthetic").unwrap();
        let mut snap = AppSnapshot::new(dicta_core::AppConfig {
            history_enabled: false,
            ..Default::default()
        });
        let saved = save_note(
            &store,
            &paths,
            &mut snap,
            1,
            ProviderMode::Subscription,
            true,
            "x".into(),
            "x".into(),
            NoteStatus::Completed,
            None,
            SessionArtifacts {
                captured_ms: 100,
                audio_filename: Some("audio-test.wav".into()),
                ..Default::default()
            },
            false,
        )
        .unwrap();
        assert!(saved.is_none());
        assert!(!root.join("audio/audio-test.wav").exists());
        assert!(store.notes("", 10).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_audio_directory_does_not_block_history_delete() {
        let (root, paths, store) = fixture();
        let id = audio_note(&store);
        fs::remove_dir(root.join("audio")).unwrap();
        delete(&store, &paths, id, true, false).unwrap();
        assert!(store.note(id).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn turning_audio_off_during_session_preserves_note_but_discards_archive() {
        let (root, paths, store) = fixture();
        fs::write(root.join("audio/audio-test.wav"), b"synthetic").unwrap();
        let mut snap = AppSnapshot::new(Default::default());
        let id = save_note(
            &store,
            &paths,
            &mut snap,
            now_ms(),
            ProviderMode::Subscription,
            true,
            "x".into(),
            "x".into(),
            NoteStatus::Completed,
            None,
            SessionArtifacts {
                captured_ms: 100,
                audio_filename: Some("audio-test.wav".into()),
                audio_bytes: 10,
                ..Default::default()
            },
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(store.note(id).unwrap().unwrap().text, "x");
        assert!(store.note(id).unwrap().unwrap().audio_filename.is_none());
        assert!(!root.join("audio/audio-test.wav").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
