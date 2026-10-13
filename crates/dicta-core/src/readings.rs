//! Historial local de lecturas en voz alta. Separado de `notes` porque las
//! métricas de captura y envío STT no aplican a texto leído.
use crate::{
    AudioRetention, ProviderMode,
    history::{DAY_MS, NoteStatus, UsagePeriod, valid_audio_filename},
    store::Store,
    tts::estimate_usd,
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HistoryKind {
    #[default]
    Dictations,
    Readings,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reading {
    pub id: i64,
    pub created_at_ms: u64,
    pub text: String,
    pub chars: u64,
    /// Caracteres enviados al proveedor, incluidas regeneraciones.
    pub chars_sent: u64,
    /// Audio generado de la versión escuchada.
    pub duration_ms: u64,
    pub voice: String,
    pub language: String,
    pub provider: ProviderMode,
    pub status: NoteStatus,
    pub error: Option<String>,
    pub audio_filename: Option<String>,
    pub audio_bytes: u64,
}
#[derive(Debug, Clone)]
pub struct NewReading {
    pub created_at_ms: u64,
    pub text: String,
    pub voice: String,
    pub language: String,
    pub provider: ProviderMode,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadingMetrics {
    pub readings: u64,
    pub chars_sent: u64,
    pub duration_ms: u64,
    /// Equivalente a tarifa pública por caracteres enviados. No es un cobro.
    pub estimated_tts_usd: f64,
    pub audio_bytes: u64,
}

pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS readings (
            id INTEGER PRIMARY KEY,
            created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
            text TEXT NOT NULL,
            chars INTEGER NOT NULL CHECK(chars >= 0),
            chars_sent INTEGER NOT NULL DEFAULT 0 CHECK(chars_sent >= 0),
            duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms >= 0),
            voice TEXT NOT NULL,
            language TEXT NOT NULL,
            provider TEXT NOT NULL CHECK(provider IN ('subscription','api_key')),
            status TEXT NOT NULL CHECK(status IN ('completed','failed')),
            error TEXT,
            audio_filename TEXT UNIQUE,
            audio_bytes INTEGER NOT NULL DEFAULT 0 CHECK(audio_bytes >= 0),
            audio_layout TEXT
        );
        CREATE INDEX IF NOT EXISTS readings_created_at ON readings(created_at_ms DESC, id DESC);",
    )
}

const COLUMNS: &str = "id,created_at_ms,text,chars,chars_sent,duration_ms,voice,language,provider,status,error,audio_filename,audio_bytes";
fn sql_u64(value: u64) -> Result<i64> {
    i64::try_from(value).context("Valor de historial fuera de rango")
}
fn read_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn read_reading(row: &rusqlite::Row<'_>) -> rusqlite::Result<Reading> {
    Ok(Reading {
        id: row.get(0)?,
        created_at_ms: read_u64(row, 1)?,
        text: row.get(2)?,
        chars: read_u64(row, 3)?,
        chars_sent: read_u64(row, 4)?,
        duration_ms: read_u64(row, 5)?,
        voice: row.get(6)?,
        language: row.get(7)?,
        provider: match row.get::<_, String>(8)?.as_str() {
            "api_key" => ProviderMode::ApiKey,
            _ => ProviderMode::Subscription,
        },
        status: match row.get::<_, String>(9)?.as_str() {
            "failed" => NoteStatus::Failed,
            _ => NoteStatus::Completed,
        },
        error: row.get(10)?,
        audio_filename: row.get(11)?,
        audio_bytes: read_u64(row, 12)?,
    })
}
impl Store {
    pub fn record_reading(&self, reading: NewReading) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO readings(created_at_ms,text,chars,voice,language,provider,status)
             VALUES(?1,?2,?3,?4,?5,?6,'completed')",
                params![
                    sql_u64(reading.created_at_ms)?,
                    reading.text,
                    sql_u64(reading.text.chars().count() as u64)?,
                    reading.voice,
                    reading.language,
                    match reading.provider {
                        ProviderMode::Subscription => "subscription",
                        ProviderMode::ApiKey => "api_key",
                    },
                ],
            )
            .context("No se pudo guardar la lectura en el historial")?;
        Ok(self.conn.last_insert_rowid())
    }
    /// Actualiza el uso acumulado y el resultado. Un error no borra lo generado.
    pub fn update_reading(
        &self,
        id: i64,
        chars_sent: u64,
        duration_ms: u64,
        voice: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE readings SET chars_sent=?1,duration_ms=?2,voice=?3,status=?4,error=?5 WHERE id=?6",
            params![
                sql_u64(chars_sent)?,
                sql_u64(duration_ms)?,
                voice,
                if error.is_some() { "failed" } else { "completed" },
                error,
                id
            ],
        )?;
        Ok(())
    }
    /// Asocia un WAV ya escrito y su distribución por fragmentos (JSON opaco
    /// para el controlador). Devuelve el archivo anterior, si lo había, para borrarlo.
    pub fn set_reading_audio(
        &self,
        id: i64,
        name: &str,
        bytes: u64,
        layout: &str,
    ) -> Result<Option<String>> {
        anyhow::ensure!(valid_audio_filename(name), "Nombre de audio inválido");
        anyhow::ensure!(layout.len() <= 1_000_000, "Distribución de audio inválida");
        let transaction = self.conn.unchecked_transaction()?;
        let previous: Option<String> = transaction
            .query_row(
                "SELECT audio_filename FROM readings WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let updated = transaction.execute(
            "UPDATE readings SET audio_filename=?1,audio_bytes=?2,audio_layout=?3 WHERE id=?4",
            params![name, sql_u64(bytes)?, layout, id],
        )?;
        anyhow::ensure!(updated == 1, "La lectura ya no está en el historial");
        transaction.commit()?;
        Ok(previous.filter(|p| p != name))
    }
    pub fn reading_audio_layout(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT audio_layout FROM readings WHERE id=?1 AND audio_filename IS NOT NULL",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    pub fn readings(&self, query: &str, limit: usize) -> Result<Vec<Reading>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM readings ORDER BY created_at_ms DESC,id DESC"
        ))?;
        let query = query.trim().to_lowercase();
        let mut result = Vec::new();
        if limit == 0 {
            return Ok(result);
        }
        for row in statement.query_map([], read_reading)? {
            let reading = row?;
            if query.is_empty() || reading.text.to_lowercase().contains(&query) {
                result.push(reading);
                if result.len() == limit {
                    break;
                }
            }
        }
        Ok(result)
    }
    pub fn reading(&self, id: i64) -> Result<Option<Reading>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM readings WHERE id=?1"),
                [id],
                read_reading,
            )
            .optional()?)
    }
    pub fn delete_reading(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "DELETE FROM readings WHERE id=?1 RETURNING audio_filename",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    pub fn delete_reading_audio(&self, id: i64) -> Result<Option<String>> {
        let transaction = self.conn.unchecked_transaction()?;
        let name: Option<String> = transaction
            .query_row(
                "SELECT audio_filename FROM readings WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        transaction.execute(
            "UPDATE readings SET audio_filename=NULL,audio_bytes=0,audio_layout=NULL WHERE id=?1",
            [id],
        )?;
        transaction.commit()?;
        Ok(name)
    }
    pub fn reading_audio_files(&self) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT audio_filename FROM readings WHERE audio_filename IS NOT NULL ORDER BY id",
        )?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn clear_readings(&self) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("DELETE FROM readings RETURNING audio_filename")?;
        let names = statement
            .query_map([], |r| r.get::<_, Option<String>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names.into_iter().flatten().collect())
    }
    /// Candidatos de solo lectura: se borra el archivo antes de soltar la referencia.
    pub fn expired_reading_audio(
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
        let mut statement = self.conn.prepare("SELECT id,audio_filename FROM readings WHERE audio_filename IS NOT NULL AND created_at_ms <= ?1 ORDER BY created_at_ms,id")?;
        Ok(statement
            .query_map([sql_u64(before)?], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn reading_metrics(&self, period: UsagePeriod, now_ms: u64) -> Result<ReadingMetrics> {
        let mut statement = self.conn.prepare("SELECT COUNT(*),COALESCE(SUM(chars_sent),0),COALESCE(SUM(duration_ms),0),COALESCE(SUM(audio_bytes),0) FROM readings WHERE created_at_ms >= ?1 AND created_at_ms <= ?2")?;
        Ok(statement.query_row(
            params![sql_u64(period.since_ms(now_ms))?, sql_u64(now_ms)?],
            |r| {
                let chars_sent = read_u64(r, 1)?;
                Ok(ReadingMetrics {
                    readings: read_u64(r, 0)?,
                    chars_sent,
                    duration_ms: read_u64(r, 2)?,
                    estimated_tts_usd: estimate_usd(chars_sent),
                    audio_bytes: read_u64(r, 3)?,
                })
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reading(at: u64, text: &str) -> NewReading {
        NewReading {
            created_at_ms: at,
            text: text.into(),
            voice: "carina".into(),
            language: "auto".into(),
            provider: ProviderMode::Subscription,
        }
    }
    #[test]
    fn readings_record_update_search_and_count_sent_characters() {
        let s = Store::in_memory().unwrap();
        let now = 40 * DAY_MS;
        let a = s.record_reading(reading(now - 1000, "Hola Marte")).unwrap();
        let b = s
            .record_reading(reading(now - 10 * DAY_MS, "Ñandú veloz"))
            .unwrap();
        s.update_reading(a, 20, 1500, "eve", None).unwrap();
        s.update_reading(b, 11, 900, "carina", Some("Cuota agotada"))
            .unwrap();
        let all = s.readings("", 10).unwrap();
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(all[0].voice, "eve");
        assert_eq!(all[0].chars, 10);
        assert_eq!(all[1].status, NoteStatus::Failed);
        assert_eq!(s.readings("ñANDÚ", 10).unwrap()[0].id, b);
        let week = s.reading_metrics(UsagePeriod::Week, now).unwrap();
        assert_eq!(
            (week.readings, week.chars_sent, week.duration_ms),
            (1, 20, 1500)
        );
        let all_time = s.reading_metrics(UsagePeriod::All, now).unwrap();
        assert_eq!(all_time.chars_sent, 31);
        assert!((all_time.estimated_tts_usd - 31.0 * 15.0 / 1e6).abs() < 1e-12);
        // Las lecturas no alteran las métricas de dictado.
        assert_eq!(s.metrics(UsagePeriod::All, now).unwrap().dictations, 0);
    }
    #[test]
    fn reading_audio_is_replaced_expired_and_deleted_without_losing_text() {
        let s = Store::in_memory().unwrap();
        let now = 40 * DAY_MS;
        let old = s
            .record_reading(reading(now - 8 * DAY_MS, "vieja"))
            .unwrap();
        let new = s.record_reading(reading(now, "nueva")).unwrap();
        assert!(s.set_reading_audio(old, "../x.wav", 10, "[]").is_err());
        assert_eq!(
            s.set_reading_audio(old, "tts-1.wav", 10, "[]").unwrap(),
            None
        );
        assert_eq!(
            s.set_reading_audio(old, "tts-2.wav", 20, "[]").unwrap(),
            Some("tts-1.wav".into())
        );
        s.set_reading_audio(new, "tts-3.wav", 30, "[]").unwrap();
        assert!(s.set_reading_audio(999, "tts-4.wav", 1, "[]").is_err());
        assert_eq!(
            s.expired_reading_audio(AudioRetention::Days7, now).unwrap(),
            vec![(old, "tts-2.wav".to_string())]
        );
        assert!(
            s.expired_reading_audio(AudioRetention::Off, now)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            s.delete_reading_audio(old).unwrap(),
            Some("tts-2.wav".into())
        );
        assert_eq!(s.reading_audio_layout(old).unwrap(), None);
        assert_eq!(s.reading_audio_layout(new).unwrap().as_deref(), Some("[]"));
        assert_eq!(s.reading(old).unwrap().unwrap().text, "vieja");
        assert_eq!(
            s.reading_audio_files().unwrap(),
            vec!["tts-3.wav".to_string()]
        );
        assert_eq!(s.delete_reading(new).unwrap(), Some("tts-3.wav".into()));
        assert!(s.clear_readings().unwrap().is_empty());
        assert!(s.readings("", 10).unwrap().is_empty());
    }
}
