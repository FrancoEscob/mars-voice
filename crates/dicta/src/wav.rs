//! PCM16 mono WAV files: read-aloud exports, saved readings and playback of
//! saved notes. Files are opened without following links (`dicta_core::fsx`).
use dicta_core::fsx;
use std::{
    io::{Read, Write},
    path::Path,
    sync::Arc,
};

fn wav_header(samples: u64, rate: u32) -> Vec<u8> {
    let data = (samples * 2) as u32;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * 2).to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data.to_le_bytes());
    h
}
/// Creates or replaces `path` without following a link. `mode` is the Unix
/// permission of a new file (ignored on Windows).
pub(crate) fn write_wav_file(
    path: &Path,
    parts: &[Arc<Vec<i16>>],
    rate: u32,
    mode: u32,
) -> std::io::Result<u64> {
    let samples: u64 = parts.iter().map(|p| p.len() as u64).sum();
    if samples * 2 > u64::from(u32::MAX) - 36 {
        return Err(std::io::Error::other("audio demasiado largo para WAV"));
    }
    let mut file = fsx::overwrite_nofollow(path, mode)?;
    let mut writer = std::io::BufWriter::new(&mut file);
    writer.write_all(&wav_header(samples, rate))?;
    for part in parts {
        for s in part.iter() {
            writer.write_all(&s.to_le_bytes())?;
        }
    }
    writer.flush()?;
    drop(writer);
    file.sync_all()?;
    Ok(44 + samples * 2)
}
/// Lee un WAV PCM16 mono de `data/audio` sin seguir enlaces.
pub(crate) fn read_history_wav(data_dir: &Path, name: &str) -> Result<(Vec<i16>, u32), String> {
    if !dicta_core::history::valid_audio_filename(name) {
        return Err("Nombre de audio inválido".into());
    }
    let dir = data_dir.join("audio");
    let meta = std::fs::symlink_metadata(&dir).map_err(|_| "No se encontró el audio guardado")?;
    if !meta.is_dir() || fsx::is_link(&meta) {
        return Err("La carpeta de audio no es válida".into());
    }
    let mut file = fsx::open_nofollow_read(&dir.join(name))
        .map_err(|_| "No se pudo abrir el audio guardado")?;
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return Err("El audio no es un archivo regular".into());
    }
    let mut bytes = vec![];
    file.read_to_end(&mut bytes)
        .map_err(|_| "No se pudo leer el audio guardado")?;
    parse_wav(&bytes).ok_or_else(|| "El audio guardado no es un WAV válido".into())
}
pub(crate) fn parse_wav(bytes: &[u8]) -> Option<(Vec<i16>, u32)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let mut i = 12;
    let mut format = None;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32_at(i + 4) as usize;
        let body = i + 8;
        if id == b"fmt " && len >= 16 && body + 16 <= bytes.len() {
            format = Some((
                u16_at(body),
                u16_at(body + 2),
                u32_at(body + 4),
                u16_at(body + 14),
            ));
        } else if id == b"data" {
            let (kind, channels, rate, bits) = format?;
            if kind != 1 || channels != 1 || bits != 16 {
                return None;
            }
            let end = (body + len).min(bytes.len());
            return Some((pcm_from_le(&bytes[body..end]), rate));
        }
        i = body + len + (len & 1);
    }
    None
}
fn pcm_from_le(bytes: &[u8]) -> Vec<i16> {
    let (pairs, _) = bytes.as_chunks::<2>();
    pairs.iter().map(|pair| i16::from_le_bytes(*pair)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mars-wav-io-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        dir
    }
    #[test]
    fn saved_audio_must_be_a_regular_pcm16_mono_file() {
        let dir = temp_dir();
        let parts = [Arc::new(vec![5i16, -5]), Arc::new(vec![7i16])];
        assert_eq!(
            write_wav_file(&dir.join("audio/note.wav"), &parts, 16_000, 0o600).unwrap(),
            44 + 6
        );
        assert_eq!(
            read_history_wav(&dir, "note.wav").unwrap(),
            (vec![5, -5, 7], 16_000)
        );
        std::fs::create_dir(dir.join("audio/folder.wav")).unwrap();
        assert!(read_history_wav(&dir, "folder.wav").is_err());
        let mut stereo = wav_header(2, 16_000);
        stereo[22] = 2;
        stereo.extend_from_slice(&[0; 4]);
        std::fs::write(dir.join("audio/stereo.wav"), stereo).unwrap();
        assert!(read_history_wav(&dir, "stereo.wav").is_err());
        assert!(read_history_wav(&dir, "missing.wav").is_err());
        assert!(read_history_wav(&dir, "../note.wav").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn links_are_not_followed_for_saved_audio_or_exports() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir();
        let outside = temp_dir();
        let one = [Arc::new(vec![1i16])];
        write_wav_file(&outside.join("audio/real.wav"), &one, 16_000, 0o600).unwrap();
        symlink(outside.join("audio/real.wav"), dir.join("audio/linked.wav")).unwrap();
        assert!(read_history_wav(&dir, "linked.wav").is_err());
        let target = outside.join("keep.txt");
        std::fs::write(&target, "keep").unwrap();
        symlink(&target, dir.join("export.wav")).unwrap();
        assert!(write_wav_file(&dir.join("export.wav"), &one, 16_000, 0o644).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        // A data folder whose audio/ is a link is refused.
        let data = std::env::temp_dir().join(format!(
            "mars-wav-io-link-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&data).unwrap();
        symlink(outside.join("audio"), data.join("audio")).unwrap();
        assert!(read_history_wav(&data, "real.wav").is_err());
        for path in [dir, outside, data] {
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
