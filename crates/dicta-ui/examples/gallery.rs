//! Writes PNGs of the overlay and settings for the Mars voice styles and colour modes.
//! Usage: cargo run -p dicta-ui --example gallery --features screenshots [-- OUT_DIR]
//! Linux only: it renders with GPUI's headless wgpu renderer.

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../design/mars-voice/screens"
        )
        .into()
    });
    for path in dicta_ui::gallery::render_gallery(std::path::Path::new(&out))? {
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The gallery renders with the Linux headless renderer only.");
}
