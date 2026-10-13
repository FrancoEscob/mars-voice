//! Windows: embeds the app icon as resource 1 (GPUI uses it for the windows
//! and the tray loads it too) and the version information Explorer and Task
//! Manager show ("Mars voice"). Nothing happens on other systems.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    windows::embed();
}

#[cfg(windows)]
mod windows {
    use std::{env, fs, path::PathBuf};

    /// Sizes available in `design/mars-voice/icons`; Windows scales the rest.
    const SIZES: [u32; 5] = [32, 48, 64, 128, 256];

    pub fn embed() {
        if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
            return;
        }
        let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
        let icons = crate_dir.join("../../design/mars-voice/icons");
        let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
        let images: Vec<Vec<u8>> = SIZES
            .iter()
            .map(|size| {
                let path = icons.join(format!("mars-voice-{size}.png"));
                println!("cargo:rerun-if-changed={}", path.display());
                fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            })
            .collect();
        let icon = out.join("mars-voice.ico");
        fs::write(&icon, ico_from_pngs(&images)).expect("write mars-voice.ico");

        let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
        let numbers: Vec<String> = version
            .split(['.', '-', '+'])
            .take(3)
            .map(|part| part.parse::<u16>().unwrap_or(0).to_string())
            .chain(std::iter::repeat("0".to_owned()))
            .take(4)
            .collect();
        let numbers = numbers.join(",");
        let icon_path = icon.display().to_string().replace('\\', "\\\\");
        let rc = format!(
            r#"#pragma code_page(65001)
1 ICON "{icon_path}"
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Mars voice"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "dicta"
      VALUE "OriginalFilename", "MarsVoice.exe"
      VALUE "ProductName", "Mars voice"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
        );
        let rc_path = out.join("mars-voice.rc");
        fs::write(&rc_path, rc).expect("write mars-voice.rc");
        embed_resource::compile(&rc_path, embed_resource::NONE)
            .manifest_required()
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// An .ico that stores each PNG unchanged (supported since Windows Vista).
    fn ico_from_pngs(images: &[Vec<u8>]) -> Vec<u8> {
        let mut ico = Vec::new();
        ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
        ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * images.len();
        for png in images {
            let (width, height) = png_size(png);
            // 0 means 256 pixels.
            ico.push(if width >= 256 { 0 } else { width as u8 });
            ico.push(if height >= 256 { 0 } else { height as u8 });
            ico.push(0); // no palette
            ico.push(0); // reserved
            ico.extend_from_slice(&1u16.to_le_bytes()); // planes
            ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
            ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
            ico.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += png.len();
        }
        for png in images {
            ico.extend_from_slice(png);
        }
        ico
    }

    fn png_size(png: &[u8]) -> (u32, u32) {
        assert!(
            png.len() > 24 && png.starts_with(b"\x89PNG\r\n\x1a\n") && &png[12..16] == b"IHDR",
            "icon is not a PNG"
        );
        let read = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().expect("4 bytes"));
        (read(16), read(20))
    }
}
