use crate::ProviderMode;
use serde::{Deserialize, Serialize};

/// Public list prices, not billed subscription credits. Verified 2026-10-09:
/// https://docs.x.ai/developers/models/speech-to-text
pub const STT_USD_PER_HOUR: f64 = 0.20;
pub const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsagePeriod {
    Today,
    #[default]
    Week,
    All,
}
impl UsagePeriod {
    /// Today starts at local midnight; Week is the last seven days.
    pub fn since_ms(self, now_ms: u64) -> u64 {
        match self {
            Self::Today => day_start_ms(now_ms, &chrono::Local),
            Self::Week => now_ms.saturating_sub(7 * DAY_MS),
            Self::All => 0,
        }
    }
}
/// Resolve local midnight, including zones whose DST transition skips midnight.
/// Injecting the timezone lets tests cover boundaries without mutating process TZ.
fn day_start_ms<T: chrono::TimeZone>(now_ms: u64, zone: &T) -> u64 {
    let Some(now) = i64::try_from(now_ms)
        .ok()
        .and_then(|ms| zone.timestamp_millis_opt(ms).single())
    else {
        return now_ms / DAY_MS * DAY_MS;
    };
    let midnight = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("valid midnight");
    for minute in 0..1440 {
        if let Some(start) = zone
            .from_local_datetime(&(midnight + chrono::Duration::minutes(minute)))
            .earliest()
        {
            return u64::try_from(start.timestamp_millis()).unwrap_or(0);
        }
    }
    now_ms / DAY_MS * DAY_MS
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    Completed,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: i64,
    pub created_at_ms: u64,
    pub original: String,
    pub text: String,
    pub word_count: u64,
    pub captured_ms: u64,
    pub sent_ms: u64,
    pub provider: ProviderMode,
    pub status: NoteStatus,
    pub error: Option<String>,
    pub audio_filename: Option<String>,
    pub audio_bytes: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}
#[derive(Debug, Clone)]
pub struct NewNote {
    pub created_at_ms: u64,
    pub original: String,
    pub text: String,
    pub captured_ms: u64,
    pub sent_ms: u64,
    pub provider: ProviderMode,
    pub status: NoteStatus,
    pub error: Option<String>,
    pub audio_filename: Option<String>,
    pub audio_bytes: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}
#[derive(Debug, Clone, Default)]
pub struct SessionArtifacts {
    pub captured_ms: u64,
    pub sent_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub audio_filename: Option<String>,
    pub audio_bytes: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryMetrics {
    /// Stored attempts, including failures with partial text or sent audio.
    pub dictations: u64,
    pub words: u64,
    pub captured_ms: u64,
    pub sent_ms: u64,
    pub estimated_stt_usd: f64,
    /// None when usage is unknown for any included note, or there are no notes.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub audio_bytes: u64,
}

/// Count whitespace-delimited words, excluding punctuation-only tokens.
/// Apostrophes, technical names and hyphenated words remain one word.
pub fn count_words(text: &str) -> u64 {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count() as u64
}

/// Storage only accepts generated ASCII basenames inside the audio directory.
/// Callers must still reject symlinks when opening/deleting the actual file.
pub fn valid_audio_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 160
        && !name.starts_with('.')
        && name.ends_with(".wav")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && !name.contains("..")
}

use crate::{AudioRetention, store::Store};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
const NOTE_COLUMNS: &str = "id,created_at_ms,original_text,final_text,word_count,captured_ms,sent_ms,provider,status,error,audio_filename,audio_bytes,input_tokens,output_tokens";
fn sql_u64(value: u64) -> Result<i64> {
    i64::try_from(value).context("Valor de historial fuera de rango")
}
fn read_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn read_optional_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| {
            u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
        })
        .transpose()
}
fn read_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: row.get(0)?,
        created_at_ms: read_u64(row, 1)?,
        original: row.get(2)?,
        text: row.get(3)?,
        word_count: read_u64(row, 4)?,
        captured_ms: read_u64(row, 5)?,
        sent_ms: read_u64(row, 6)?,
        provider: match row.get::<_, String>(7)?.as_str() {
            "api_key" => ProviderMode::ApiKey,
            _ => ProviderMode::Subscription,
        },
        status: match row.get::<_, String>(8)?.as_str() {
            "failed" => NoteStatus::Failed,
            _ => NoteStatus::Completed,
        },
        error: row.get(9)?,
        audio_filename: row.get(10)?,
        audio_bytes: read_u64(row, 11)?,
        input_tokens: read_optional_u64(row, 12)?,
        output_tokens: read_optional_u64(row, 13)?,
    })
}
impl Store {
    pub fn record_note(&self, note: NewNote) -> Result<i64> {
        if let Some(name) = &note.audio_filename {
            anyhow::ensure!(valid_audio_filename(name), "Nombre de audio inválido");
        }
        anyhow::ensure!(
            note.audio_filename.is_some() || note.audio_bytes == 0,
            "El tamaño de audio requiere un archivo asociado"
        );
        // Checked conversion prevents durations or counters wrapping on insertion.
        self.conn.execute("INSERT INTO notes(created_at_ms,original_text,final_text,word_count,captured_ms,sent_ms,provider,status,error,audio_filename,audio_bytes,input_tokens,output_tokens)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![
                sql_u64(note.created_at_ms)?, note.original, note.text, sql_u64(count_words(&note.text))?,
                sql_u64(note.captured_ms)?, sql_u64(note.sent_ms)?,
                match note.provider { ProviderMode::Subscription => "subscription", ProviderMode::ApiKey => "api_key" },
                match note.status { NoteStatus::Completed => "completed", NoteStatus::Failed => "failed" },
                note.error, note.audio_filename, sql_u64(note.audio_bytes)?, note.input_tokens.map(sql_u64).transpose()?, note.output_tokens.map(sql_u64).transpose()?
            ]).context("No se pudo guardar la nota")?;
        Ok(self.conn.last_insert_rowid())
    }
    /// Literal Unicode case-insensitive search across final and original text.
    /// Filtering in Rust also avoids SQL LIKE wildcard surprises for user input.
    pub fn notes(&self, query: &str, limit: usize) -> Result<Vec<Note>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {NOTE_COLUMNS} FROM notes ORDER BY created_at_ms DESC,id DESC"
        ))?;
        let query = query.trim().to_lowercase();
        let mut result = Vec::new();
        if limit == 0 {
            return Ok(result);
        }
        for row in statement.query_map([], read_note)? {
            let note = row?;
            if query.is_empty()
                || note.text.to_lowercase().contains(&query)
                || note.original.to_lowercase().contains(&query)
            {
                result.push(note);
                if result.len() == limit {
                    break;
                }
            }
        }
        Ok(result)
    }
    pub fn note(&self, id: i64) -> Result<Option<Note>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {NOTE_COLUMNS} FROM notes WHERE id=?1"),
                [id],
                read_note,
            )
            .optional()?)
    }
    /// Edits only the final text; original text and audio usage remain intact.
    pub fn edit_note_text(&self, id: i64, text: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE notes SET final_text=?1,word_count=?2 WHERE id=?3",
            params![text, sql_u64(count_words(text))?, id],
        )?;
        Ok(())
    }
    pub fn delete_note(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "DELETE FROM notes WHERE id=?1 RETURNING audio_filename",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    pub fn delete_note_audio(&self, id: i64) -> Result<Option<String>> {
        let transaction = self.conn.unchecked_transaction()?;
        let name: Option<String> = transaction
            .query_row("SELECT audio_filename FROM notes WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        transaction.execute(
            "UPDATE notes SET audio_filename=NULL,audio_bytes=0 WHERE id=?1",
            [id],
        )?;
        transaction.commit()?;
        Ok(name)
    }
    pub fn clear_history(&self) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("DELETE FROM notes RETURNING audio_filename")?;
        let names = statement
            .query_map([], |r| r.get::<_, Option<String>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names.into_iter().flatten().collect())
    }
    /// Read-only candidates: callers delete files before clearing associations,
    /// so a filesystem error leaves a reference that can be retried.
    pub fn expired_audio(
        &self,
        retention: AudioRetention,
        now_ms: u64,
    ) -> Result<Vec<(i64, String)>> {
        let age = match retention {
            AudioRetention::Off | AudioRetention::Forever => return Ok(Vec::new()),
            AudioRetention::Days7 => 7 * DAY_MS,
            AudioRetention::Days30 => 30 * DAY_MS,
        };
        let Some(before) = now_ms.checked_sub(age) else {
            return Ok(Vec::new());
        };
        let mut statement = self.conn.prepare("SELECT id,audio_filename FROM notes WHERE audio_filename IS NOT NULL AND created_at_ms <= ?1 ORDER BY created_at_ms,id")?;
        Ok(statement
            .query_map([sql_u64(before)?], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// No UI pagination limit applies to explicit history cleanup.
    pub fn audio_files(&self) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT audio_filename FROM notes WHERE audio_filename IS NOT NULL ORDER BY id",
        )?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// Off disables future recording only. Existing audio needs explicit deletion.
    /// Audio cleanup does not delete the transcript or its usage metrics.
    pub fn expire_audio(&self, retention: AudioRetention, now_ms: u64) -> Result<Vec<String>> {
        let before = match retention {
            AudioRetention::Forever | AudioRetention::Off => return Ok(Vec::new()),
            AudioRetention::Days7 => match now_ms.checked_sub(7 * DAY_MS) {
                Some(v) => v,
                None => return Ok(Vec::new()),
            },
            AudioRetention::Days30 => match now_ms.checked_sub(30 * DAY_MS) {
                Some(v) => v,
                None => return Ok(Vec::new()),
            },
        };
        let before = sql_u64(before)?;
        let transaction = self.conn.unchecked_transaction()?;
        let names = {
            let mut statement = transaction.prepare("SELECT audio_filename FROM notes WHERE audio_filename IS NOT NULL AND created_at_ms <= ?1")?;
            statement
                .query_map([before], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.execute("UPDATE notes SET audio_filename=NULL,audio_bytes=0 WHERE audio_filename IS NOT NULL AND created_at_ms <= ?1", [before])?;
        transaction.commit()?;
        Ok(names)
    }
    pub fn metrics(&self, period: UsagePeriod, now_ms: u64) -> Result<HistoryMetrics> {
        let mut statement = self.conn.prepare("SELECT COUNT(*),COALESCE(SUM(word_count),0),COALESCE(SUM(captured_ms),0),COALESCE(SUM(sent_ms),0),COALESCE(SUM(audio_bytes),0),CASE WHEN COUNT(input_tokens)=COUNT(*) THEN SUM(input_tokens) END,CASE WHEN COUNT(output_tokens)=COUNT(*) THEN SUM(output_tokens) END FROM notes WHERE created_at_ms >= ?1 AND created_at_ms <= ?2")?;
        Ok(statement.query_row(
            params![sql_u64(period.since_ms(now_ms))?, sql_u64(now_ms)?],
            |r| {
                let sent_ms = read_u64(r, 3)?;
                Ok(HistoryMetrics {
                    dictations: read_u64(r, 0)?,
                    words: read_u64(r, 1)?,
                    captured_ms: read_u64(r, 2)?,
                    sent_ms,
                    estimated_stt_usd: sent_ms as f64 / 3_600_000.0 * STT_USD_PER_HOUR,
                    audio_bytes: read_u64(r, 4)?,
                    input_tokens: read_optional_u64(r, 5)?,
                    output_tokens: read_optional_u64(r, 6)?,
                })
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn example(at: u64) -> NewNote {
        NewNote {
            created_at_ms: at,
            original: "hola mars".into(),
            text: "Hola, Mars. — 17-pro".into(),
            captured_ms: 3600,
            sent_ms: 3000,
            provider: ProviderMode::Subscription,
            status: NoteStatus::Completed,
            error: None,
            audio_filename: None,
            audio_bytes: 0,
            input_tokens: None,
            output_tokens: None,
        }
    }
    #[test]
    fn persists_searches_literal_unicode_and_keeps_dictionary() {
        let store = Store::in_memory().unwrap();
        store.remember("marte voz", "Mars voice").unwrap();
        let mut old = example(1);
        old.original = "ÓRBITA 100%".into();
        let old_id = store.record_note(old).unwrap();
        let new_id = store.record_note(example(2)).unwrap();
        let list = store.notes("", 10).unwrap();
        assert_eq!(
            list.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![new_id, old_id]
        );
        assert_eq!(list[0].word_count, 3);
        assert_eq!(store.notes("órbita", 10).unwrap()[0].id, old_id);
        assert_eq!(store.notes("%", 10).unwrap().len(), 1);
        assert!(store.notes("_", 10).unwrap().is_empty());
        assert_eq!(store.notes("", 1).unwrap().len(), 1);
        assert!(store.notes("", 0).unwrap().is_empty());
        store.delete_note(old_id).unwrap();
        assert!(store.note(old_id).unwrap().is_none());
        store.clear_history().unwrap();
        assert_eq!(store.list().unwrap()[0].to, "Mars voice");
    }
    #[test]
    fn metrics_use_sent_audio_and_real_optional_tokens() {
        let store = Store::in_memory().unwrap();
        let now = 40 * DAY_MS + 12 * 3_600_000;
        let empty = store.metrics(UsagePeriod::All, now).unwrap();
        assert_eq!(empty.dictations, 0);
        assert_eq!(empty.input_tokens, None);
        let mut note = example(now);
        note.sent_ms = 3_600_000;
        note.input_tokens = Some(24);
        note.output_tokens = Some(10);
        let id = store.record_note(note).unwrap();
        let metrics = store.metrics(UsagePeriod::Today, now).unwrap();
        assert_eq!(metrics.words, 3);
        assert_eq!(metrics.input_tokens, Some(24));
        assert!((metrics.estimated_stt_usd - 0.20).abs() < 1e-9);
        let mut failed = example(now - DAY_MS);
        failed.status = NoteStatus::Failed;
        failed.text = String::new();
        failed.error = Some("Conexión cerrada".into());
        store.record_note(failed).unwrap();
        let metrics = store.metrics(UsagePeriod::Week, now).unwrap();
        assert_eq!(metrics.dictations, 2);
        assert_eq!(metrics.input_tokens, None);
        assert_eq!(metrics.output_tokens, None);
        assert_eq!(metrics.sent_ms, 3_603_000);
        store.record_note(example(now - 8 * DAY_MS)).unwrap();
        store.record_note(example(now + DAY_MS)).unwrap();
        assert_eq!(store.metrics(UsagePeriod::Week, now).unwrap().dictations, 2);
        assert_eq!(store.metrics(UsagePeriod::All, now).unwrap().dictations, 3);
        store.delete_note(id).unwrap();
        assert_eq!(
            store.metrics(UsagePeriod::Today, now).unwrap().dictations,
            0
        );
    }
    #[test]
    fn retention_changes_only_audio_and_off_preserves_existing_files() {
        let store = Store::in_memory().unwrap();
        let now = 40 * DAY_MS;
        for (i, age) in [31, 8, 7, 0].into_iter().enumerate() {
            let mut note = example(now - age * DAY_MS);
            note.audio_filename = Some(format!("recording-{i}.wav"));
            note.audio_bytes = 48044;
            store.record_note(note).unwrap();
        }
        assert!(
            store
                .expire_audio(AudioRetention::Off, now)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .expire_audio(AudioRetention::Forever, now)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.expire_audio(AudioRetention::Days30, now).unwrap(),
            vec!["recording-0.wav"]
        );
        assert_eq!(
            store
                .expire_audio(AudioRetention::Days7, now)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(store.notes("", 20).unwrap().len(), 4);
        assert_eq!(
            store.metrics(UsagePeriod::All, now).unwrap().audio_bytes,
            48044
        );
        assert!(
            store
                .expire_audio(AudioRetention::Days7, now)
                .unwrap()
                .is_empty()
        );
        let id = store.notes("", 1).unwrap()[0].id;
        assert_eq!(
            store.delete_note_audio(id).unwrap(),
            Some("recording-3.wav".into())
        );
        assert!(store.note(id).unwrap().unwrap().audio_filename.is_none());
        assert_eq!(store.metrics(UsagePeriod::All, now).unwrap().audio_bytes, 0);
    }
    #[test]
    fn audio_paths_and_out_of_range_values_are_rejected_atomically() {
        let store = Store::in_memory().unwrap();
        for bad in [
            "../secret.wav",
            "/tmp/a.wav",
            "..wav",
            ".secret.wav",
            "foo/bar.wav",
            "a\\b.wav",
            "a.mp3",
        ] {
            let mut note = example(1);
            note.audio_filename = Some(bad.into());
            assert!(store.record_note(note).is_err(), "{bad}");
        }
        let mut note = example(1);
        note.input_tokens = Some(u64::MAX);
        assert!(store.record_note(note).is_err());
        assert!(store.notes("", 10).unwrap().is_empty());
        let mut note = example(1);
        note.audio_bytes = 10;
        assert!(store.record_note(note).is_err());
        let mut note = example(1);
        note.audio_filename = Some("dictation-123_abc.wav".into());
        store.record_note(note.clone()).unwrap();
        assert!(store.record_note(note).is_err());
        assert_eq!(
            store.clear_history().unwrap(),
            vec!["dictation-123_abc.wav"]
        );
        assert!(store.clear_history().unwrap().is_empty());
    }
    #[test]
    fn expiration_candidates_preserve_references_until_explicit_cleanup() {
        let store = Store::in_memory().unwrap();
        let now = 40 * DAY_MS;
        for (index, age) in [31, 8, 0].into_iter().enumerate() {
            let mut note = example(now - age * DAY_MS);
            note.audio_filename = Some(format!("audio-{index}.wav"));
            note.audio_bytes = 32044;
            store.record_note(note).unwrap();
        }
        let candidates = store.expired_audio(AudioRetention::Days7, now).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            store.expired_audio(AudioRetention::Days7, now).unwrap(),
            candidates
        );
        assert_eq!(store.audio_files().unwrap().len(), 3);
        assert_eq!(
            store.metrics(UsagePeriod::All, now).unwrap().audio_bytes,
            3 * 32044
        );
        assert!(
            store
                .expired_audio(AudioRetention::Off, now)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .expired_audio(AudioRetention::Days30, now)
                .unwrap()
                .len(),
            1
        );
        let (id, name) = &candidates[0];
        assert_eq!(store.delete_note_audio(*id).unwrap().as_ref(), Some(name));
        assert_eq!(
            store
                .expired_audio(AudioRetention::Days7, now)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.audio_files().unwrap().len(), 2);
        assert_eq!(store.notes("", 10).unwrap().len(), 3);
    }
    #[test]
    fn local_day_boundaries_do_not_use_utc_midnight() {
        let argentina = chrono::FixedOffset::west_opt(3 * 3600).unwrap();
        let tokyo = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
        let day = 40 * DAY_MS;
        assert_eq!(
            day_start_ms(day + 2 * 3_600_000, &argentina),
            day - DAY_MS + 3 * 3_600_000
        );
        assert_eq!(
            day_start_ms(day + 4 * 3_600_000, &argentina),
            day + 3 * 3_600_000
        );
        assert_eq!(day_start_ms(day, &tokyo), day - 9 * 3_600_000);
        assert_eq!(UsagePeriod::Week.since_ms(2 * DAY_MS), 0);
    }
    #[test]
    fn editing_note_preserves_original_audio_and_recounts_words() {
        let store = Store::in_memory().unwrap();
        let id = store.record_note(example(42)).unwrap();
        store
            .edit_note_text(id, "Texto corregido para Mars voice.")
            .unwrap();
        let note = store.note(id).unwrap().unwrap();
        assert_eq!(note.original, "hola mars");
        assert_eq!(note.word_count, 5);
        assert_eq!(note.sent_ms, 3000);
        assert_eq!(store.metrics(UsagePeriod::All, 100).unwrap().words, 5);
    }
    #[test]
    #[cfg(unix)]
    fn database_migrates_existing_corrections_and_reopens_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "dicta-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let db = dir.join("corrections.sqlite3");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE corrections(id INTEGER PRIMARY KEY, from_text TEXT NOT NULL UNIQUE COLLATE NOCASE, to_text TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO corrections(from_text,to_text) VALUES('hola','Hello');").unwrap();
        drop(conn);
        let store = Store::open(&dir).unwrap();
        assert_eq!(store.list().unwrap()[0].to, "Hello");
        let id = store.record_note(example(42)).unwrap();
        drop(store);
        let reopened = Store::open(&dir).unwrap();
        assert_eq!(reopened.note(id).unwrap().unwrap().created_at_ms, 42);
        assert_eq!(
            std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
