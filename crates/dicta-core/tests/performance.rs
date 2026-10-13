//! Repeatable synthetic workload. Run ignored in isolation with --nocapture.
//! No user's files, audio, credentials, network or clipboard are accessed.
use dicta_core::{AppSnapshot, NewNote, NoteStatus, ProviderMode, UsagePeriod, store::Store};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
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

fn measure(name: &str, iterations: u64, mut f: impl FnMut()) {
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let micros = start.elapsed().as_micros() as u64;
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
    let bytes = BYTES.load(Ordering::Relaxed) - bytes;
    println!(
        "PERF {name} iterations={iterations} total_us={micros} us_each={:.2} allocations_each={:.2} bytes_each={:.2}",
        micros as f64 / iterations as f64,
        allocations as f64 / iterations as f64,
        bytes as f64 / iterations as f64,
    );
}

#[test]
#[ignore = "synthetic performance measurement, run alone with --nocapture"]
fn snapshot_and_history_workload() {
    let store = Store::in_memory().unwrap();
    let text = "Órbita sintética para medir la copia del historial. ".repeat(160);
    for index in 0..10_000 {
        store
            .record_note(NewNote {
                created_at_ms: 1_000_000 + index,
                original: text.clone(),
                text: text.clone(),
                captured_ms: 1000,
                sent_ms: 1000,
                provider: ProviderMode::Subscription,
                status: NoteStatus::Completed,
                error: None,
                audio_filename: None,
                audio_bytes: 0,
                input_tokens: None,
                output_tokens: None,
            })
            .unwrap();
    }
    let mut snap = AppSnapshot::new(Default::default());
    snap.notes = store.notes("", 100).unwrap().into();
    assert_eq!(snap.notes.len(), 100);
    measure("snapshot_100_notes_8k", 1000, || {
        black_box(snap.clone());
    });
    measure("list_100_from_10000", 100, || {
        black_box(store.notes("", 100).unwrap());
    });
    measure("search_miss_10000", 10, || {
        assert!(store.notes("missing-query", 100).unwrap().is_empty());
    });
    measure("metrics_all_10000", 100, || {
        black_box(store.metrics(UsagePeriod::All, 2_000_000).unwrap());
    });
}
