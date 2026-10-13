//! Reproducible render benchmark for the overlay and the main window. Built
//! only with `--features screenshots`; run with
//! `cargo run -p dicta-ui --example perf --features screenshots`.
//!
//! The harness opens the real views in a headless GPUI app, feeds synthetic
//! DEMO snapshots at a controller-like cadence and steps a simulated
//! display clock frame by frame. GPUI in test mode draws every dirty window
//! as soon as the update that dirtied it finishes, which stands in for the
//! platform frame that would follow a snapshot; the harness only adds the
//! frames an animation asked for with `request_animation_frame`. For every
//! scenario it reports the wall time of a *tick* (applying one snapshot
//! plus the frames it caused: render, layout, prepaint and paint, no GPU
//! present), the wall time of the animation frames, and the work counters
//! of [`crate::perf`] (renders per window, frames requested, ticks skipped).
//! Clocks are simulated, so counters are deterministic; timings depend on
//! the machine and the build profile.
//!
//! Nothing here records audio, touches the network, the keyring or the
//! clipboard; every text is invented and marked DEMO.

use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Context as _;
use dicta_core::{
    AppConfig, AppSnapshot, AudioDevice, ColorMode, HistoryMetrics, Note, NoteStatus, Phase,
    ProviderMode, Theme,
    tts::{
        TtsPhase, TtsSnapshot, builtin_voices, estimate_ms, proportional_words, sentence_at,
        split_sentences, word_at,
    },
};
use gpui::{
    AnyWindowHandle, App, AppContext as _, Entity, EntityInputHandler as _, Focusable as _,
    HeadlessAppContext, Keystroke, px, size,
};

use crate::{
    Hub, OVERLAY_MAX_HEIGHT, OVERLAY_WIDTH, OverlayPlacement, READER_MAX_HEIGHT, READER_WIDTH,
    UiState, assets,
    overlay::Overlay,
    perf::Counters,
    reader::Reader,
    settings::{Section, Settings},
    text_field,
    theme::Palette,
    widgets,
};

const NOW_MS: u64 = 1_791_550_800_000;
const MIN: u64 = 60_000;

/// Simulated display refresh, frames per second. The user's main monitor
/// runs at 144 Hz; `DICTA_PERF_FPS` overrides.
fn fps() -> u64 {
    std::env::var("DICTA_PERF_FPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(144)
}

/// Simulated length of each timed scenario in seconds (`DICTA_PERF_SECONDS`).
fn seconds() -> f32 {
    std::env::var("DICTA_PERF_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6.)
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub n: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
    pub mean_us: u64,
    pub total_ms: f64,
}

impl Stats {
    fn of(samples: &[Duration]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut us: Vec<u64> = samples.iter().map(|d| d.as_micros() as u64).collect();
        us.sort_unstable();
        let pick = |q: f64| us[((us.len() - 1) as f64 * q).round() as usize];
        let total: u64 = us.iter().sum();
        Self {
            n: us.len(),
            p50_us: pick(0.5),
            p95_us: pick(0.95),
            max_us: *us.last().unwrap(),
            mean_us: total / us.len() as u64,
            total_ms: total as f64 / 1000.,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub name: String,
    /// Simulated frames the display offered.
    pub frames_offered: u64,
    /// Animation frames drawn on request, besides the ones snapshots caused.
    pub draws: u64,
    pub draw: Stats,
    /// Applying one snapshot plus the frames it caused.
    pub tick: Stats,
    pub snapshots: u64,
    pub seconds: f32,
    pub counters: Counters,
    pub notes: Vec<(String, String)>,
}

impl Report {
    pub fn json(&self) -> String {
        let mut s = format!(
            "{{\"name\":\"{}\",\"frames_offered\":{},\"draws\":{},\"snapshots\":{},\"seconds\":{},",
            self.name, self.frames_offered, self.draws, self.snapshots, self.seconds
        );
        for (label, st) in [("draw", &self.draw), ("tick", &self.tick)] {
            s += &format!(
                "\"{label}\":{{\"n\":{},\"p50_us\":{},\"p95_us\":{},\"max_us\":{},\"mean_us\":{},\"total_ms\":{:.3}}},",
                st.n, st.p50_us, st.p95_us, st.max_us, st.mean_us, st.total_ms
            );
        }
        s += "\"counters\":{";
        s += &self
            .counters
            .fields()
            .iter()
            .map(|(k, v)| format!("\"{}\":{}", k.to_lowercase(), v))
            .collect::<Vec<_>>()
            .join(",");
        s += "},\"notes\":{";
        s += &self
            .notes
            .iter()
            .map(|(k, v)| format!("\"{k}\":\"{v}\""))
            .collect::<Vec<_>>()
            .join(",");
        s += "}}";
        s
    }

    /// UI-thread milliseconds per simulated second.
    pub fn ui_ms_per_s(&self) -> f64 {
        if self.seconds <= 0. {
            return 0.;
        }
        (self.tick.total_ms + self.draw.total_ms) / self.seconds as f64
    }

    pub fn row(&self) -> String {
        let c = &self.counters;
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0} |",
            self.name,
            self.snapshots,
            self.tick.p50_us,
            self.tick.p95_us,
            self.draws,
            self.draw.p50_us,
            self.draw.p95_us,
            c.OVERLAY_RENDERS,
            c.SETTINGS_RENDERS,
            c.FRAMES_REQUESTED,
            c.SKIPPED_TICKS,
            self.ui_ms_per_s(),
        )
    }

    pub const HEADER: &'static str = "| escena | snapshots | tick p50 µs | tick p95 µs | frames anim | draw p50 µs | draw p95 µs | renders overlay | renders ventana | frames pedidos | ticks omitidos | ms UI por s |\n|---|---|---|---|---|---|---|---|---|---|---|---|";
}

// ------------------------------------------------------------------ fixtures

fn note(id: i64, ago_min: u64, text: String, captured_s: u64) -> Note {
    Note {
        id,
        created_at_ms: NOW_MS - ago_min * MIN,
        word_count: text.split_whitespace().count() as u64,
        original: text.clone(),
        text,
        captured_ms: captured_s * 1000,
        sent_ms: captured_s * 1000 - 400,
        provider: ProviderMode::Subscription,
        status: NoteStatus::Completed,
        error: None,
        audio_filename: None,
        audio_bytes: 0,
        input_tokens: None,
        output_tokens: None,
    }
}

/// `n` DEMO notes of about `bytes` bytes each (the controller's page is 100
/// notes; a long dictation is several KiB).
pub fn big_notes(n: usize, bytes: usize) -> Vec<Note> {
    let sentence = "DEMO: texto inventado para medir el historial, con acentos, ñ y \
                    longitud parecida a un dictado real. ";
    (0..n)
        .map(|i| {
            let mut text = String::with_capacity(bytes + sentence.len());
            while text.len() < bytes {
                text.push_str(sentence);
                if text.len() % 7 == 0 {
                    text.push('\n');
                }
            }
            note(
                1000 - i as i64,
                (i as u64 + 1) * 37,
                text,
                20 + i as u64 % 90,
            )
        })
        .collect()
}

pub fn demo_snapshot(theme: Theme, mode: ColorMode) -> AppSnapshot {
    let config = AppConfig {
        theme,
        color_mode: mode,
        ..Default::default()
    };
    let mut s = AppSnapshot::new(config);
    s.demo = true;
    s.authenticated = true;
    s.show_settings = false;
    s.status = "Ejemplo DEMO".into();
    s.devices = vec![AudioDevice {
        id: "demo-mic".into(),
        name: "Micrófono DEMO".into(),
    }];
    s.metrics = HistoryMetrics {
        dictations: 6,
        words: 1_284,
        captured_ms: 9 * MIN + 32_000,
        sent_ms: 9 * MIN + 4_000,
        estimated_stt_usd: 0.03,
        input_tokens: Some(412),
        output_tokens: Some(96),
        audio_bytes: 612_000,
    };
    s
}

/// Long DEMO dictation: `paragraphs` paragraphs of about 180 bytes.
pub fn long_text(paragraphs: usize) -> String {
    (1..=paragraphs)
        .map(|i| {
            format!(
                "DEMO párrafo {i}. Esta es una transcripción de prueba larga con palabras \
                 acentuadas y suficiente contenido para verificar ajuste de líneas, \
                 desplazamiento y controles visibles.\n"
            )
        })
        .collect()
}

/// Synthetic RMS at `t` seconds: 1.2 s of speech, 0.8 s of pause, repeated.
/// Speech ranges from soft to firm with a syllable rhythm.
fn speech_rms(t: f32) -> f32 {
    let cycle = t % 2.0;
    if cycle < 1.2 {
        0.02 + 0.12 * ((t * 11.).sin().abs()) + 0.04 * ((t * 3.).sin())
    } else {
        0.0006
    }
}

fn silence_rms(_t: f32) -> f32 {
    0.0006
}

fn hub(snap: AppSnapshot, cx: &mut App) -> Entity<Hub> {
    let (tx, _rx) = mpsc::channel();
    cx.new(|_| Hub::new(snap, tx))
}

// ------------------------------------------------------------------ driver

struct Harness {
    cx: HeadlessAppContext,
    /// Real instant that stands for simulated time zero.
    t0: Instant,
}

/// What the frame loop measured for one scenario.
struct Run {
    frames_offered: u64,
    draws: u64,
    draw: Vec<Duration>,
    tick: Vec<Duration>,
    snapshots: u64,
    seconds: f32,
}

fn renders() -> u64 {
    let c = Counters::now();
    c.OVERLAY_RENDERS + c.SETTINGS_RENDERS + c.READER_RENDERS
}

impl Harness {
    fn new() -> Self {
        let text = Arc::new(gpui_wgpu::CosmicTextSystem::new("Noto Sans"));
        let mut cx = HeadlessAppContext::with_platform(
            text,
            Arc::new(assets::Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.update(|cx| -> anyhow::Result<()> {
            assets::load_fonts(cx)?;
            cx.set_global(UiState::new(
                Palette::new(Theme::Signal, ColorMode::Light),
                crate::Lang::Es,
            ));
            text_field::bind_keys(cx);
            Ok(())
        })
        .expect("fonts");
        Self {
            cx,
            t0: Instant::now(),
        }
    }

    /// Switches the palette and, like the app at startup and on a style
    /// change, rasterises its grain in the background. Returns how long the
    /// rasterisation took to land (zero when already done).
    fn palette(&mut self, theme: Theme, mode: ColorMode) -> Duration {
        let palette = Palette::new(theme, mode);
        self.cx.update(|cx| {
            cx.global_mut::<UiState>().palette = palette;
            widgets::prewarm_grain(palette, cx);
        });
        let start = Instant::now();
        self.cx.run_until_parked();
        assert!(widgets::grain_ready(palette), "grain rasterised");
        start.elapsed()
    }

    /// Registers `window` as the hub's overlay, so live ticks reach it as
    /// in the app, where `sync_windows` opens it.
    fn adopt_overlay(&mut self, hub: &Entity<Hub>, window: gpui::WindowHandle<Overlay>) {
        let placement = OverlayPlacement::from_config(&AppConfig::default());
        self.cx.update(|cx| {
            hub.update(cx, |h, _| {
                h.overlay = Some((window, placement, OVERLAY_MAX_HEIGHT))
            })
        });
    }

    fn draw(&mut self, window: AnyWindowHandle) -> Duration {
        let start = Instant::now();
        self.cx
            .update_window(window, |_, window, cx| {
                window.draw(cx).clear(cx);
            })
            .expect("draw");
        start.elapsed()
    }

    /// Draws until background work (image decoding) settles; returns the
    /// first draw, the settle time and the number of extra draws.
    fn warm(&mut self, window: AnyWindowHandle) -> (Duration, Duration, u64) {
        let first = self.draw(window);
        let start = Instant::now();
        self.cx.run_until_parked();
        let mut extra = 0;
        for _ in 0..3 {
            self.draw(window);
            self.cx.run_until_parked();
            extra += 1;
        }
        (first, start.elapsed(), extra)
    }

    /// Steps the display clock for `duration` simulated seconds.
    /// `snapshot(sim_ms)` returns what the controller would publish at that
    /// millisecond (at most one every `cadence_ms`). Applying it lets GPUI
    /// draw the windows it dirtied; that whole update is one *tick*. When no
    /// snapshot drew a frame but the previous frame asked for another one,
    /// `window` is drawn as the animation frame the compositor would run.
    fn run_frames(
        &mut self,
        window: AnyWindowHandle,
        hub: &Entity<Hub>,
        duration: f32,
        cadence_ms: u64,
        mut snapshot: impl FnMut(u64) -> Option<AppSnapshot>,
    ) -> Run {
        let fps = fps();
        let frames = (duration * fps as f32) as u64;
        let mut run = Run {
            frames_offered: frames,
            draws: 0,
            draw: Vec::new(),
            tick: Vec::new(),
            snapshots: 0,
            seconds: duration,
        };
        let mut next_publish = 0u64;
        let mut requests_seen = Counters::now().FRAMES_REQUESTED;
        for f in 0..frames {
            let sim_ms = f * 1000 / fps;
            let at = self.t0 + Duration::from_millis(sim_ms);
            // Did the frames drawn since the last check ask for another one?
            let requests_now = Counters::now().FRAMES_REQUESTED;
            let animating = requests_now != requests_seen;
            requests_seen = requests_now;
            self.cx
                .update(|cx| hub.update(cx, |h, _| h.levels.drive_at(at)));
            let renders_before = renders();
            if sim_ms >= next_publish {
                next_publish = sim_ms + cadence_ms;
                if let Some(snap) = snapshot(sim_ms) {
                    run.snapshots += 1;
                    let start = Instant::now();
                    self.cx.update(|cx| {
                        hub.update(cx, |h, cx| {
                            h.apply(snap, at, cx);
                        })
                    });
                    run.tick.push(start.elapsed());
                }
            }
            if renders() == renders_before && animating {
                run.draw.push(self.draw(window));
                run.draws += 1;
            }
        }
        run
    }
}

fn report(name: &str, run: Run, counters: Counters, notes: Vec<(String, String)>) -> Report {
    Report {
        name: name.into(),
        frames_offered: run.frames_offered,
        draws: run.draws,
        draw: Stats::of(&run.draw),
        tick: Stats::of(&run.tick),
        snapshots: run.snapshots,
        seconds: run.seconds,
        counters,
        notes,
    }
}

fn slug(theme: Theme) -> &'static str {
    match theme {
        Theme::Signal | Theme::Night => "senal",
        Theme::Regolith => "regolito",
        Theme::Orbit => "orbita",
    }
}

fn n(label: &str, value: impl std::fmt::Display) -> (String, String) {
    (label.into(), value.to_string())
}

/// Frames are drawn while the harness pretends a 20 Hz level stream speaks
/// and pauses, with the live transcript growing.
fn overlay_recording(
    h: &mut Harness,
    theme: Theme,
    cadence_ms: u64,
    rms: fn(f32) -> f32,
    name: &str,
) -> anyhow::Result<Report> {
    let prewarm = h.palette(theme, ColorMode::Light);
    let mut snap = demo_snapshot(theme, ColorMode::Light);
    snap.phase = Phase::Recording;
    snap.confirmed = "DEMO. Che, mañana a las diez".into();
    snap.partial = "revisamos".into();
    let hub = h.cx.update(|cx| hub(snap.clone(), cx));
    let placement = OverlayPlacement::from_config(&AppConfig::default());
    let window = h.cx.open_window(
        size(px(OVERLAY_WIDTH), px(OVERLAY_MAX_HEIGHT)),
        |window, cx| cx.new(|cx| Overlay::new(hub.clone(), placement, window, cx)),
    )?;
    h.adopt_overlay(&hub, window);
    let (first, settle, extra) = h.warm(window.into());
    let words = [
        "el", "kernel", "nuevo", "en", "Hyprland", "y", "después", "vemos", "por", "qué",
    ];
    let mut seq = 0u64;
    let mut sentence = 0usize;
    let before = Counters::now();
    let speak = rms;
    let run = h.run_frames(window.into(), &hub, seconds(), cadence_ms, |sim_ms| {
        seq += 1;
        let t = sim_ms as f32 / 1000.;
        snap.level_seq = seq;
        snap.level = speak(t);
        snap.elapsed_ms = sim_ms;
        snap.revision += 1;
        // Partial changes every 200 ms, a sentence is confirmed every second.
        let word = (sim_ms / 200) as usize % words.len();
        snap.partial = words[..=word].join(" ");
        let s = (sim_ms / 1000) as usize;
        if s != sentence {
            sentence = s;
            snap.confirmed
                .push_str(" DEMO frase confirmada número con varias palabras más.");
        }
        Some(snap.clone())
    });
    let counters = Counters::now().since(before);
    h.cx.update(|cx| window.update(cx, |_, w, _| w.remove_window()))
        .ok();
    Ok(report(
        name,
        run,
        counters,
        vec![
            n("estilo", slug(theme)),
            n("cadencia_ms", cadence_ms),
            n("fps", fps()),
            n("prewarm_us", prewarm.as_micros()),
            n("first_draw_us", first.as_micros()),
            n("settle_us", settle.as_micros()),
            n("warm_draws", extra),
        ],
    ))
}

/// Idle pill, no snapshots: nothing may ask for frames.
fn overlay_idle(h: &mut Harness, theme: Theme) -> anyhow::Result<Report> {
    let prewarm = h.palette(theme, ColorMode::Light);
    let snap = demo_snapshot(theme, ColorMode::Light);
    let hub = h.cx.update(|cx| hub(snap, cx));
    let placement = OverlayPlacement::from_config(&AppConfig::default());
    let window = h.cx.open_window(
        size(px(OVERLAY_WIDTH), px(OVERLAY_MAX_HEIGHT)),
        |window, cx| cx.new(|cx| Overlay::new(hub.clone(), placement, window, cx)),
    )?;
    h.adopt_overlay(&hub, window);
    let (first, settle, extra) = h.warm(window.into());
    let before = Counters::now();
    let run = h.run_frames(window.into(), &hub, 2., 50, |_| None);
    let counters = Counters::now().since(before);
    h.cx.update(|cx| window.update(cx, |_, w, _| w.remove_window()))
        .ok();
    Ok(report(
        &format!("overlay-reposo-{}", slug(theme)),
        run,
        counters,
        vec![
            n("prewarm_us", prewarm.as_micros()),
            n("first_draw_us", first.as_micros()),
            n("settle_us", settle.as_micros()),
            n("warm_draws", extra),
        ],
    ))
}

/// Ready card with an 80-paragraph text: steady redraws, then typing.
fn overlay_ready(h: &mut Harness, theme: Theme) -> anyhow::Result<Vec<Report>> {
    h.palette(theme, ColorMode::Light);
    let mut snap = demo_snapshot(theme, ColorMode::Light);
    snap.phase = Phase::Ready;
    snap.final_text = long_text(80);
    snap.original_text = snap.final_text.clone();
    snap.status = "Texto listo".into();
    let hub = h.cx.update(|cx| hub(snap, cx));
    let placement = OverlayPlacement::from_config(&AppConfig::default());
    let window = h.cx.open_window(
        size(px(OVERLAY_WIDTH), px(OVERLAY_MAX_HEIGHT)),
        |window, cx| cx.new(|cx| Overlay::new(hub.clone(), placement, window, cx)),
    )?;
    h.adopt_overlay(&hub, window);
    let overlay: Entity<Overlay> =
        h.cx.update(|cx| window.root(cx))
            .ok()
            .context("overlay root")?;
    let (first, settle, extra) = h.warm(window.into());
    let mut out = Vec::new();

    // Steady: the overlay is notified (as a snapshot would) and redrawn.
    // Notifying inside an update makes GPUI draw at once: one tick each.
    let before = Counters::now();
    let mut ticks = Vec::new();
    for _ in 0..60 {
        let start = Instant::now();
        h.cx.update(|cx| overlay.update(cx, |_, cx| cx.notify()));
        ticks.push(start.elapsed());
    }
    out.push(Report {
        name: format!("overlay-listo-largo-{}", slug(theme)),
        frames_offered: 60,
        draws: 0,
        draw: Stats::default(),
        tick: Stats::of(&ticks),
        snapshots: 60,
        seconds: 0.,
        counters: Counters::now().since(before),
        notes: vec![
            n("parrafos", 80),
            n("first_draw_us", first.as_micros()),
            n("settle_us", settle.as_micros()),
            n("warm_draws", extra),
        ],
    });

    // Typing: focus the editor, insert one character per frame, redraw.
    let field = h.cx.update(|cx| overlay.read(cx).field().clone());
    h.cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                let focus = field.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            })
            .ok();
    });
    h.cx.run_until_parked();
    // One keystroke: the edit plus the frame GPUI draws for it.
    let before = Counters::now();
    let mut ticks = Vec::new();
    for i in 0..40 {
        let start = Instant::now();
        h.cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    field.update(cx, |f, cx| {
                        let ch = if i % 7 == 6 { " " } else { "a" };
                        f.replace_text_in_range(None, ch, window, cx);
                    })
                })
                .ok();
        });
        ticks.push(start.elapsed());
    }
    out.push(Report {
        name: format!("overlay-listo-tipeo-{}", slug(theme)),
        frames_offered: 40,
        draws: 0,
        draw: Stats::default(),
        tick: Stats::of(&ticks),
        snapshots: 40,
        seconds: 0.,
        counters: Counters::now().since(before),
        notes: vec![n("parrafos", 80), n("teclas", 40)],
    });
    h.cx.update(|cx| window.update(cx, |_, w, _| w.remove_window()))
        .ok();
    Ok(out)
}

/// Main window on `section` with 100 notes of 8 KiB while a dictation
/// records in the overlay: the window must stay quiet.
fn main_recording(
    h: &mut Harness,
    theme: Theme,
    section: Section,
    name: &str,
) -> anyhow::Result<Report> {
    h.palette(theme, ColorMode::Light);
    let mut snap = demo_snapshot(theme, ColorMode::Light);
    snap.notes = big_notes(100, 8 * 1024).into();
    snap.show_settings = true;
    if section == Section::History {
        snap.selected_note = snap.notes.first().cloned();
        snap.active_note_id = snap.selected_note.as_ref().map(|n| n.id);
    }
    snap.phase = Phase::Recording;
    snap.confirmed = "DEMO. Che, mañana a las diez".into();
    let hub = h.cx.update(|cx| hub(snap.clone(), cx));
    // The overlay is open too while recording; its ticks are part of the
    // scenario, the main window must stay quiet.
    let placement = OverlayPlacement::from_config(&AppConfig::default());
    let overlay = h.cx.open_window(
        size(px(OVERLAY_WIDTH), px(OVERLAY_MAX_HEIGHT)),
        |window, cx| cx.new(|cx| Overlay::new(hub.clone(), placement, window, cx)),
    )?;
    h.adopt_overlay(&hub, overlay);
    h.warm(overlay.into());
    let window = h.cx.open_window(size(px(1040.), px(720.)), |window, cx| {
        cx.new(|cx| Settings::new(hub.clone(), window, cx).with_section(section))
    })?;
    let (first, settle, extra) = h.warm(window.into());
    let mut seq = 0u64;
    let before = Counters::now();
    // Animation frames belong to the overlay; the main window needs none.
    let run = h.run_frames(overlay.into(), &hub, seconds(), 50, |sim_ms| {
        seq += 1;
        snap.level_seq = seq;
        snap.level = speech_rms(sim_ms as f32 / 1000.);
        snap.elapsed_ms = sim_ms;
        snap.revision += 1;
        snap.partial = format!("palabra {}", sim_ms / 200);
        Some(snap.clone())
    });
    let counters = Counters::now().since(before);
    h.cx.update(|cx| window.update(cx, |_, w, _| w.remove_window()))
        .ok();
    h.cx.update(|cx| overlay.update(cx, |_, w, _| w.remove_window()))
        .ok();
    Ok(report(
        name,
        run,
        counters,
        vec![
            n("notas", 100),
            n("bytes_por_nota", 8192),
            n("first_draw_us", first.as_micros()),
            n("settle_us", settle.as_micros()),
            n("warm_draws", extra),
        ],
    ))
}

/// About `chars` characters of invented DEMO paragraphs to read aloud.
fn reading_text(chars: usize) -> String {
    let mut text = String::new();
    let mut i = 1;
    while text.chars().count() < chars {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!(
            "DEMO párrafo {i}. La sonda midió el viento durante la tarde y encontró ráfagas \
             más fuertes de lo previsto. Los ingenieros revisaron los paneles y anotaron cada \
             cambio. Al caer el sol, la temperatura bajó cerca de ochenta grados."
        ));
        i += 1;
    }
    text
}

/// The reader plays a 20 000 character text at a 30 Hz snapshot cadence
/// (position, word, output level) while the main window shows Inicio with
/// 100 notes: only the reader may redraw.
fn reader_playing(h: &mut Harness, theme: Theme, name: &str) -> anyhow::Result<Report> {
    h.palette(theme, ColorMode::Light);
    let mut snap = demo_snapshot(theme, ColorMode::Light);
    snap.notes = big_notes(100, 8 * 1024).into();
    snap.show_settings = true;
    let text = reading_text(20_000);
    let chars = text.chars().count();
    let total = estimate_ms(chars, 1.0);
    let sentences = Arc::new(split_sentences(&text));
    let words = Arc::new(proportional_words(&text, total));
    let paragraphs = text.split("\n\n").count();
    snap.tts = TtsSnapshot {
        phase: TtsPhase::Playing,
        show: true,
        reading: 1,
        text: Arc::new(text),
        sentences: sentences.clone(),
        words: words.clone(),
        generated: Arc::new(vec![(0, total / 2)]),
        total_ms: total,
        rate: 1.,
        voice: "carina".into(),
        voices: Arc::new(builtin_voices()),
        chars: chars as u64,
        ..Default::default()
    };
    let hub = h.cx.update(|cx| hub(snap.clone(), cx));
    let placement = OverlayPlacement::for_reader(&AppConfig::default());
    let reader = h.cx.open_window(
        size(px(READER_WIDTH), px(READER_MAX_HEIGHT)),
        |window, cx| cx.new(|cx| Reader::new(hub.clone(), placement, window, cx)),
    )?;
    h.cx.update(|cx| {
        hub.update(cx, |h, _| {
            h.reader = Some((reader, placement, READER_MAX_HEIGHT))
        })
    });
    let (first, settle, extra) = h.warm(reader.into());
    let main = h.cx.open_window(size(px(1040.), px(720.)), |window, cx| {
        cx.new(|cx| Settings::new(hub.clone(), window, cx).with_section(Section::Home))
    })?;
    h.warm(main.into());
    let start_ms = total * 3 / 10;
    let mut seq = 0u64;
    let before = Counters::now();
    let run = h.run_frames(reader.into(), &hub, seconds(), 33, |sim_ms| {
        seq += 1;
        let position = start_ms + sim_ms;
        let word = word_at(&words, position);
        snap.tts.level_seq = seq;
        snap.tts.level = speech_rms(sim_ms as f32 / 1000.);
        snap.tts.position_ms = position;
        snap.tts.current_word = word;
        snap.tts.current_sentence = word.and_then(|w| sentence_at(&sentences, words[w].span.start));
        snap.revision += 1;
        Some(snap.clone())
    });
    let counters = Counters::now().since(before);
    h.cx.update(|cx| main.update(cx, |_, w, _| w.remove_window()))
        .ok();
    h.cx.update(|cx| reader.update(cx, |_, w, _| w.remove_window()))
        .ok();
    Ok(report(
        name,
        run,
        counters,
        vec![
            n("caracteres", chars),
            n("parrafos", paragraphs),
            n("cadencia_ms", 33),
            n("reader_renders", counters.READER_RENDERS),
            n("reader_ticks", counters.READER_TICKS),
            n("first_draw_us", first.as_micros()),
            n("settle_us", settle.as_micros()),
            n("warm_draws", extra),
        ],
    ))
}

/// Typing in the history search: how many `SearchNotes` reach the
/// controller for ten quick keystrokes, and the frame cost of each.
fn search_typing(h: &mut Harness) -> anyhow::Result<Report> {
    h.palette(Theme::Signal, ColorMode::Light);
    let mut snap = demo_snapshot(Theme::Signal, ColorMode::Light);
    snap.notes = big_notes(100, 8 * 1024).into();
    snap.show_settings = true;
    let hub = h.cx.update(|cx| hub(snap, cx));
    let window = h.cx.open_window(size(px(1040.), px(720.)), |window, cx| {
        cx.new(|cx| Settings::new(hub.clone(), window, cx).with_section(Section::History))
    })?;
    let settings: Entity<Settings> =
        h.cx.update(|cx| window.root(cx))
            .ok()
            .context("settings root")?;
    h.warm(window.into());
    let search = h.cx.update(|cx| settings.read(cx).fields.search.clone());
    h.cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                let focus = search.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            })
            .ok();
    });
    h.cx.run_until_parked();
    let before = Counters::now();
    let mut ticks = Vec::new();
    for ch in "hyprland k".chars() {
        let start = Instant::now();
        h.cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    search.update(cx, |f, cx| {
                        f.replace_text_in_range(None, &ch.to_string(), window, cx);
                    })
                })
                .ok();
        });
        ticks.push(start.elapsed());
        // 60 ms between keystrokes, well inside any debounce.
        h.cx.advance_clock(Duration::from_millis(60));
        h.cx.run_until_parked();
    }
    let sent_while_typing = Counters::now().since(before).SEARCH_SENT;
    // Let a debounce expire.
    h.cx.advance_clock(Duration::from_millis(500));
    h.cx.run_until_parked();
    let counters = Counters::now().since(before);
    h.cx.update(|cx| window.update(cx, |_, w, _| w.remove_window()))
        .ok();
    Ok(Report {
        name: "historial-busqueda-tipeo".into(),
        frames_offered: 10,
        draws: 0,
        draw: Stats::default(),
        tick: Stats::of(&ticks),
        snapshots: 10,
        seconds: 0.,
        counters,
        notes: vec![
            n("teclas", 10),
            n("search_sent_mientras_tipea", sent_while_typing),
            n("search_sent_total", counters.SEARCH_SENT),
        ],
    })
}

/// Which windows each kind of notification redraws, with both windows open.
/// Printed, not reported: it explains the counters of the other scenarios.
fn diag_invalidation(h: &mut Harness) -> anyhow::Result<()> {
    h.palette(Theme::Signal, ColorMode::Light);
    let mut snap = demo_snapshot(Theme::Signal, ColorMode::Light);
    snap.notes = big_notes(20, 1024).into();
    snap.show_settings = true;
    snap.phase = Phase::Recording;
    let hub = h.cx.update(|cx| hub(snap.clone(), cx));
    let placement = OverlayPlacement::from_config(&AppConfig::default());
    let overlay = h.cx.open_window(
        size(px(OVERLAY_WIDTH), px(OVERLAY_MAX_HEIGHT)),
        |window, cx| cx.new(|cx| Overlay::new(hub.clone(), placement, window, cx)),
    )?;
    h.adopt_overlay(&hub, overlay);
    let settings = h.cx.open_window(size(px(1040.), px(720.)), |window, cx| {
        cx.new(|cx| Settings::new(hub.clone(), window, cx).with_section(Section::Home))
    })?;
    h.warm(overlay.into());
    h.warm(settings.into());
    let overlay_view: Entity<Overlay> = h.cx.update(|cx| overlay.root(cx))?;
    let settings_view: Entity<Settings> = h.cx.update(|cx| settings.root(cx))?;
    let step = |label: &str, h: &mut Harness, f: &mut dyn FnMut(&mut Harness)| {
        let before = Counters::now();
        f(h);
        h.cx.run_until_parked();
        let d = Counters::now().since(before);
        eprintln!(
            "diag {label}: overlay renders {} · settings renders {} · hub notif {} · live ticks {}",
            d.OVERLAY_RENDERS, d.SETTINGS_RENDERS, d.HUB_NOTIFIES, d.LIVE_TICKS
        );
    };
    step("nada", h, &mut |_| {});
    step("overlay.notify", h, &mut |h| {
        h.cx.update(|cx| overlay_view.update(cx, |_, cx| cx.notify()));
    });
    step("hub.tick_overlay", h, &mut |h| {
        h.cx.update(|cx| hub.update(cx, |hub, cx| hub.tick_overlay(cx)));
    });
    step("hub.notify", h, &mut |h| {
        h.cx.update(|cx| hub.update(cx, |_, cx| cx.notify()));
    });
    step("settings.notify", h, &mut |h| {
        h.cx.update(|cx| settings_view.update(cx, |_, cx| cx.notify()));
    });
    let mut level = snap.clone();
    level.level_seq += 1;
    level.level = 0.2;
    level.revision += 1;
    let at = h.t0 + Duration::from_millis(10);
    step("snapshot de nivel", h, &mut |h| {
        let s = level.clone();
        h.cx.update(|cx| hub.update(cx, |hub, cx| hub.apply(s, at, cx)));
    });
    h.cx.update(|cx| overlay.update(cx, |_, w, _| w.remove_window()))
        .ok();
    h.cx.update(|cx| settings.update(cx, |_, w, _| w.remove_window()))
        .ok();
    Ok(())
}

/// A keystroke through the window dispatcher, to make sure the harness can
/// drive real key paths (used by tests; the scenarios above call the input
/// handler directly, which is what IME and the platform do).
#[allow(dead_code)]
fn keystroke(h: &mut Harness, window: AnyWindowHandle, key: &str) {
    let ks = Keystroke::parse(key).expect("keystroke");
    h.cx.update(|cx| {
        cx.update_window(window, |_, window, cx| {
            window.dispatch_keystroke(ks, cx);
        })
        .ok();
    });
}

/// Runs every scenario whose name contains one of the comma separated parts
/// of `DICTA_PERF_FILTER` (all when unset) and writes `<out>` as JSON lines.
pub fn run(out: &Path) -> anyhow::Result<Vec<Report>> {
    let filter: Vec<String> = std::env::var("DICTA_PERF_FILTER")
        .map(|v| {
            v.split(',')
                .map(|f| f.trim().to_owned())
                .filter(|f| !f.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let wanted = |name: &str| filter.is_empty() || filter.iter().any(|f| name.contains(f));
    let mut h = Harness::new();
    let mut reports = Vec::new();
    let push = |r: Report, reports: &mut Vec<Report>| {
        eprintln!("{}", r.row());
        reports.push(r);
    };
    eprintln!("{}", Report::HEADER);
    for theme in Theme::ALL {
        let name = format!("overlay-grabando-{}-50ms", slug(theme));
        if wanted(&name) {
            push(
                overlay_recording(&mut h, theme, 50, speech_rms, &name)?,
                &mut reports,
            );
        }
        let name = format!("overlay-silencio-{}-50ms", slug(theme));
        if wanted(&name) {
            push(
                overlay_recording(&mut h, theme, 50, silence_rms, &name)?,
                &mut reports,
            );
        }
    }
    for cadence in [20u64, 100] {
        let name = format!("overlay-grabando-senal-{cadence}ms");
        if wanted(&name) {
            push(
                overlay_recording(&mut h, Theme::Signal, cadence, speech_rms, &name)?,
                &mut reports,
            );
        }
    }
    if wanted("overlay-reposo") {
        push(overlay_idle(&mut h, Theme::Regolith)?, &mut reports);
    }
    if wanted("overlay-listo") {
        for r in overlay_ready(&mut h, Theme::Signal)? {
            push(r, &mut reports);
        }
    }
    for (section, name) in [
        (Section::Home, "inicio-grabando-100notas"),
        (Section::History, "historial-grabando-100notas"),
    ] {
        if wanted(name) {
            push(
                main_recording(&mut h, Theme::Regolith, section, name)?,
                &mut reports,
            );
        }
    }
    if wanted("historial-busqueda") {
        push(search_typing(&mut h)?, &mut reports);
    }
    for theme in Theme::ALL {
        let name = format!("lector-reproduciendo-20k-{}", slug(theme));
        if wanted(&name) {
            push(reader_playing(&mut h, theme, &name)?, &mut reports);
        }
    }
    if filter.iter().any(|f| f == "diag") {
        diag_invalidation(&mut h)?;
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body: String = reports.iter().map(|r| r.json() + "\n").collect();
    std::fs::write(out, body).with_context(|| format!("escribiendo {}", out.display()))?;
    Ok(reports)
}
