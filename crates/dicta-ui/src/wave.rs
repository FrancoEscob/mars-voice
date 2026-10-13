//! Live voice drawing.
//!
//! Input: one linear RMS reading per audio `Level` event. The controller
//! bumps `AppSnapshot::level_seq` only for those events, so snapshots sent
//! for partials or the clock never add a reading ([`LevelHistory::push`]).
//!
//! Output: an envelope that is continuous in time. Each reading becomes a
//! point reached over a short ramp after it arrived; between points the
//! envelope is interpolated, so a 60 Hz frame draws smooth motion whatever
//! the reading cadence (10 Hz buffers or faster). Frames are only requested
//! while something visible still moves; silence is drawn still.
//!
//! Scale: dBFS between a fixed floor and a ceiling that only rises when the
//! voice gets close to it and slowly returns afterwards. This is a visual
//! scale, never audio gain: quiet rooms stay at zero, loud voices keep
//! headroom so normal and loud speech still look different.

use std::{
    cell::RefCell,
    collections::VecDeque,
    f32::consts::{PI, TAU},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{Bounds, Pixels, Rgba, Window, canvas, div, fill, img, point, prelude::*, px, size};

use crate::{
    assets, perf,
    theme::{Palette, VoiceStyle},
};

/// Quiet room noise stays below this and draws nothing.
const FLOOR_DB: f32 = -54.;
/// Ceiling while nobody has spoken loudly: -8 dBFS is loud speech close to
/// a typical microphone, so normal speech (-30..-20) sits mid-height.
const BASE_CEIL_DB: f32 = -8.;
/// The ceiling stays this far above the loudest recent reading.
const HEADROOM_DB: f32 = 4.;
/// How fast a raised ceiling returns to [`BASE_CEIL_DB`].
const CEIL_RETURN_DB_PER_S: f32 = 1.5;
/// Display curve and the gate under which nothing shows.
const GAMMA: f32 = 1.4;
const GATE: f32 = 0.05;
/// Time a reading takes to be reached after it arrives.
const RAMP: Duration = Duration::from_millis(70);
/// Release time constant: a syllable decays instead of dropping.
const RELEASE_S: f32 = 0.12;
/// Without new readings for this long the envelope falls to silence.
const STALE: Duration = Duration::from_millis(260);
const STALE_FADE_S: f32 = 0.18;
/// Readings older than this are dropped.
const KEEP: Duration = Duration::from_millis(3200);
/// RMS at or above -1 dBFS is reported as a very high level, whatever the
/// visual scale does, so a too-hot microphone stays visible. RMS near full
/// scale is a strong hint, not proof: true clipping is a sample peak at full
/// scale, which this stream does not carry, so the UI never says "saturando".
const VERY_LOUD_DB: f32 = -1.;
/// Overlay badge while the level stays very high: [`crate::i18n::Msg::VeryLoud`].
const VERY_LOUD_HOLD: Duration = Duration::from_millis(1500);
/// Time shown across the wave and the dots, newest at the right.
pub const WAVE_SPAN: f32 = 2.2;
const DOTS_SPAN: f32 = 2.6;
/// Height of the voice row in the overlay (wave and dots).
pub const VOICE_HEIGHT: f32 = 36.;

/// Linear RMS to dBFS; silence, NaN and negatives are -inf.
fn db(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0. {
        return f32::NEG_INFINITY;
    }
    20. * rms.min(1.).log10()
}

/// Normalised level on the scale `[FLOOR_DB, ceil]` through the display
/// curve; under the gate it is exactly 0.
fn display(db: f32, ceil: f32) -> f32 {
    if !db.is_finite() {
        return 0.;
    }
    let n = ((db - FLOOR_DB) / (ceil - FLOOR_DB)).clamp(0., 1.);
    let v = n.powf(GAMMA);
    if v < GATE { 0. } else { v }
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    at: Instant,
    /// Display level 0..=1, release already applied.
    level: f32,
}

#[derive(Clone, Debug, Default)]
pub struct LevelHistory {
    samples: VecDeque<Sample>,
    last_seq: Option<u64>,
    /// Visual ceiling in dBFS (see module docs); None before any reading.
    ceil: Option<f32>,
    /// Fixed clock for offscreen renders and tests.
    frozen: Option<Instant>,
    /// With a frozen clock, still ask for frames while something moves
    /// (the perf harness steps the clock itself; stills never set this).
    animated: bool,
    /// Last reading at or above [`VERY_LOUD_DB`].
    very_loud_at: Option<Instant>,
    orbit: Rc<RefCell<OrbitMotion>>,
}

impl LevelHistory {
    pub fn clear(&mut self) {
        let (frozen, animated) = (self.frozen, self.animated);
        *self = Self::default();
        self.frozen = frozen;
        self.animated = animated;
    }

    pub fn now(&self) -> Instant {
        self.frozen.unwrap_or_else(Instant::now)
    }

    /// Paints every frame as if it were `at` (gallery, tests).
    #[cfg_attr(not(any(test, feature = "screenshots")), allow(dead_code))]
    pub fn freeze_at(&mut self, at: Instant) {
        self.frozen = Some(at);
    }

    /// Steps a frozen clock to `at` and keeps requesting frames while the
    /// drawing moves, so a harness can drive the animation deterministically.
    #[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
    pub fn drive_at(&mut self, at: Instant) {
        self.frozen = Some(at);
        self.animated = true;
    }

    /// Whether the drawing may ask for another frame.
    fn wants_frames(&self) -> bool {
        self.frozen.is_none() || self.animated
    }

    /// Visible level right now: the newest reading or a decaying envelope.
    pub fn is_silent(&self, now: Instant) -> bool {
        self.current() <= 0. && self.envelope(now) <= 0.
    }

    fn ceiling(&self) -> f32 {
        self.ceil.unwrap_or(BASE_CEIL_DB)
    }

    /// Adds the reading of audio event `seq` that arrived at `at`. Repeated
    /// sequence numbers (snapshots sent for other reasons) are ignored.
    /// Returns whether a reading was added.
    pub fn push(&mut self, seq: u64, rms: f32, at: Instant) -> bool {
        if self.last_seq == Some(seq) {
            return false;
        }
        self.last_seq = Some(seq);
        let level_db = db(rms);
        if level_db >= VERY_LOUD_DB {
            self.very_loud_at = Some(at);
        }
        let prev = self.samples.back().copied();
        let dt = prev.map_or(0., |p| at.saturating_duration_since(p.at).as_secs_f32());
        let mut ceil = self.ceiling();
        ceil = (ceil - CEIL_RETURN_DB_PER_S * dt).max(BASE_CEIL_DB);
        if level_db.is_finite() {
            ceil = ceil.max((level_db + HEADROOM_DB).min(0.));
        }
        self.ceil = Some(ceil);
        let raw = display(level_db, ceil);
        let released = prev.map_or(0., |p| p.level * (-dt / RELEASE_S).exp());
        let mut level = raw.max(released);
        if level < 0.02 {
            level = 0.;
        }
        self.samples.push_back(Sample { at, level });
        while self
            .samples
            .front()
            .is_some_and(|s| at.saturating_duration_since(s.at) > KEEP)
        {
            self.samples.pop_front();
        }
        true
    }

    /// True for a moment after an RMS reading at or above -1 dBFS.
    pub fn very_loud(&self, now: Instant) -> bool {
        self.very_loud_at
            .is_some_and(|at| now.saturating_duration_since(at) <= VERY_LOUD_HOLD)
    }

    /// Newest reading, 0..=1 (reduced motion meter).
    pub fn current(&self) -> f32 {
        self.samples.back().map_or(0., |s| s.level)
    }

    /// Envelope at time `t`, 0..=1, continuous.
    pub fn envelope(&self, t: Instant) -> f32 {
        let Some(last) = self.samples.back() else {
            return 0.;
        };
        // Each reading is reached over [arrival, arrival + RAMP], starting
        // from the previous value; between ramps the value holds. The shape
        // depends on when readings arrive, never on how often.
        let key = |s: &Sample| s.at + RAMP;
        if t >= key(last) {
            let since = t.saturating_duration_since(last.at);
            if since <= STALE {
                return last.level;
            }
            let fade = 1. - (since - STALE).as_secs_f32() / STALE_FADE_S;
            return last.level * fade.max(0.);
        }
        let i = self.samples.partition_point(|s| key(s) <= t);
        let next = self.samples[i];
        let (from_t, from_v) = if i == 0 {
            (next.at, 0.)
        } else {
            let p = self.samples[i - 1];
            (key(&p).max(next.at), p.level)
        };
        if t <= from_t {
            return from_v;
        }
        let span = key(&next).saturating_duration_since(from_t).as_secs_f32();
        let f = if span <= 0. {
            1.
        } else {
            t.saturating_duration_since(from_t).as_secs_f32() / span
        };
        from_v + (next.level - from_v) * f.clamp(0., 1.)
    }

    /// Envelope `ago` seconds before `now`.
    fn at(&self, now: Instant, ago: f32) -> f32 {
        match now.checked_sub(Duration::from_secs_f32(ago.max(0.))) {
            Some(t) => self.envelope(t),
            None => 0.,
        }
    }

    /// Loudest envelope over the last `span` seconds (sampled).
    fn any_voice(&self, now: Instant, span: f32) -> bool {
        self.samples
            .iter()
            .any(|s| s.level > 0. && now.saturating_duration_since(s.at).as_secs_f32() < span + 0.5)
            || self.envelope(now) > 0.
    }
}

/// Element that paints the voice for the current style. `live` is false
/// while finalizing, which settles the drawing to rest.
pub fn voice(
    history: &LevelHistory,
    palette: Palette,
    live: bool,
    reduce_motion: bool,
) -> gpui::AnyElement {
    let history = history.clone();
    if palette.voice_style == VoiceStyle::Orbit {
        return orbit(history, palette, live, reduce_motion);
    }
    canvas(
        move |_, _, _| (),
        move |bounds, (), window, _| {
            perf::bump(&perf::VOICE_PAINTS);
            let now = history.now();
            let busy = if reduce_motion {
                paint_meter(bounds, &history, palette, live, window);
                false
            } else {
                match palette.voice_style {
                    VoiceStyle::Dots => paint_dots(bounds, &history, palette, live, now, window),
                    _ => paint_wave(bounds, &history, palette, live, now, window),
                }
                live && history.any_voice(now, DOTS_SPAN)
            };
            if busy && history.wants_frames() {
                perf::bump(&perf::FRAMES_REQUESTED);
                window.request_animation_frame();
            }
        },
    )
    .w_full()
    .h(px(VOICE_HEIGHT))
    .into_any_element()
}

fn paint_wave(
    bounds: Bounds<Pixels>,
    h: &LevelHistory,
    p: Palette,
    live: bool,
    now: Instant,
    window: &mut Window,
) {
    let w = f32::from(bounds.size.width);
    let mid = f32::from(bounds.size.height) / 2.;
    let max_amp = mid - 2.;
    // The carrier travels with real time, not with the number of readings.
    let phase = epoch_secs(now) * 7.;
    let mut builder = gpui::PathBuilder::stroke(px(1.8));
    let steps = (w / 2.5).max(8.) as usize;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let x = t * w;
        let taper = (PI * t).sin().max(0.).sqrt();
        let env = if live {
            h.at(now, (1. - t) * WAVE_SPAN).powf(0.8)
        } else {
            0.
        };
        let carrier = (x / 7.5 - phase).sin() * 0.7 + (x / 3.1 - phase * 1.7).sin() * 0.3;
        let y = mid + carrier * env * max_amp * taper;
        let pt = point(bounds.left() + px(x), bounds.top() + px(y));
        if s == 0 {
            builder.move_to(pt)
        } else {
            builder.line_to(pt)
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, p.voice);
    }
}

/// Seconds of a monotonic instant, for phases that must not jump.
fn epoch_secs(t: Instant) -> f32 {
    thread_local!(static EPOCH: Instant = Instant::now());
    EPOCH.with(|e| {
        if t >= *e {
            (t - *e).as_secs_f32()
        } else {
            -(*e - t).as_secs_f32()
        }
    }) % 1000.
}

fn lerp(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0., 1.);
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// Ink for the loudest dust: ink, then orange shade for clear speech, and
/// orange only near the top of the scale.
fn accent(p: Palette, level: f32) -> Rgba {
    let c = lerp(p.voice, p.orange_shade, smoothstep(0.55, 0.78, level));
    lerp(c, p.orange, smoothstep(0.88, 0.97, level))
}

/// Lit height in rows for a column, continuous: 0 in silence, `rows` only at
/// the top of the scale.
fn dots_height(level: f32, rows: usize) -> f32 {
    level.clamp(0., 1.) * rows as f32
}

const DOT_PITCH: f32 = 9.;
const DOT_ROW: f32 = 7.;
const DOT: f32 = 3.5;

fn paint_dots(
    bounds: Bounds<Pixels>,
    h: &LevelHistory,
    p: Palette,
    live: bool,
    now: Instant,
    window: &mut Window,
) {
    let w = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let rows = ((height + 2.) / DOT_ROW).floor() as usize;
    let cols = (w / DOT_PITCH).floor() as usize;
    let mut faint = p.text;
    faint.a = 0.14;
    let mut track = p.voice;
    track.a = 0.32;
    let per_col = DOTS_SPAN / cols.max(1) as f32;
    for c in 0..cols {
        let ago = (cols - 1 - c) as f32 * per_col;
        // A column is the loudest moment of its own time slice, so a short
        // syllable is never skipped between two columns.
        let level = if live {
            (0..3)
                .map(|k| h.at(now, ago + per_col * (k as f32 - 1.) / 2.))
                .fold(0., f32::max)
        } else {
            0.
        };
        let lit = dots_height(level, rows);
        for r in 0..rows {
            // The bottom row is a quiet baseline; it lights only with signal.
            let base = if r == 0 { track } else { faint };
            let fill_amount = (lit - r as f32).clamp(0., 1.);
            let top = lit <= r as f32 + 1.;
            let ink = if top { accent(p, level) } else { p.voice };
            let color = lerp(base, ink, fill_amount);
            // The row being reached grows with the level instead of popping.
            let d = if fill_amount > 0. && fill_amount < 1. {
                DOT * (0.7 + 0.3 * fill_amount)
            } else {
                DOT
            };
            let x = bounds.left() + px(c as f32 * DOT_PITCH + (DOT_PITCH - d) / 2.);
            let y = bounds.bottom() - px((r as f32 + 1.) * DOT_ROW) + px((DOT - d) / 2.);
            window.paint_quad(
                fill(Bounds::new(point(x, y), size(px(d), px(d))), color).corner_radii(px(d / 2.)),
            );
        }
    }
}

/// Reduced motion: a static baseline with a filled bar for the newest level.
/// Repaints only when a reading arrives; never asks for frames.
fn paint_meter(
    bounds: Bounds<Pixels>,
    h: &LevelHistory,
    p: Palette,
    live: bool,
    window: &mut Window,
) {
    let mid = bounds.top() + bounds.size.height / 2.;
    let mut track = p.voice;
    track.a = 0.2;
    window.paint_quad(
        fill(
            Bounds::new(
                point(bounds.left(), mid - px(2.)),
                size(bounds.size.width, px(4.)),
            ),
            track,
        )
        .corner_radii(px(2.)),
    );
    let level = if live { h.current() } else { 0. };
    if level <= 0. {
        return;
    }
    let width = bounds.size.width * level.max(0.02);
    window.paint_quad(
        fill(
            Bounds::new(point(bounds.left(), mid - px(3.)), size(width, px(6.))),
            p.voice,
        )
        .corner_radii(px(3.)),
    );
}

// ---------------------------------------------------------------- Órbita

/// Height of the Órbita row and the width of its drawing.
pub const ORBIT_H: f32 = 64.;
const ORBIT_W: f32 = 176.;
const MARS_R: f32 = 11.;
/// Rings: rest radius (horizontal), dust grains, speed factor, how late the
/// ring hears the voice (the voice ripples outward from Mars).
const RINGS: [(f32, usize, f32, f32); 3] = [
    (27., 22, 1.0, 0.0),
    (43., 34, 0.72, 0.16),
    (60., 46, 0.5, 0.32),
];
/// Ring perspective (vertical/horizontal) and tilt.
const ORBIT_FLAT: f32 = 0.31;
const ORBIT_TILT: f32 = -0.05;
/// How far the loudest voice pushes a ring out, as a fraction of its radius.
const ORBIT_LIFT: f32 = 0.28;
/// Turn speed at full level, rad/s, and the time it takes to settle.
const ORBIT_SPEED: f32 = 1.3;
const ORBIT_INERTIA: f32 = 0.25;

/// Moving part of the rings. Phases only advance through
/// [`OrbitMotion::step`], which follows the real envelope.
#[derive(Debug, Default)]
pub struct OrbitMotion {
    omega: [f32; 3],
    phi: [f32; 3],
    last: Option<Instant>,
    /// Grains computed by the back layer for the front layer of this frame.
    front: Vec<Grain>,
}

#[derive(Clone, Copy, Debug)]
struct Grain {
    x: f32,
    y: f32,
    r: f32,
    color: Rgba,
}

impl OrbitMotion {
    /// Advances by `dt` seconds with each ring's envelope. Returns true while
    /// any ring still turns.
    pub fn step(&mut self, env: [f32; 3], dt: f32) -> bool {
        let mut moving = false;
        for k in 0..3 {
            let wanted = ORBIT_SPEED * RINGS[k].2 * env[k];
            self.omega[k] += (wanted - self.omega[k]) * (1. - (-dt / ORBIT_INERTIA).exp());
            if wanted == 0. && self.omega[k] < 0.004 {
                self.omega[k] = 0.;
            }
            self.phi[k] = (self.phi[k] + self.omega[k] * dt) % TAU;
            moving |= self.omega[k] > 0. || env[k] > 0.;
        }
        moving
    }
}

/// Mars at the centre with three tilted rings of regolith dust. In silence
/// the rings rest, still and faint. Voice swells the inner ring first and
/// ripples outward; rings turn only while there is voice.
fn orbit(h: LevelHistory, p: Palette, live: bool, reduce: bool) -> gpui::AnyElement {
    let back_h = h.clone();
    let front = h.orbit.clone();
    div()
        .relative()
        .w_full()
        .h(px(ORBIT_H))
        .flex()
        .justify_center()
        .child(
            div()
                .relative()
                .w(px(ORBIT_W))
                .h_full()
                .child(
                    canvas(
                        move |_, _, _| (),
                        move |bounds, (), window, _| {
                            perf::bump(&perf::VOICE_PAINTS);
                            if paint_orbit_back(bounds, &back_h, p, live, reduce, window)
                                && back_h.wants_frames()
                            {
                                perf::bump(&perf::FRAMES_REQUESTED);
                                window.request_animation_frame();
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .child(
                    img(assets::art(assets::mars(MARS_R * 2.)))
                        .absolute()
                        .left(px(ORBIT_W / 2. - MARS_R))
                        .top(px(ORBIT_H / 2. - MARS_R))
                        .size(px(MARS_R * 2.)),
                )
                .child(
                    canvas(
                        move |_, _, _| (),
                        move |_, (), window, _| {
                            for g in front.borrow_mut().front.drain(..) {
                                paint_grain(g, window);
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                ),
        )
        .into_any_element()
}

fn paint_grain(g: Grain, window: &mut Window) {
    window.paint_quad(
        fill(
            Bounds::new(
                point(px(g.x - g.r), px(g.y - g.r)),
                size(px(g.r * 2.), px(g.r * 2.)),
            ),
            g.color,
        )
        .corner_radii(px(g.r)),
    );
}

/// Ring envelopes: each ring hears the voice a little later.
fn ring_envelopes(h: &LevelHistory, now: Instant, live: bool, reduce: bool) -> [f32; 3] {
    if !live {
        return [0.; 3];
    }
    if reduce {
        return [h.current(); 3];
    }
    RINGS.map(|(_, _, _, lag)| h.at(now, lag))
}

/// Paints the far half of the rings, stores the near half for the front
/// layer. Returns whether another frame is needed.
fn paint_orbit_back(
    bounds: Bounds<Pixels>,
    h: &LevelHistory,
    p: Palette,
    live: bool,
    reduce: bool,
    window: &mut Window,
) -> bool {
    let now = h.now();
    let env = ring_envelopes(h, now, live, reduce);
    let mut m = h.orbit.borrow_mut();
    let busy = if reduce {
        m.last = None;
        false
    } else {
        let dt = m.last.map_or(0., |l| {
            now.saturating_duration_since(l).as_secs_f32().min(0.05)
        });
        let turning = m.step(env, dt);
        // Keep frames while a later ring still has voice to hear.
        let busy = turning || (live && h.any_voice(now, 0.4));
        m.last = busy.then_some(now);
        busy
    };
    let cx = f32::from(bounds.left()) + ORBIT_W / 2.;
    let cy = f32::from(bounds.top()) + ORBIT_H / 2.;
    let (st, ct) = ORBIT_TILT.sin_cos();
    m.front.clear();
    let mut back = Vec::new();
    // A slow swell travels around each ring while it hears the voice, so
    // the dust breathes without random jitter.
    let swell_t = epoch_secs(now) * 1.6;
    for (k, &(radius, n, _, _)) in RINGS.iter().enumerate() {
        let e = env[k];
        let color_level = e * (1. - 0.18 * k as f32);
        let mut color = accent(p, color_level);
        for i in 0..n {
            let theta = i as f32 / n as f32 * TAU + m.phi[k];
            let swell = 0.75 + 0.25 * (3. * theta + swell_t + k as f32 * 1.9).sin();
            let lift = 1. + e * ORBIT_LIFT * swell;
            let (s, c) = theta.sin_cos();
            let ex = radius * lift * c;
            let ey = radius * ORBIT_FLAT * lift * s;
            let x = cx + ex * ct - ey * st;
            let y = cy + ex * st + ey * ct;
            let near = s > 0.;
            // Mars hides the far grains behind it.
            if !near && (x - cx).hypot(y - cy) < MARS_R + 1.2 {
                continue;
            }
            let depth = (s + 1.) / 2.;
            let rest = 0.24 + 0.3 * depth;
            color.a = rest + (1. - rest) * (e * 1.4).min(1.);
            let r = 0.8 + 0.45 * depth + 0.75 * e;
            let g = Grain { x, y, r, color };
            if near {
                m.front.push(g);
            } else {
                back.push(g);
            }
        }
    }
    drop(m);
    for g in back {
        paint_grain(g, window);
    }
    busy
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// Feeds `rms` readings every `every` ms starting at `from`; returns the
    /// next time and sequence number.
    fn speak(
        h: &mut LevelHistory,
        start: Instant,
        (from, seq): (u64, u64),
        rms: f32,
        n: u64,
        every: u64,
    ) -> (u64, u64) {
        for i in 0..n {
            h.push(seq + i, rms, start + MS(from + i * every));
        }
        (from + n * every, seq + n)
    }

    #[test]
    fn repeated_sequence_numbers_add_nothing() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        assert!(h.push(1, 0.1, start));
        // Partials and clock snapshots repeat the last level and sequence.
        for k in 1..5 {
            assert!(!h.push(1, 0.1, start + MS(10 * k)));
        }
        assert_eq!(h.samples.len(), 1);
        assert!(h.push(2, 0.1, start + MS(100)));
        assert_eq!(h.samples.len(), 2);
    }

    #[test]
    fn silence_and_room_noise_draw_nothing() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        // -66 dBFS hiss, zeros, NaN and negatives.
        let mut at = (0, 0);
        for rms in [0.0005, 0., f32::NAN, -0.3, 0.001] {
            at = speak(&mut h, start, at, rms, 10, 50);
        }
        for ms in (0..at.0 + 500).step_by(16) {
            assert_eq!(h.envelope(start + MS(ms)), 0.);
        }
        assert!(!h.any_voice(start + MS(at.0), DOTS_SPAN));
    }

    #[test]
    fn normal_and_loud_speech_stay_apart_without_saturating() {
        let level = |rms: f32| {
            let start = Instant::now();
            let mut h = LevelHistory::default();
            speak(&mut h, start, (0, 0), rms, 10, 100);
            h.current()
        };
        // Quiet (-40), normal (-26), firm (-20) and loud (-12 dBFS) speech.
        let quiet = level(0.01);
        let normal = level(0.05);
        let firm = level(0.1);
        let loud = level(0.25);
        assert!(quiet > 0.15, "quiet speech is visible: {quiet}");
        // Normal speech stays mid-height: the dots keep rows for louder.
        assert!(normal > 0.35 && normal < 0.6, "{normal}");
        assert!(
            normal > quiet + 0.15 && firm > normal + 0.1,
            "{quiet} {normal} {firm}"
        );
        assert!(loud > firm, "{firm} {loud}");
        assert!(loud < 0.95, "loud voice keeps headroom: {loud}");
    }

    #[test]
    fn a_loud_speaker_keeps_dynamics_through_the_auto_scale() {
        // Someone who peaks near full scale: their normal speech must not
        // light the top rows any more.
        let start = Instant::now();
        let mut h = LevelHistory::default();
        let mut at = speak(&mut h, start, (0, 0), 0.7, 10, 100);
        let shout = h.current();
        at = speak(&mut h, start, at, 0.1, 10, 100);
        let normal = h.current();
        assert!(shout > 0.85 && shout <= 1., "{shout}");
        assert!(normal < shout - 0.25, "{normal} vs {shout}");
        assert!(dots_height(normal, 5) < 4., "{normal}");
        // The ceiling relaxes slowly back towards the base.
        let raised = h.ceiling();
        speak(&mut h, start, at, 0.0, 60, 100);
        assert!(h.ceiling() < raised && h.ceiling() >= BASE_CEIL_DB);
    }

    #[test]
    fn a_near_full_scale_level_is_reported_and_loud_speech_is_not() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        speak(&mut h, start, (0, 0), 0.5, 10, 100);
        assert!(
            !h.very_loud(start + MS(1000)),
            "-6 dBFS is loud, not near full scale"
        );
        h.push(10, 0.95, start + MS(1000));
        assert!(h.very_loud(start + MS(1100)));
        assert!(!h.very_loud(start + MS(1000) + VERY_LOUD_HOLD + MS(10)));
        // RMS is not a sample peak: the badge must not claim clipping.
        for l in crate::i18n::Lang::ALL {
            let label = l.t(crate::i18n::Msg::VeryLoud).to_lowercase();
            assert!(
                !label.contains("satur") && !label.contains("clip"),
                "{label}"
            );
        }
    }

    #[test]
    fn the_envelope_is_smooth_between_readings_at_60_fps() {
        // 10 Hz readings alternating soft and loud: a 60 Hz frame must never
        // jump more than the ramp allows, unlike 100 ms steps.
        let start = Instant::now();
        let mut h = LevelHistory::default();
        for i in 0..20u64 {
            let rms = if i % 2 == 0 { 0.02 } else { 0.2 };
            h.push(i, rms, start + MS(100 * i));
        }
        let frames: Vec<f32> = (0..120)
            .map(|f| h.envelope(start + MS(200 + f * 1000 / 60)))
            .collect();
        let max_step = frames
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0., f32::max);
        let range =
            frames.iter().cloned().fold(0., f32::max) - frames.iter().cloned().fold(1., f32::min);
        assert!(range > 0.2, "{range}");
        assert!(max_step < range * 0.5, "{max_step} of {range}");
    }

    #[test]
    fn cadence_does_not_change_the_shape() {
        // The same 1 s phrase reported at 10 Hz and at 50 Hz.
        let shape = |every: u64| {
            let start = Instant::now();
            let mut h = LevelHistory::default();
            for (seq, ms) in (0..2000).step_by(every as usize).enumerate() {
                let rms = if (500..1500).contains(&ms) { 0.08 } else { 0. };
                h.push(seq as u64, rms, start + MS(ms));
            }
            (0..20)
                .map(|k| h.envelope(start + MS(100 * k)))
                .collect::<Vec<f32>>()
        };
        let (slow, fast) = (shape(100), shape(20));
        for (a, b) in slow.iter().zip(&fast) {
            assert!((a - b).abs() < 0.2, "{slow:?}\n{fast:?}");
        }
    }

    #[test]
    fn speech_attacks_fast_and_releases_back_to_true_silence() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        h.push(0, 0.1, start);
        assert!(
            h.envelope(start + RAMP) > 0.5,
            "first syllable shows within the ramp"
        );
        let peak = h.current();
        h.push(1, 0., start + MS(100));
        assert!(h.current() > 0. && h.current() < peak, "decays, no cut");
        speak(&mut h, start, (200, 2), 0., 10, 100);
        assert_eq!(h.current(), 0.);
    }

    #[test]
    fn a_stalled_stream_falls_to_silence() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        speak(&mut h, start, (0, 0), 0.1, 5, 100);
        assert!(h.envelope(start + MS(500)) > 0.);
        assert_eq!(h.envelope(start + MS(400) + STALE + MS(250)), 0.);
    }

    #[test]
    fn the_orbit_stands_still_in_silence() {
        let mut m = OrbitMotion::default();
        for _ in 0..120 {
            assert!(!m.step([0.; 3], 1. / 60.));
        }
        assert_eq!(m.phi, [0.; 3], "no movement without voice");
    }

    #[test]
    fn the_orbit_turns_with_voice_and_settles_after_it() {
        let mut m = OrbitMotion::default();
        for _ in 0..60 {
            assert!(m.step([0.6; 3], 1. / 60.));
        }
        assert!(m.phi[0] > 0.2, "{:?}", m.phi);
        assert!(m.phi[0] > m.phi[2], "inner rings turn faster");
        let mut frames = 0;
        while m.step([0.; 3], 1. / 60.) {
            frames += 1;
            assert!(frames < 120, "settles in under 2 s");
        }
        let stopped = m.phi;
        for _ in 0..30 {
            m.step([0.; 3], 1. / 60.);
        }
        assert_eq!(m.phi, stopped);
    }

    #[test]
    fn the_loudest_rings_stay_inside_their_row() {
        let (rx, _, _, _) = RINGS[2];
        let reach = rx * (1. + ORBIT_LIFT);
        let vertical = reach * ORBIT_FLAT + reach * ORBIT_TILT.abs() + 2.5;
        assert!(reach + 2.5 <= ORBIT_W / 2., "{reach}");
        assert!(vertical <= ORBIT_H / 2., "{vertical}");
    }

    #[test]
    fn voice_reaches_the_outer_ring_later() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        speak(&mut h, start, (0, 0), 0.0, 5, 100);
        speak(&mut h, start, (500, 5), 0.2, 2, 100);
        let env = ring_envelopes(&h, start + MS(700), true, false);
        assert!(env[0] > env[2], "{env:?}");
        let later = ring_envelopes(&h, start + MS(950), true, false);
        assert!(later[2] > env[2], "{env:?} {later:?}");
    }
}
