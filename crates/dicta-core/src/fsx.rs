//! Owner-only files and folders that never follow a final link.
//!
//! - Unix: folders 0700, files 0600, files opened with `O_NOFOLLOW`.
//! - Windows: everything lives under the per-user profile, whose inherited ACL
//!   already limits access to that user. Reparse points (symlinks, junctions)
//!   are opened as themselves with `FILE_FLAG_OPEN_REPARSE_POINT` and refused.
//!
//! Errors are plain [`io::Error`]s; callers add an actionable context.
use std::{
    ffi::OsString,
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Creates `path` and its parents and restricts the final folder to the owner.
/// An existing link to a folder is accepted, so a dotfiles-managed
/// `~/.config/dicta` keeps working. Use [`ensure_private_dir`] to refuse links.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    sys::restrict_dir(path)
}

/// Creates `path` (with parents) if it is missing, refuses a final link or a
/// non-folder and restricts it to the owner.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    sys::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if is_link(&metadata) || !metadata.is_dir() {
        return Err(refused());
    }
    sys::restrict_dir(path)
}

/// Replaces `path` atomically: writes an owner-only `<name>.tmp` beside it,
/// syncs it and renames it over `path`.
pub fn write_private_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = tmp_path(path)?;
    let mut file = overwrite_nofollow(&tmp, sys::PRIVATE_FILE)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    sys::rename(&tmp, path)
}

/// Creates a new owner-only file for writing. Fails if anything, including a
/// link, already exists at `path`.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    open(&mut options, path, sys::PRIVATE_FILE)
}

/// Opens or creates an owner-only file for reading and writing, without
/// truncating it or following a final link. Used for lock files.
pub fn open_private_rw(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    sys::refuse_existing_link(path)?;
    open(&mut options, path, sys::PRIVATE_FILE)
}

/// Creates or truncates `path` for writing without following a final link.
/// `mode` is the Unix permission of a new file; on Windows the folder's ACL
/// applies instead.
pub fn overwrite_nofollow(path: &Path, mode: u32) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    sys::refuse_existing_link(path)?;
    open(&mut options, path, mode)
}

/// Opens an existing file for reading without following a final link.
pub fn open_nofollow_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    open(&mut options, path, sys::PRIVATE_FILE)
}

/// Restricts an existing file to its owner (0600 on Unix). On Windows the
/// profile ACL already does, so this does nothing.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    sys::restrict_file(path)
}

/// True for symbolic links and, on Windows, any reparse point (junctions
/// included). Pass metadata from [`fs::symlink_metadata`] or an opened file.
pub fn is_link(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink() || sys::is_reparse_point(metadata)
}

fn open(options: &mut OpenOptions, path: &Path, mode: u32) -> io::Result<File> {
    sys::no_follow(options, mode);
    let file = options.open(path)?;
    if sys::CHECK_OPENED && is_link(&file.metadata()?) {
        return Err(refused());
    }
    Ok(file)
}

fn tmp_path(path: &Path) -> io::Result<PathBuf> {
    let mut name: OsString = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ruta sin nombre de archivo"))?
        .to_os_string();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "la ruta es un enlace o no es del tipo esperado; Mars voice no sigue enlaces",
    )
}

#[cfg(unix)]
mod sys {
    use std::{
        fs::{self, DirBuilder, Metadata, OpenOptions, Permissions},
        io,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        path::Path,
    };
    pub const PRIVATE_FILE: u32 = 0o600;
    const PRIVATE_DIR: u32 = 0o700;
    /// `O_NOFOLLOW` already refuses links at open time.
    pub const CHECK_OPENED: bool = false;

    pub fn no_follow(options: &mut OpenOptions, mode: u32) {
        // std already opens with O_CLOEXEC; mode only applies on creation.
        options.mode(mode).custom_flags(libc::O_NOFOLLOW);
    }
    pub fn refuse_existing_link(_: &Path) -> io::Result<()> {
        Ok(())
    }
    pub fn create_dir_all(path: &Path) -> io::Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR)
            .create(path)
    }
    pub fn restrict_dir(path: &Path) -> io::Result<()> {
        fs::set_permissions(path, Permissions::from_mode(PRIVATE_DIR))
    }
    pub fn restrict_file(path: &Path) -> io::Result<()> {
        fs::set_permissions(path, Permissions::from_mode(PRIVATE_FILE))
    }
    pub fn is_reparse_point(_: &Metadata) -> bool {
        false
    }
    pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
}

#[cfg(windows)]
mod sys {
    use std::{
        fs::{self, Metadata, OpenOptions},
        io,
        os::windows::fs::{MetadataExt, OpenOptionsExt},
        path::Path,
        thread,
        time::Duration,
    };
    // winbase.h / winnt.h / winerror.h
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    /// Unused on Windows: the per-user profile ACL is inherited.
    pub const PRIVATE_FILE: u32 = 0;
    /// The open flag only avoids following a link; the handle is checked after.
    pub const CHECK_OPENED: bool = true;

    pub fn no_follow(options: &mut OpenOptions, _mode: u32) {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    /// Opening a link with create/truncate would rewrite the link itself, so
    /// refuse it before opening. The handle is checked again after opening.
    pub fn refuse_existing_link(path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if super::is_link(&metadata) => Err(super::refused()),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    pub fn create_dir_all(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }
    pub fn restrict_dir(_: &Path) -> io::Result<()> {
        Ok(())
    }
    pub fn restrict_file(_: &Path) -> io::Result<()> {
        Ok(())
    }
    pub fn is_reparse_point(metadata: &Metadata) -> bool {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    /// Antivirus and indexers briefly hold new files open; retry ~1 s.
    pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
        let mut attempts = 0;
        loop {
            match fs::rename(from, to) {
                Err(error)
                    if attempts < 40
                        && matches!(
                            error.raw_os_error(),
                            Some(
                                ERROR_ACCESS_DENIED
                                    | ERROR_SHARING_VIOLATION
                                    | ERROR_LOCK_VIOLATION
                            )
                        ) =>
                {
                    attempts += 1;
                    thread::sleep(Duration::from_millis(25));
                }
                result => return result,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dicta-fsx-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temporary_file() {
        let temp = Temp::new();
        let dir = temp.0.join("a/b");
        create_private_dir(&dir).unwrap();
        let path = dir.join("config.toml");
        write_private_file(&path, b"one").unwrap();
        write_private_file(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!dir.join("config.toml.tmp").exists());
        let mut read = String::new();
        open_nofollow_read(&path)
            .unwrap()
            .read_to_string(&mut read)
            .unwrap();
        assert_eq!(read, "two");
    }

    #[test]
    fn create_private_file_never_reuses_an_existing_path() {
        let temp = Temp::new();
        let path = temp.0.join("audio.wav");
        create_private_file(&path).unwrap().write_all(b"x").unwrap();
        assert!(create_private_file(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"x");
        // Lock files keep their content and are reopened, not truncated.
        let lock = temp.0.join("instance.lock");
        open_private_rw(&lock).unwrap().write_all(b"7 8").unwrap();
        drop(open_private_rw(&lock).unwrap());
        assert_eq!(fs::read(&lock).unwrap(), b"7 8");
    }

    #[test]
    fn ensure_private_dir_refuses_files() {
        let temp = Temp::new();
        let file = temp.0.join("file");
        fs::write(&file, "untouched").unwrap();
        assert!(ensure_private_dir(&file).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "untouched");
        ensure_private_dir(&temp.0.join("x/audio")).unwrap();
        ensure_private_dir(&temp.0.join("x/audio")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_modes_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temp = Temp::new();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let dir = temp.0.join("config");
        create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        let audio = temp.0.join("data/audio");
        ensure_private_dir(&audio).unwrap();
        assert_eq!(mode(&audio), 0o700);
        assert_eq!(mode(&temp.0.join("data")), 0o700);
        write_private_file(&dir.join("config.toml"), b"x").unwrap();
        assert_eq!(mode(&dir.join("config.toml")), 0o600);
        create_private_file(&audio.join("a.wav")).unwrap();
        assert_eq!(mode(&audio.join("a.wav")), 0o600);
        open_private_rw(&temp.0.join("lock")).unwrap();
        assert_eq!(mode(&temp.0.join("lock")), 0o600);
        overwrite_nofollow(&temp.0.join("export.wav"), 0o644).unwrap();
        assert_eq!(mode(&temp.0.join("export.wav")), 0o644);
        let shared = temp.0.join("shared.sqlite3");
        fs::write(&shared, "").unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o644)).unwrap();
        restrict_file(&shared).unwrap();
        assert_eq!(mode(&shared), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_refused_without_touching_their_targets() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = Temp::new();
        let target = temp.0.join("target.txt");
        fs::write(&target, "secret").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let link = temp.0.join("link");
        symlink(&target, &link).unwrap();
        assert!(is_link(&fs::symlink_metadata(&link).unwrap()));
        assert!(!is_link(&fs::symlink_metadata(&target).unwrap()));
        assert!(open_nofollow_read(&link).is_err());
        assert!(overwrite_nofollow(&link, 0o600).is_err());
        assert!(open_private_rw(&link).is_err());
        assert!(create_private_file(&link).is_err());
        // The temporary file of an atomic write is not followed either.
        symlink(&target, temp.0.join("config.toml.tmp")).unwrap();
        assert!(write_private_file(&temp.0.join("config.toml"), b"x").is_err());
        assert!(!temp.0.join("config.toml").exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "secret");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let target_dir = temp.0.join("target-dir");
        fs::create_dir(&target_dir).unwrap();
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o750)).unwrap();
        let dir_link = temp.0.join("dir-link");
        symlink(&target_dir, &dir_link).unwrap();
        assert!(ensure_private_dir(&dir_link).is_err());
        assert_eq!(
            fs::metadata(&target_dir).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }
}
