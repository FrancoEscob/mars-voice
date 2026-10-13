//! Actual demo controller, no microphone/provider/clipboard or filesystem I/O.
use dicta::controller::{self, Options};
use dicta_core::{AppPaths, AppSnapshot, Command, Phase};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
struct CountAlloc;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;

#[test]
fn continuous_snapshots_follow_meter_cadence_without_idle_publications() {
    let (tx, commands) = mpsc::channel();
    let (snap_tx, snapshots) = mpsc::channel();
    let controller_tx = tx.clone();
    let worker = std::thread::spawn(move || {
        controller::run(
            AppPaths {
                config: "/unused-synthetic".into(),
                data: "/unused-synthetic".into(),
                runtime: "/unused-synthetic".into(),
            },
            AppSnapshot::new(Default::default()),
            commands,
            controller_tx,
            snap_tx,
            Options {
                demo: true,
                no_tray: true,
                ..Default::default()
            },
        )
    });
    struct Shutdown {
        tx: mpsc::Sender<Command>,
        worker: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    }
    impl Drop for Shutdown {
        fn drop(&mut self) {
            let _ = self.tx.send(Command::Quit);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap().unwrap();
            }
        }
    }
    let _shutdown = Shutdown {
        tx: tx.clone(),
        worker: Some(worker),
    };
    snapshots.recv_timeout(Duration::from_secs(3)).unwrap();
    let idle = Instant::now();
    let idle_published = usize::from(snapshots.recv_timeout(Duration::from_millis(250)).is_ok());
    let idle_ms = idle.elapsed().as_millis();
    assert_eq!(
        idle_published, 0,
        "idle must not publish unchanged snapshots"
    );
    tx.send(Command::Start).unwrap();
    loop {
        if snapshots
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .phase
            == Phase::Recording
        {
            break;
        }
    }
    let start = Instant::now();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);
    let mut previous = None;
    let mut total_interval_us = 0;
    let mut max_interval_us = 0;
    let mut count = 0;
    while start.elapsed() < Duration::from_millis(1200) {
        if snapshots.recv_timeout(Duration::from_millis(100)).is_err() {
            break;
        }
        let now = Instant::now();
        if let Some(previous) = previous {
            let us = now.duration_since(previous).as_micros() as u64;
            total_interval_us += us;
            max_interval_us = max_interval_us.max(us);
        }
        previous = Some(now);
        count += 1;
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
    let allocated_bytes = BYTES.load(Ordering::Relaxed) - bytes;
    println!(
        "PERF controller idle_ms={idle_ms} idle_snapshots={idle_published} recording_ms={} snapshots={count} avg_interval_us={} max_interval_us={max_interval_us} allocations={allocated} bytes={allocated_bytes}",
        start.elapsed().as_millis(),
        total_interval_us / (count - 1).max(1)
    );
    assert!(
        count >= 40,
        "continuous meter snapshots are too sparse: {count} in 1.2s"
    );
    assert!(
        count <= 65,
        "publication must remain rate limited: {count} in 1.2s"
    );
}
