//! Render benchmark of the overlay and main window (headless, DEMO data).
//! Usage: cargo run -p dicta-ui --example perf --features screenshots [-- OUT.jsonl]
//! Env: DICTA_PERF_FPS (144), DICTA_PERF_SECONDS (6), DICTA_PERF_FILTER (all).
//! Linux only: it renders with GPUI's headless wgpu renderer.

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".local/perf-ui/last.jsonl".into());
    let reports = dicta_ui::bench::run(std::path::Path::new(&out))?;
    println!("{}", dicta_ui::bench::Report::HEADER);
    for r in &reports {
        println!("{}", r.row());
    }
    println!("{} escenas → {}", reports.len(), out);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The perf harness renders with the Linux headless renderer only.");
}
