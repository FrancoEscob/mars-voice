use crate::{Correction, fsx};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;
pub struct Store {
    pub(crate) conn: Connection,
}
impl Store {
    pub fn open(data_dir: &Path) -> Result<Self> {
        // Do not change permissions on a symlink target before rejecting it.
        match std::fs::symlink_metadata(data_dir) {
            Ok(meta) => anyhow::ensure!(
                meta.is_dir() && !fsx::is_link(&meta),
                "La carpeta de datos no puede ser un enlace simbólico"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fsx::create_private_dir(data_dir)?;
        let path = data_dir.join("corrections.sqlite3");
        const NOT_A_FILE: &str = "No se pudo abrir el historial y diccionario local; la base debe ser un archivo, no un enlace simbólico";
        // SQLite's NOFOLLOW is enforced by its Unix VFS only; Windows relies
        // on this check of the database path (links and junctions).
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => anyhow::ensure!(!fsx::is_link(&meta), NOT_A_FILE),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context(NOT_A_FILE),
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .context(NOT_A_FILE)?;
        fsx::restrict_file(&path)?;
        Self::from_connection(conn)
    }
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS corrections(id INTEGER PRIMARY KEY, from_text TEXT NOT NULL UNIQUE COLLATE NOCASE, to_text TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
            id INTEGER PRIMARY KEY,
            created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
            original_text TEXT NOT NULL,
            final_text TEXT NOT NULL,
            word_count INTEGER NOT NULL CHECK(word_count >= 0),
            captured_ms INTEGER NOT NULL CHECK(captured_ms >= 0),
            sent_ms INTEGER NOT NULL CHECK(sent_ms >= 0),
            provider TEXT NOT NULL CHECK(provider IN ('subscription','api_key')),
            status TEXT NOT NULL CHECK(status IN ('completed','failed')),
            error TEXT,
            audio_filename TEXT UNIQUE,
            audio_bytes INTEGER NOT NULL DEFAULT 0 CHECK(audio_bytes >= 0),
            input_tokens INTEGER CHECK(input_tokens >= 0),
            output_tokens INTEGER CHECK(output_tokens >= 0)
        );
        CREATE INDEX IF NOT EXISTS notes_created_at ON notes(created_at_ms DESC, id DESC);",
        )?;
        crate::readings::migrate(&conn)?;
        Ok(Self { conn })
    }
    pub fn remember(&self, from: &str, to: &str) -> Result<()> {
        let from = from.trim();
        let to = to.trim();
        anyhow::ensure!(
            !from.is_empty() && !to.is_empty() && from != to,
            "Escribí una corrección con origen y destino distintos"
        );
        anyhow::ensure!(
            from.chars().count() <= 200 && to.chars().count() <= 200,
            "Usá una corrección de hasta 200 caracteres"
        );
        anyhow::ensure!(
            !from.chars().any(char::is_control) && !to.chars().any(char::is_control),
            "La corrección no puede incluir caracteres de control"
        );
        self.conn.execute("INSERT INTO corrections(from_text,to_text) VALUES(?1,?2) ON CONFLICT(from_text) DO UPDATE SET to_text=excluded.to_text",params![from,to])?;
        Ok(())
    }
    pub fn list(&self) -> Result<Vec<Correction>> {
        let mut st = self
            .conn
            .prepare("SELECT id,from_text,to_text FROM corrections ORDER BY id")?;
        Ok(st
            .query_map([], |r| {
                Ok(Correction {
                    id: r.get(0)?,
                    from: r.get(1)?,
                    to: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn forget(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM corrections WHERE id=?1", [id])?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn learn_is_explicit_reversible_and_updates() {
        let s = Store::in_memory().unwrap();
        assert!(s.list().unwrap().is_empty());
        s.remember("hiper land", "Hyprland").unwrap();
        s.remember("hiper land", "HYPRLAND").unwrap();
        let rows = s.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].to, "HYPRLAND");
        s.forget(rows[0].id).unwrap();
        assert!(s.list().unwrap().is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_database_and_data_dir_without_modifying_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!(
            "dicta-store-links-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let target = root.join("private.txt");
        std::fs::write(&target, "not a database").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&target, data.join("corrections.sqlite3")).unwrap();
        assert!(Store::open(&data).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "not a database");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let directory_target = root.join("target-dir");
        std::fs::create_dir(&directory_target).unwrap();
        std::fs::set_permissions(&directory_target, std::fs::Permissions::from_mode(0o750))
            .unwrap();
        let link = root.join("linked-data");
        symlink(&directory_target, &link).unwrap();
        assert!(Store::open(&link).is_err());
        assert_eq!(
            std::fs::metadata(&directory_target)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert!(!directory_target.join("corrections.sqlite3").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_empty_control_and_self_replacements() {
        let s = Store::in_memory().unwrap();
        assert!(s.remember("", "a").is_err());
        assert!(s.remember("a", "a").is_err());
        assert!(s.remember("a\nb", "a").is_err());
    }
}
