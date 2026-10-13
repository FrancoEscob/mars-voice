//! Opt-in PCM archive. Disk I/O runs on a bounded worker, never on PipeWire or Tokio.
use dicta_core::fsx;
use std::{
    fs::{self, File},
    io::{self, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

const QUEUE_PACKETS: usize = 32;
const MAX_BYTES: u64 = 16_000 * 2 * 60 * 30;
static SERIAL: AtomicU64 = AtomicU64::new(0);

pub struct Recording {
    tx: Option<SyncSender<Vec<u8>>>,
    keep: Arc<AtomicBool>,
    done: oneshot::Receiver<io::Result<Option<SavedAudio>>>,
    failed: bool,
}
pub struct SavedAudio {
    path: Option<PathBuf>,
    pub filename: String,
    pub bytes: u64,
}
impl SavedAudio {
    pub fn commit(mut self) -> (String, u64) {
        self.path.take();
        (self.filename.clone(), self.bytes)
    }
}
impl Drop for SavedAudio {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}
impl Recording {
    pub fn start(data_dir: PathBuf) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE_PACKETS);
        let (done_tx, done) = oneshot::channel();
        let keep = Arc::new(AtomicBool::new(false));
        let worker_keep = keep.clone();
        std::thread::Builder::new()
            .name("mars-audio-archive".into())
            .spawn(move || {
                let result = write_archive(data_dir, rx, &worker_keep);
                let _ = done_tx.send(result);
            })?;
        Ok(Self {
            tx: Some(tx),
            keep,
            done,
            failed: false,
        })
    }
    /// A full/broken queue permanently disables this archive, preserving STT.
    pub fn push(&mut self, pcm: &[u8]) -> bool {
        if self.failed {
            return false;
        }
        if self
            .tx
            .as_ref()
            .is_none_or(|tx| tx.try_send(pcm.to_vec()).is_err())
        {
            self.failed = true;
            self.tx.take();
            return false;
        }
        true
    }
    pub async fn finish(mut self) -> io::Result<Option<SavedAudio>> {
        if !self.failed {
            self.keep.store(true, Ordering::Release);
        }
        self.tx.take();
        let result = (&mut self.done)
            .await
            .map_err(|_| io::Error::other("archive worker closed"))?;
        if self.failed {
            Err(io::Error::other("archive queue unavailable"))
        } else {
            result
        }
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        // A task cancelled while finish awaits also revokes retention.
        self.keep.store(false, Ordering::Release);
        self.tx.take();
    }
}
fn write_archive(
    data_dir: PathBuf,
    rx: mpsc::Receiver<Vec<u8>>,
    keep: &AtomicBool,
) -> io::Result<Option<SavedAudio>> {
    let directory = data_dir.join("audio");
    // Created 0700 if missing; a link or non-folder is refused.
    fsx::ensure_private_dir(&directory)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let filename = format!(
        "audio-{stamp}-{}-{}.wav",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    let path = directory.join(&filename);
    let mut file = fsx::create_private_file(&path)?;
    let mut saved = SavedAudio {
        path: Some(path),
        filename,
        bytes: 0,
    };
    file.write_all(&wav_header(0))?;
    let mut size = 0u64;
    for packet in rx {
        if !packet.len().is_multiple_of(2) || size + packet.len() as u64 > MAX_BYTES {
            return Err(io::Error::other("invalid PCM size"));
        }
        file.write_all(&packet)?;
        size += packet.len() as u64;
    }
    if size == 0 || !keep.load(Ordering::Acquire) {
        return Ok(None);
    }
    finalize(&mut file, size as u32)?;
    saved.bytes = size + 44;
    Ok(Some(saved))
}
fn finalize(file: &mut File, size: u32) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&wav_header(size))?;
    file.flush()?;
    Ok(())
}
fn wav_header(size: u32) -> [u8; 44] {
    let mut h = [0; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + size).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&1u16.to_le_bytes());
    h[24..28].copy_from_slice(&16000u32.to_le_bytes());
    h[28..32].copy_from_slice(&32000u32.to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&size.to_le_bytes());
    h
}
#[cfg(test)]
mod tests {
    use super::*;
    fn dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "mars-archive-test-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ))
    }
    #[test]
    fn full_queue_disables_archiving_without_waiting() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let (_done_tx, done) = oneshot::channel();
        let mut recording = Recording {
            tx: Some(tx),
            keep: Arc::new(AtomicBool::new(false)),
            done,
            failed: false,
        };
        assert!(recording.push(&[0, 0]));
        assert!(!recording.push(&[0, 0]));
        assert!(recording.tx.is_none());
        assert!(!recording.push(&[0, 0]));
    }
    #[tokio::test]
    async fn wav_is_valid_private_and_exact_duration() {
        let dir = dir();
        let mut recording = Recording::start(dir.clone()).unwrap();
        assert!(recording.push(&vec![0; 32000]));
        let saved = recording.finish().await.unwrap().unwrap();
        let file = dir.join("audio").join(&saved.filename);
        let data = fs::read(&file).unwrap();
        assert_eq!(&data[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(data[40..44].try_into().unwrap()), 32000);
        assert_eq!(data.len(), 32044);
        assert_eq!(saved.bytes, 32044);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(dir.join("audio"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        drop(saved);
        assert!(!file.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn cancel_removes_uncommitted_audio() {
        let dir = dir();
        let mut recording = Recording::start(dir.clone()).unwrap();
        recording.push(&[1, 2]);
        let (_dummy_tx, dummy_rx) = oneshot::channel();
        let done = std::mem::replace(&mut recording.done, dummy_rx);
        drop(recording);
        let result = done.await.unwrap().unwrap();
        assert!(result.is_none());
        assert_eq!(fs::read_dir(dir.join("audio")).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn filesystem_failure_is_nonfatal_result() {
        let dir = dir();
        fs::write(&dir, b"not a directory").unwrap();
        let mut recording = Recording::start(dir.clone()).unwrap();
        recording.push(&[1, 2]);
        assert!(recording.finish().await.is_err());
        fs::remove_file(dir).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn linked_audio_directory_is_refused() {
        let dir = dir();
        let elsewhere = dir.with_extension("target");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("audio")).unwrap();
        let mut recording = Recording::start(dir.clone()).unwrap();
        recording.push(&[1, 2]);
        assert!(recording.finish().await.is_err());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(elsewhere).unwrap();
    }
    #[tokio::test]
    async fn invalid_pcm_discards_archive() {
        let dir = dir();
        let mut recording = Recording::start(dir.clone()).unwrap();
        recording.push(&[1]);
        assert!(recording.finish().await.is_err());
        assert_eq!(fs::read_dir(dir.join("audio")).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
