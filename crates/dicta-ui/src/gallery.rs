//! Offscreen gallery: renders the real overlay and main window with the GPUI
//! wgpu headless renderer and saves PNGs, without opening windows on the
//! desktop. Built only with `--features screenshots`. Every scene is marked
//! DEMO and uses invented notes, levels and numbers; nothing is recorded.
//!
//! Scenes: the six style × colour combinations (overlay idle and recording,
//! Inicio), Historial light and dark, Apariencia, Historial y audio, and
//! animation frames of each voice style on synthetic levels, and `resp-*`
//! responsive checks at 720–1364 px (`DICTA_GALLERY_FILTER=resp-`).
//!
//! Read aloud (`DICTA_GALLERY_FILTER=lector-,lectura`): the reader window in
//! every state (`lector-*`: reading, compact, confirm, error, generating,
//! waiting for audio, paused, finished, failed, selection, a 20 000
//! character text, both panels, queue, reduced motion) for the three styles
//! in light and dark, Ajustes → Lectura, Historial → Lecturas and Inicio
//! with reading metrics. Every text, voice, cost and level is invented.
//!
//! Escritura (`DICTA_GALLERY_FILTER=escritura`): the window fixing (diff
//! and explanations) in the six style × colour combinations; translating,
//! working, re-running, copying the selection, the language menu and an
//! error in Señal claro and Órbita oscuro; Ajustes → Escritura. The texts
//! are invented examples of a Spanish speaker writing English.
//!
//! Languages (`DICTA_GALLERY_FILTER=i18n-`): the same overlay, reader,
//! Proveedor (with the subscription consent), Lectura, General and Inicio
//! in Spanish and English, `i18n-es-*` and `i18n-en-*`. The English set uses
//! English DEMO content and statuses, as the controller will send once it
//! has message ids; today its status text is Spanish.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::Context as _;
use dicta_core::{
    AppConfig, AppSnapshot, AudioDevice, AudioRetention, ColorMode, HistoryKind, HistoryMetrics,
    Note, NoteStatus, Phase, ProviderMode, Reading, ReadingMetrics, Theme,
    tts::{
        TtsConfirm, TtsPhase, TtsSnapshot, TtsVersion, builtin_voices, estimate_ms, estimate_usd,
        proportional_words, sentence_at, split_sentences, word_at,
    },
};
use gpui::{AppContext as _, HeadlessAppContext, px, size};

use crate::{
    Hub, Lang, OVERLAY_WIDTH, OverlayPlacement, READER_WIDTH, UiState, WRITER_MAX_HEIGHT,
    WRITER_WIDTH, assets,
    overlay::Overlay,
    overlay_height,
    reader::{Popover, Reader, reader_height},
    settings::{Consent, Section, Settings},
    text_field,
    theme::Palette,
    wave::LevelHistory,
    writer::Writer,
};
use dicta_core::writer::{
    DiffSpan, WriterChange, WriterMode, WriterOutput, WriterPhase, WriterSnapshot,
};

const NOW_MS: u64 = 1_791_550_800_000;
const MIN: u64 = 60_000;

fn note(id: i64, ago_min: u64, text: &str, captured_s: u64) -> Note {
    Note {
        id,
        created_at_ms: NOW_MS - ago_min * MIN,
        original: text.into(),
        text: text.into(),
        word_count: text.split_whitespace().count() as u64,
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

/// Invented DEMO notes.
fn notes() -> Vec<Note> {
    let mut first = note(
        41,
        12,
        "DEMO. Mañana a las diez revisamos el kernel nuevo en Hyprland y después vemos por qué \
         el overlay no se arrastra en el monitor externo.\nAnotá también lo del atajo.",
        19,
    );
    first.original = first.text.replace("Hyprland", "jiperland");
    first.audio_filename = Some("demo.wav".into());
    first.audio_bytes = 612_000;
    first.input_tokens = Some(412);
    first.output_tokens = Some(96);
    let mut failed = note(
        39,
        95,
        "DEMO. Quedó a medias el mensaje para el equipo de",
        7,
    );
    failed.status = NoteStatus::Failed;
    failed.error = Some("DEMO: se cortó la conexión con Grok antes del final.".into());
    vec![
        first,
        note(
            40,
            48,
            "DEMO. Comprar yerba, pilas AA y cinta para el micrófono.",
            5,
        ),
        failed,
        note(
            38,
            60 * 26,
            "DEMO. Idea para el post: dictar es más rápido que escribir cuando ya sabés qué \
             querés decir.",
            9,
        ),
        note(
            37,
            60 * 50,
            "DEMO. Recordar renovar el dominio antes del viernes.",
            4,
        ),
        note(
            36,
            60 * 24 * 6,
            "DEMO. Lista de pendientes de la semana.",
            11,
        ),
    ]
}

/// Invented DEMO text for the reader: three short paragraphs.
const READ_TEXT: &str = "DEMO. Marte tiene el volcán más alto del sistema solar: el monte Olimpo \
mide unos 22 kilómetros, casi tres veces el Everest. Su base ocupa una superficie parecida a la \
de Arizona.\n\nEl planeta debe su color al óxido de hierro del regolito, el polvo fino que lo \
cubre casi todo. Las tormentas de polvo pueden durar meses y envolver el planeta entero.\n\nUn \
día marciano dura 24 horas y 39 minutos. Los equipos de la misión ajustan su reloj a ese ritmo \
y llaman «sol» a cada jornada de trabajo.";

/// About 20 000 characters of invented DEMO paragraphs.
fn long_read_text() -> String {
    let parts = [
        "La sonda midió el viento durante la tarde y encontró ráfagas más fuertes de lo previsto.",
        "Los ingenieros revisaron los paneles, limpiaron el polvo acumulado y anotaron cada cambio.",
        "Nadie esperaba que el cráter guardara hielo tan cerca de la superficie, pero los datos insistían.",
        "Al caer el sol, la temperatura bajó cerca de ochenta grados en pocas horas.",
    ];
    let mut text = String::new();
    let mut n = 1;
    while text.chars().count() < 19_600 {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!("DEMO {n}. "));
        for k in 0..4 {
            text.push_str(parts[(n + k) % parts.len()]);
            text.push(' ');
        }
        text.pop();
        n += 1;
    }
    text
}

/// A reading of `text`: `played` and `generated` are fractions of the
/// estimated duration. Voices, versions, costs and levels are invented.
fn tts(text: &str, phase: TtsPhase, played: f32, generated: f32) -> TtsSnapshot {
    let text = text.to_string();
    let sentences = split_sentences(&text);
    let chars = text.chars().count() as u64;
    let total = estimate_ms(chars as usize, 1.0);
    let words = proportional_words(&text, total);
    let position = (total as f32 * played) as u64;
    let word = (played > 0.).then(|| word_at(&words, position)).flatten();
    let sentence = word.and_then(|w| sentence_at(&sentences, words[w].span.start));
    let sent = ((chars as f32) * generated.clamp(0.35, 1.)) as u64;
    TtsSnapshot {
        phase,
        show: true,
        reading: 7,
        text: Arc::new(text),
        sentences: Arc::new(sentences),
        words: Arc::new(words),
        generated: Arc::new(if generated > 0. {
            vec![(0, (total as f32 * generated) as u64)]
        } else {
            vec![]
        }),
        position_ms: position,
        total_ms: total,
        total_exact: generated >= 1.,
        generating: generated < 1.,
        current_sentence: sentence,
        current_word: word,
        rate: 1.25,
        volume: 0.9,
        voice: "carina".into(),
        versions: vec![TtsVersion {
            id: 1,
            voice: "carina".into(),
            label: "Carina · original".into(),
        }],
        active_version: 1,
        voices: Arc::new(builtin_voices()),
        chars,
        chars_sent: sent,
        estimated_usd: estimate_usd(sent),
        account_ready: true,
        outputs: outputs(),
        ..Default::default()
    }
}

fn outputs() -> Vec<AudioDevice> {
    vec![
        AudioDevice {
            id: "alsa_output.pci-0000_0c_00.4.analog-stereo".into(),
            name: "Parlantes integrados".into(),
        },
        AudioDevice {
            id: "bluez_output.AA_BB.1".into(),
            name: "Auriculares Bluetooth".into(),
        },
    ]
}

fn reading(id: i64, ago_min: u64, text: &str, voice: &str, duration_s: u64) -> Reading {
    let chars = text.chars().count() as u64;
    Reading {
        id,
        created_at_ms: NOW_MS - ago_min * MIN,
        text: text.into(),
        chars,
        chars_sent: chars,
        duration_ms: duration_s * 1000,
        voice: voice.into(),
        language: "auto".into(),
        provider: ProviderMode::Subscription,
        status: NoteStatus::Completed,
        error: None,
        audio_filename: None,
        audio_bytes: 0,
    }
}

/// Invented DEMO readings.
fn readings() -> Vec<Reading> {
    let mut first = reading(21, 8, READ_TEXT, "carina", 38);
    first.chars_sent = first.chars * 2;
    first.voice = "eve".into();
    first.audio_filename = Some("tts-21.wav".into());
    first.audio_bytes = 1_824_000;
    let mut failed = reading(
        19,
        130,
        "DEMO. Resumen del artículo sobre baterías de estado sólido que quedó a medias.",
        "carina",
        6,
    );
    failed.status = NoteStatus::Failed;
    failed.error = Some(
        "DEMO: la suscripción no autorizó la lectura. Probá con la clave de API en Ajustes → \
         Lectura."
            .into(),
    );
    vec![
        first,
        reading(
            20,
            55,
            "DEMO. Mañana hay corte de luz programado entre las diez y las doce en todo el barrio.",
            "carina",
            7,
        ),
        failed,
        reading(
            18,
            60 * 27,
            "DEMO. Capítulo tres: el regreso. La nave tardó siete meses en volver y nadie habló del \
             viaje durante la primera semana.",
            "ara",
            12,
        ),
        reading(
            17,
            60 * 24 * 4,
            "DEMO. Instrucciones del horno: precalentar a 200 grados y hornear veinte minutos.",
            "rex",
            6,
        ),
    ]
}

/// Invented English DEMO text for the reader.
const READ_TEXT_EN: &str = "DEMO. Mars has the tallest volcano in the solar system: Olympus \
Mons rises about 22 kilometers, nearly three times Everest. Its base covers an area close to \
that of Arizona.\n\nThe planet owes its color to the iron oxide in the regolith, the fine dust \
that covers almost everything. Dust storms can last for months and wrap the whole planet.\n\nA \
Martian day lasts 24 hours and 39 minutes. Mission teams set their clocks to that rhythm and \
call each working day a \u{201c}sol\u{201d}.";

/// Invented English DEMO notes, the same shapes as [`notes`].
fn notes_en() -> Vec<Note> {
    let mut list = notes();
    let texts = [
        "DEMO. Tomorrow at ten we review the new kernel on Hyprland, then look at why the \
         overlay won't drag on the external monitor.\nAlso note the shortcut thing.",
        "DEMO. Buy coffee, AA batteries and tape for the microphone.",
        "DEMO. The message for the team was cut halfway through, about the",
        "DEMO. Idea for the post: dictating beats typing once you know what you want to say.",
        "DEMO. Remember to renew the domain before Friday.",
        "DEMO. To-do list for the week.",
    ];
    for (n, text) in list.iter_mut().zip(texts) {
        n.text = text.into();
        n.original = text.into();
        n.word_count = text.split_whitespace().count() as u64;
    }
    list[0].original = list[0].text.replace("Hyprland", "hyper land");
    list[2].error = Some("DEMO: the connection to Grok dropped before the end.".into());
    list
}

/// Spanish or English DEMO content for the language shots.
fn localize_demo(s: &mut AppSnapshot, lang: Lang) {
    if lang == Lang::Es {
        return;
    }
    s.status = "DEMO example".into();
    s.notes = notes_en().into();
    s.devices = vec![AudioDevice {
        id: "alsa_input.usb-mic".into(),
        name: "USB microphone".into(),
    }];
    s.tts.outputs = vec![
        AudioDevice {
            id: "alsa_output.pci-0000_0c_00.4.analog-stereo".into(),
            name: "Built-in speakers".into(),
        },
        AudioDevice {
            id: "bluez_output.AA_BB.1".into(),
            name: "Bluetooth headphones".into(),
        },
    ];
}

/// Synthetic output RMS of a voice reading: syllables with short pauses.
fn reading_rms(t: f32) -> f32 {
    if t % 1.6 > 1.35 {
        0.0008
    } else {
        0.03 + 0.11 * ((t * 9.).sin().abs())
    }
}

fn reading_levels(until: f32, start: Instant) -> LevelHistory {
    let mut h = LevelHistory::default();
    let mut ms = 0;
    let mut seq = 0;
    while (ms as f32) / 1000. <= until {
        h.push(
            seq,
            reading_rms(ms as f32 / 1000.),
            start + Duration::from_millis(ms),
        );
        seq += 1;
        ms += 50;
    }
    h.freeze_at(start + Duration::from_secs_f32(until));
    h
}

/// Invented DEMO input for Escritura: English written by a Spanish speaker.
const WRITE_INPUT: &str = "DEMO. Yesterday I go to the store and buyed two apple. I am agree with you that is very cheap.";

/// The fixed text with its diff and explanations, Spanish or English.
fn writer_fixed(lang: Lang) -> WriterSnapshot {
    let why: [&str; 5] = match lang {
        Lang::Es => [
            "Con yesterday la acción ya pasó: va el pasado, went.",
            "Buy es irregular: su pasado es bought.",
            "Con two, el sustantivo va en plural.",
            "Agree ya es un verbo: no lleva am.",
            "En inglés la oración necesita sujeto: it is, o it's.",
        ],
        Lang::En => [
            "Yesterday puts it in the past: use the past tense, went.",
            "Buy is irregular: its past tense is bought.",
            "After two, the noun is plural.",
            "Agree is already a verb, so it takes no am.",
            "English sentences need a subject: it is, or it's.",
        ],
    };
    let change = |original: &str, corrected: &str, why: &str| WriterChange {
        original: original.into(),
        corrected: corrected.into(),
        why: why.into(),
    };
    let same = |t: &str| DiffSpan::Same(t.into());
    let removed = |t: &str| DiffSpan::Removed(t.into());
    let added = |t: &str| DiffSpan::Added(t.into());
    WriterSnapshot {
        phase: WriterPhase::Ready,
        request_id: 4,
        mode: WriterMode::Auto,
        target_language: "en".into(),
        input: WRITE_INPUT.into(),
        output: Some(WriterOutput {
            mode: WriterMode::Fix,
            detected_language: "en".into(),
            text: "DEMO. Yesterday I went to the store and bought two apples. I agree with you \
                   that it's very cheap."
                .into(),
            changes: vec![
                change("go", "went", why[0]),
                change("buyed", "bought", why[1]),
                change("apple", "apples", why[2]),
                change("am agree", "agree", why[3]),
                change("is", "it's", why[4]),
            ],
        }),
        diff: vec![
            same("DEMO. Yesterday I "),
            removed("go"),
            added("went"),
            same(" to the store and "),
            removed("buyed"),
            added("bought"),
            same(" two "),
            removed("apple."),
            added("apples."),
            same(" I "),
            removed("am "),
            same("agree with you that "),
            removed("is"),
            added("it's"),
            same(" very cheap."),
        ],
        status: match lang {
            Lang::Es => "Copiado al portapapeles",
            Lang::En => "Copied to the clipboard",
        }
        .into(),
        input_tokens: Some(412),
        output_tokens: Some(96),
        can_replace: true,
        ..Default::default()
    }
}

/// A Spanish message translated to English.
fn writer_translated() -> WriterSnapshot {
    WriterSnapshot {
        phase: WriterPhase::Ready,
        request_id: 5,
        mode: WriterMode::Auto,
        target_language: "en".into(),
        input: "DEMO. Mañana tengo una reunión con el equipo a las diez, ¿me avisás si llegás \
                tarde?"
            .into(),
        output: Some(WriterOutput {
            mode: WriterMode::Translate,
            detected_language: "es".into(),
            text: "DEMO. I have a meeting with the team tomorrow at ten. Can you let me know \
                   if you'll be late?"
                .into(),
            changes: Vec::new(),
        }),
        status: "Copiado al portapapeles".into(),
        input_tokens: Some(388),
        output_tokens: Some(41),
        can_replace: true,
        ..Default::default()
    }
}

fn base(theme: Theme, mode: ColorMode) -> AppSnapshot {
    let config = AppConfig {
        theme,
        color_mode: mode,
        audio_retention: AudioRetention::Days7,
        ..Default::default()
    };
    let mut s = AppSnapshot::new(config);
    s.demo = true;
    s.authenticated = true;
    s.status = "Ejemplo DEMO".into();
    s.devices = vec![AudioDevice {
        id: "alsa_input.usb-mic".into(),
        name: "Micrófono USB".into(),
    }];
    s.notes = notes().into();
    s.metrics = HistoryMetrics {
        dictations: 6,
        words: 1_284,
        captured_ms: 9 * MIN + 32_000,
        sent_ms: 9 * MIN + 4_000,
        estimated_stt_usd: 0.2 * 9.07 / 60.,
        input_tokens: Some(412),
        output_tokens: Some(96),
        audio_bytes: 612_000,
    };
    s.readings = readings().into();
    s.reading_metrics = ReadingMetrics {
        readings: 5,
        chars_sent: 18_420,
        duration_ms: 21 * MIN + 14_000,
        estimated_tts_usd: estimate_usd(18_420),
        audio_bytes: 1_824_000,
    };
    s.tts.account_ready = true;
    s.tts.outputs = outputs();
    s
}

/// Synthetic RMS for a gallery voice: silence, then soft-to-normal
/// syllables, then a loud phrase. `t` in seconds.
fn demo_rms(t: f32) -> f32 {
    if t < 0.6 {
        0.0006
    } else if t < 2.4 {
        0.012 + 0.05 * ((t * 7.).sin().abs())
    } else {
        0.06 + 0.22 * ((t * 5.).sin().abs())
    }
}

fn levels(until: f32, every_ms: u64, start: Instant) -> LevelHistory {
    let mut h = LevelHistory::default();
    let mut seq = 0;
    let mut ms = 0;
    while (ms as f32) / 1000. <= until {
        h.push(
            seq,
            demo_rms(ms as f32 / 1000.),
            start + Duration::from_millis(ms),
        );
        seq += 1;
        ms += every_ms;
    }
    h.freeze_at(start + Duration::from_secs_f32(until));
    h
}

fn hub(snap: AppSnapshot, levels: LevelHistory, cx: &mut gpui::App) -> gpui::Entity<Hub> {
    let (tx, _rx) = mpsc::channel();
    cx.new(|_| {
        let mut hub = Hub::new(snap, tx);
        hub.levels = levels;
        hub
    })
}

fn capture(
    cx: &mut HeadlessAppContext,
    window: gpui::AnyWindowHandle,
    path: &Path,
) -> anyhow::Result<()> {
    cx.run_until_parked();
    cx.update(|cx| cx.update_window(window, |_, window, _| window.refresh()))?;
    cx.run_until_parked();
    // Images decode on the background executor; draw once more after they land.
    cx.update(|cx| cx.update_window(window, |_, window, _| window.refresh()))?;
    cx.run_until_parked();
    let image = cx.capture_screenshot(window)?;
    image
        .save(path)
        .with_context(|| format!("guardando {}", path.display()))?;
    Ok(())
}

fn slug(theme: Theme) -> &'static str {
    match theme {
        Theme::Signal | Theme::Night => "senal",
        Theme::Regolith => "regolito",
        Theme::Orbit => "orbita",
    }
}

fn mode_slug(mode: ColorMode) -> &'static str {
    match mode {
        ColorMode::Light => "claro",
        ColorMode::Dark => "oscuro",
    }
}

pub fn render_gallery(out: &Path) -> anyhow::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out)?;
    let text = Arc::new(gpui_wgpu::CosmicTextSystem::new("Noto Sans"));
    let mut cx = HeadlessAppContext::with_platform(
        text,
        Arc::new(assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| -> anyhow::Result<()> {
        assets::load_fonts(cx)?;
        cx.set_global(UiState {
            // Deterministic shots: the Spanish set shows the Hyprland hints.
            session: crate::session::Session::Hyprland,
            ..UiState::new(
                Palette::new(Theme::Signal, ColorMode::Light),
                crate::Lang::Es,
            )
        });
        text_field::bind_keys(cx);
        Ok(())
    })?;
    // DICTA_GALLERY_FILTER=orbita,historial renders only the shots whose
    // file name contains one of the parts.
    let filter: Vec<String> = std::env::var("DICTA_GALLERY_FILTER")
        .map(|v| {
            v.split(',')
                .map(|f| f.trim().to_owned())
                .filter(|f| !f.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let wanted = |file: &str| filter.is_empty() || filter.iter().any(|f| file.contains(f));
    let mut written = Vec::new();
    let start = Instant::now();

    let overlay_shot = |cx: &mut HeadlessAppContext,
                        name: String,
                        snap: AppSnapshot,
                        levels: LevelHistory,
                        written: &mut Vec<PathBuf>|
     -> anyhow::Result<()> {
        if !wanted(&name) {
            return Ok(());
        }
        let height = overlay_height(&snap);
        let hub = cx.update(|cx| hub(snap, levels, cx));
        let placement = OverlayPlacement::from_config(&AppConfig::default());
        let window = cx.open_window(size(px(OVERLAY_WIDTH), px(height)), |window, cx| {
            cx.new(|cx| Overlay::new(hub, placement, window, cx))
        })?;
        let path = out.join(format!("{name}.png"));
        capture(cx, window.into(), &path)?;
        written.push(path);
        Ok(())
    };

    for mode in [ColorMode::Light, ColorMode::Dark] {
        for theme in Theme::ALL {
            let tag = format!("{}-{}", slug(theme), mode_slug(mode));
            cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(theme, mode));
            let mut idle = base(theme, mode);
            idle.status = "Listo".into();
            overlay_shot(
                &mut cx,
                format!("overlay-{tag}-reposo"),
                idle,
                LevelHistory::default(),
                &mut written,
            )?;
            let mut rec = base(theme, mode);
            rec.phase = Phase::Recording;
            rec.elapsed_ms = 4_200;
            rec.confirmed = "Che, mañana a las diez".into();
            rec.partial = "revisamos el kernel".into();
            overlay_shot(
                &mut cx,
                format!("overlay-{tag}-grabando"),
                rec,
                levels(2.2, 100, start),
                &mut written,
            )?;
            let name = format!("app-{tag}-inicio");
            if wanted(&name) {
                let hub = cx.update(|cx| hub(base(theme, mode), LevelHistory::default(), cx));
                let window = cx.open_window(size(px(1040.), px(720.)), |window, cx| {
                    cx.new(|cx| Settings::new(hub, window, cx).with_section(Section::Home))
                })?;
                let path = out.join(format!("{name}.png"));
                capture(&mut cx, window.into(), &path)?;
                written.push(path);
            }
        }
        // Historial on Señal, a note with audio selected.
        let name = format!("app-senal-{}-historial", mode_slug(mode));
        cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(Theme::Signal, mode));
        if wanted(&name) {
            let mut snap = base(Theme::Signal, mode);
            snap.selected_note = snap.notes.first().cloned();
            snap.active_note_id = Some(41);
            snap.notes_has_more = true;
            let hub = cx.update(|cx| hub(snap, LevelHistory::default(), cx));
            let window = cx.open_window(size(px(1040.), px(720.)), |window, cx| {
                cx.new(|cx| Settings::new(hub, window, cx).with_section(Section::History))
            })?;
            let path = out.join(format!("{name}.png"));
            capture(&mut cx, window.into(), &path)?;
            written.push(path);
        }
    }

    cx.update(|cx| {
        cx.global_mut::<UiState>().palette = Palette::new(Theme::Regolith, ColorMode::Light)
    });
    for (name, section) in [
        ("app-regolito-claro-apariencia", Section::Appearance),
        ("app-regolito-claro-historial-audio", Section::Data),
    ] {
        if !wanted(name) {
            continue;
        }
        let hub = cx.update(|cx| {
            hub(
                base(Theme::Regolith, ColorMode::Light),
                LevelHistory::default(),
                cx,
            )
        });
        let window = cx.open_window(size(px(1040.), px(720.)), |window, cx| {
            cx.new(|cx| Settings::new(hub, window, cx).with_section(section))
        })?;
        let path = out.join(format!("{name}.png"));
        capture(&mut cx, window.into(), &path)?;
        written.push(path);
    }

    // Responsive checks, `resp-<width>x<height>-…`: the minimum window
    // width, a mid width where Historial already shows one pane, and a wide
    // one. Render only these with DICTA_GALLERY_FILTER=resp-.
    struct Resp {
        w: f32,
        h: f32,
        theme: Theme,
        mode: ColorMode,
        section: Section,
        what: &'static str,
    }
    let resp = |w, h, theme, mode, section, what| Resp {
        w,
        h,
        theme,
        mode,
        section,
        what,
    };
    use ColorMode::{Dark, Light};
    use Theme::{Orbit, Regolith, Signal};
    let scenes = [
        resp(
            720.,
            540.,
            Signal,
            Light,
            Section::History,
            "historial-lista",
        ),
        resp(
            725.,
            900.,
            Signal,
            Light,
            Section::History,
            "historial-lista",
        ),
        resp(
            725.,
            900.,
            Signal,
            Dark,
            Section::History,
            "historial-detalle",
        ),
        resp(
            720.,
            540.,
            Regolith,
            Light,
            Section::History,
            "historial-detalle",
        ),
        resp(
            725.,
            900.,
            Orbit,
            Light,
            Section::History,
            "historial-confirmar",
        ),
        resp(
            880.,
            720.,
            Regolith,
            Dark,
            Section::History,
            "historial-detalle",
        ),
        resp(
            1040.,
            720.,
            Orbit,
            Dark,
            Section::History,
            "historial-split",
        ),
        resp(
            1364.,
            900.,
            Signal,
            Light,
            Section::History,
            "historial-split",
        ),
        resp(725., 900., Signal, Light, Section::Home, "inicio"),
        resp(725., 1300., Regolith, Dark, Section::Home, "inicio"),
        resp(725., 900., Orbit, Light, Section::Home, "inicio"),
        resp(720., 540., Orbit, Dark, Section::Home, "inicio"),
        resp(1364., 900., Regolith, Light, Section::Home, "inicio"),
        resp(1364., 900., Orbit, Dark, Section::Home, "inicio"),
        resp(725., 900., Signal, Light, Section::Appearance, "apariencia"),
        resp(725., 900., Orbit, Dark, Section::Appearance, "apariencia"),
        resp(
            1364.,
            900.,
            Regolith,
            Light,
            Section::Appearance,
            "apariencia",
        ),
        resp(
            725.,
            900.,
            Regolith,
            Light,
            Section::Data,
            "historial-audio",
        ),
    ];
    for r in scenes {
        let name = format!(
            "resp-{}x{}-{}-{}-{}",
            r.w,
            r.h,
            r.what,
            slug(r.theme),
            mode_slug(r.mode)
        );
        if !wanted(&name) {
            continue;
        }
        cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(r.theme, r.mode));
        let mut snap = base(r.theme, r.mode);
        let detail = r.what != "historial-lista" && r.section == Section::History;
        if r.section == Section::History {
            snap.notes_has_more = true;
            if detail {
                snap.selected_note = snap.notes.first().cloned();
                snap.active_note_id = Some(41);
            }
        }
        let confirm =
            (r.what == "historial-confirmar").then_some(crate::settings::Confirm::DeleteNote(41));
        let hub = cx.update(|cx| hub(snap, LevelHistory::default(), cx));
        let window = cx.open_window(size(px(r.w), px(r.h)), |window, cx| {
            cx.new(|cx| {
                let mut s = Settings::new(hub, window, cx).with_section(r.section);
                s.history_detail_open = detail;
                s.confirm = confirm;
                s
            })
        })?;
        let path = out.join(format!("{name}.png"));
        capture(&mut cx, window.into(), &path)?;
        written.push(path);
    }

    // ------------------------------------------------------------ lectura
    struct ReaderScene {
        name: String,
        snap: AppSnapshot,
        playing: bool,
        popover: Option<Popover>,
        choice: Option<&'static str>,
        reduce: bool,
    }
    let reader_shot = |cx: &mut HeadlessAppContext,
                       scene: ReaderScene,
                       written: &mut Vec<PathBuf>|
     -> anyhow::Result<()> {
        if !wanted(&scene.name) {
            return Ok(());
        }
        let orbit = cx.update(|cx| {
            cx.global_mut::<UiState>().reduce_motion = scene.reduce;
            cx.global::<UiState>().palette.voice_style == crate::theme::VoiceStyle::Orbit
        });
        let height = reader_height(&scene.snap, orbit);
        let levels = if scene.playing {
            reading_levels(2.4, start)
        } else {
            LevelHistory::default()
        };
        let hub = cx.update(|cx| {
            let hub = hub(scene.snap, LevelHistory::default(), cx);
            hub.update(cx, |h, _| h.tts_levels = levels);
            hub
        });
        let placement = OverlayPlacement::for_reader(&AppConfig::default());
        let (popover, choice) = (scene.popover, scene.choice);
        let window = cx.open_window(size(px(READER_WIDTH), px(height)), |window, cx| {
            cx.new(|cx| {
                let mut r = Reader::new(hub, placement, window, cx);
                if let Some(p) = popover {
                    r = r.with_popover(p);
                }
                if let Some(v) = choice {
                    r = r.with_voice_choice(v);
                }
                r
            })
        })?;
        let path = out.join(format!("{}.png", scene.name));
        capture(cx, window.into(), &path)?;
        cx.update(|cx| cx.global_mut::<UiState>().reduce_motion = false);
        written.push(path);
        Ok(())
    };
    let scene = |name: String, snap: AppSnapshot| ReaderScene {
        playing: snap.tts.phase == TtsPhase::Playing,
        name,
        snap,
        popover: None,
        choice: None,
        reduce: false,
    };
    let long = long_read_text();
    for mode in [ColorMode::Light, ColorMode::Dark] {
        for theme in Theme::ALL {
            let tag = format!("{}-{}", slug(theme), mode_slug(mode));
            cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(theme, mode));
            let with = |t: TtsSnapshot| {
                let mut s = base(theme, mode);
                s.tts = t;
                s
            };
            let playing = with(tts(READ_TEXT, TtsPhase::Playing, 0.42, 0.7));
            reader_shot(
                &mut cx,
                scene(format!("lector-{tag}-leyendo"), playing.clone()),
                &mut written,
            )?;
            let mut compact = playing.clone();
            compact.config.tts.compact = true;
            reader_shot(
                &mut cx,
                scene(format!("lector-{tag}-compacto"), compact),
                &mut written,
            )?;
            let mut confirm = with(tts(&long, TtsPhase::Confirm, 0., 0.));
            confirm.tts.confirm = Some(TtsConfirm {
                chars: 24_310,
                estimated_usd: estimate_usd(24_310),
                truncated: true,
            });
            confirm.tts.chars_sent = 0;
            reader_shot(
                &mut cx,
                scene(format!("lector-{tag}-confirmar"), confirm),
                &mut written,
            )?;
            let mut error = with(tts(READ_TEXT, TtsPhase::Error, 0.55, 0.6));
            error.tts.error = Some(
                "DEMO: Grok cortó la generación por cuota (429). Lo ya generado se puede \
                 escuchar; Regenerar sigue desde la frase actual cuando quieras."
                    .into(),
            );
            reader_shot(
                &mut cx,
                scene(format!("lector-{tag}-error"), error.clone()),
                &mut written,
            )?;
            let mut compact_error = error;
            compact_error.config.tts.compact = true;
            reader_shot(
                &mut cx,
                scene(format!("lector-{tag}-compacto-error"), compact_error),
                &mut written,
            )?;
            let reduced = ReaderScene {
                reduce: true,
                ..scene(format!("lector-{tag}-reducido"), playing)
            };
            reader_shot(&mut cx, reduced, &mut written)?;
        }
    }
    // Every other state, Señal claro.
    cx.update(|cx| {
        cx.global_mut::<UiState>().palette = Palette::new(Theme::Signal, ColorMode::Light)
    });
    let with = |t: TtsSnapshot| {
        let mut s = base(Theme::Signal, ColorMode::Light);
        s.tts = t;
        s
    };
    let mut largo = with(tts(&long, TtsPhase::Playing, 0.46, 0.5));
    largo.tts.status = "Fragmento 9 de 34".into();
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-largo".into(), largo),
        &mut written,
    )?;
    let mut generando = with(tts(READ_TEXT, TtsPhase::Connecting, 0., 0.));
    generando.tts.status = "Generando el primer fragmento con Grok…".into();
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-generando".into(), generando),
        &mut written,
    )?;
    let esperando = with(tts(READ_TEXT, TtsPhase::Buffering, 0.3, 0.3));
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-esperando".into(), esperando),
        &mut written,
    )?;
    let mut pausado = with(tts(READ_TEXT, TtsPhase::Paused, 0.62, 1.));
    pausado.tts.versions.push(TtsVersion {
        id: 2,
        voice: "eve".into(),
        label: "Eve".into(),
    });
    reader_shot(
        &mut cx,
        ReaderScene {
            choice: Some("ara"),
            ..scene("lector-senal-claro-pausado".into(), pausado.clone())
        },
        &mut written,
    )?;
    let terminado = with(tts(READ_TEXT, TtsPhase::Finished, 1., 1.));
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-terminado".into(), terminado),
        &mut written,
    )?;
    let mut fallo = with(TtsSnapshot {
        phase: TtsPhase::Error,
        show: true,
        error: Some(
            "No se copió texto de la ventana activa. Seleccioná algo y volvé a pulsar el \
             atajo, o usá Leer portapapeles."
                .into(),
        ),
        ..Default::default()
    });
    fallo.demo = true;
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-fallo".into(), fallo),
        &mut written,
    )?;
    let mut sin_cuenta = with(tts(READ_TEXT, TtsPhase::Error, 0., 0.));
    sin_cuenta.tts.error = Some(
        "DEMO: la suscripción no autorizó la lectura (403).\nProbá con la clave de API en \
         Ajustes → Lectura → Cuenta. Mars voice no cambia de modo por su cuenta."
            .into(),
    );
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-fallo-cuenta".into(), sin_cuenta),
        &mut written,
    )?;
    let seleccion = with(TtsSnapshot {
        phase: TtsPhase::Reading,
        show: true,
        ..Default::default()
    });
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-seleccion".into(), seleccion),
        &mut written,
    )?;
    reader_shot(
        &mut cx,
        ReaderScene {
            popover: Some(Popover::Voice),
            ..scene("lector-senal-claro-voces".into(), pausado.clone())
        },
        &mut written,
    )?;
    let mut rapido = with(tts(READ_TEXT, TtsPhase::Playing, 0.42, 0.7));
    rapido.tts.rate = 1.75;
    reader_shot(
        &mut cx,
        ReaderScene {
            popover: Some(Popover::Rate),
            ..scene("lector-senal-claro-velocidad".into(), rapido.clone())
        },
        &mut written,
    )?;
    let mut compact_rate = rapido;
    compact_rate.config.tts.compact = true;
    reader_shot(
        &mut cx,
        ReaderScene {
            popover: Some(Popover::Rate),
            ..scene("lector-senal-claro-compacto-velocidad".into(), compact_rate)
        },
        &mut written,
    )?;
    let mut compact_voices = pausado;
    compact_voices.config.tts.compact = true;
    reader_shot(
        &mut cx,
        ReaderScene {
            popover: Some(Popover::Voice),
            ..scene("lector-senal-claro-compacto-voces".into(), compact_voices)
        },
        &mut written,
    )?;
    let mut cola = with(tts(READ_TEXT, TtsPhase::Playing, 0.2, 0.5));
    cola.tts.queued = 2;
    cola.tts.notice = Some("Se agregó a la cola: 2 lecturas esperando.".into());
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-cola".into(), cola),
        &mut written,
    )?;
    let mut real = with(tts(READ_TEXT, TtsPhase::Playing, 0.42, 0.7));
    real.demo = false;
    reader_shot(
        &mut cx,
        scene("lector-senal-claro-costo".into(), real),
        &mut written,
    )?;

    // Ajustes → Lectura, Historial → Lecturas and Inicio with reading metrics.
    struct Page {
        name: &'static str,
        w: f32,
        h: f32,
        theme: Theme,
        mode: ColorMode,
        section: Section,
        detail: bool,
    }
    let page = |name, w, h, theme, mode, section, detail| Page {
        name,
        w,
        h,
        theme,
        mode,
        section,
        detail,
    };
    for pg in [
        page(
            "app-senal-claro-lectura",
            1040.,
            5000.,
            Theme::Signal,
            ColorMode::Light,
            Section::Reading,
            false,
        ),
        page(
            "app-orbita-oscuro-lectura",
            1040.,
            5000.,
            Theme::Orbit,
            ColorMode::Dark,
            Section::Reading,
            false,
        ),
        page(
            "resp-720x5600-lectura-regolito-claro",
            720.,
            5600.,
            Theme::Regolith,
            ColorMode::Light,
            Section::Reading,
            false,
        ),
        page(
            "app-senal-claro-historial-lecturas",
            1040.,
            720.,
            Theme::Signal,
            ColorMode::Light,
            Section::History,
            true,
        ),
        page(
            "app-regolito-oscuro-historial-lecturas",
            1040.,
            900.,
            Theme::Regolith,
            ColorMode::Dark,
            Section::History,
            true,
        ),
        page(
            "resp-725x900-historial-lecturas-orbita-claro",
            725.,
            900.,
            Theme::Orbit,
            ColorMode::Light,
            Section::History,
            true,
        ),
        page(
            "resp-720x540-historial-lecturas-lista-senal-oscuro",
            720.,
            540.,
            Theme::Signal,
            ColorMode::Dark,
            Section::History,
            false,
        ),
        page(
            "app-senal-claro-inicio-lectura",
            1040.,
            1500.,
            Theme::Signal,
            ColorMode::Light,
            Section::Home,
            false,
        ),
        page(
            "app-orbita-oscuro-inicio-lectura",
            1364.,
            1300.,
            Theme::Orbit,
            ColorMode::Dark,
            Section::Home,
            false,
        ),
        page(
            "app-senal-claro-historial-nota-lectura",
            1040.,
            720.,
            Theme::Signal,
            ColorMode::Light,
            Section::History,
            false,
        ),
    ] {
        if !wanted(pg.name) {
            continue;
        }
        cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(pg.theme, pg.mode));
        let mut snap = base(pg.theme, pg.mode);
        snap.tts.voices = Arc::new(builtin_voices());
        if pg.section == Section::History && pg.name.contains("lecturas") {
            snap.history_kind = HistoryKind::Readings;
            snap.readings_has_more = true;
            if pg.detail {
                let pick = if pg.name.contains("regolito") { 2 } else { 0 };
                snap.selected_reading = snap.readings.get(pick).cloned();
                snap.active_reading_id = snap.selected_reading.as_ref().map(|r| r.id);
            }
        }
        if pg.name.contains("nota-lectura") {
            snap.selected_note = snap.notes.first().cloned();
            snap.active_note_id = Some(41);
        }
        if pg.section == Section::Reading {
            snap.config.tts.pronunciations = vec![
                dicta_core::tts::Pronunciation {
                    from: "Alumni".into(),
                    to: "Álumni".into(),
                },
                dicta_core::tts::Pronunciation {
                    from: "GPUI".into(),
                    to: "ye pe u i".into(),
                },
            ];
            snap.config.tts.output_device = Some(outputs()[0].id.clone());
            snap.config.tts.terminal_classes = vec![
                "com.raggesilver.BlackBox".into(),
                "org.codeberg.dnkl.footclient".into(),
            ];
        }
        let hub = cx.update(|cx| hub(snap, LevelHistory::default(), cx));
        let detail = pg.detail;
        let window = cx.open_window(size(px(pg.w), px(pg.h)), |window, cx| {
            cx.new(|cx| {
                let mut s = Settings::new(hub, window, cx).with_section(pg.section);
                s.history_detail_open = detail;
                s
            })
        })?;
        let path = out.join(format!("{}.png", pg.name));
        capture(&mut cx, window.into(), &path)?;
        written.push(path);
    }

    // ---------------------------------------------------------- escritura
    let writer_shot = |cx: &mut HeadlessAppContext,
                       name: String,
                       snap: AppSnapshot,
                       menu: bool,
                       written: &mut Vec<PathBuf>|
     -> anyhow::Result<()> {
        if !wanted(&name) {
            return Ok(());
        }
        let hub = cx.update(|cx| hub(snap, LevelHistory::default(), cx));
        let placement = OverlayPlacement::for_writer(&AppConfig::default());
        let window = cx.open_window(
            size(px(WRITER_WIDTH), px(WRITER_MAX_HEIGHT)),
            |window, cx| {
                cx.new(|cx| {
                    let w = Writer::new(hub, placement, window, cx);
                    if menu { w.with_menu() } else { w }
                })
            },
        )?;
        let path = out.join(format!("{name}.png"));
        capture(cx, window.into(), &path)?;
        written.push(path);
        Ok(())
    };
    for mode in [ColorMode::Light, ColorMode::Dark] {
        for theme in Theme::ALL {
            let tag = format!("{}-{}", slug(theme), mode_slug(mode));
            cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(theme, mode));
            let with = |w: WriterSnapshot| {
                let mut s = base(theme, mode);
                s.writer = w;
                s
            };
            writer_shot(
                &mut cx,
                format!("escritura-{tag}-arreglado"),
                with(writer_fixed(Lang::Es)),
                false,
                &mut written,
            )?;
            if !matches!(
                (theme, mode),
                (Theme::Signal, ColorMode::Light) | (Theme::Orbit, ColorMode::Dark)
            ) {
                continue;
            }
            writer_shot(
                &mut cx,
                format!("escritura-{tag}-traducido"),
                with(writer_translated()),
                false,
                &mut written,
            )?;
            writer_shot(
                &mut cx,
                format!("escritura-{tag}-trabajando"),
                with(WriterSnapshot {
                    phase: WriterPhase::Working,
                    request_id: 3,
                    target_language: "en".into(),
                    input: WRITE_INPUT.into(),
                    truncated: true,
                    ..Default::default()
                }),
                false,
                &mut written,
            )?;
            let mut again = writer_fixed(Lang::Es);
            again.phase = WriterPhase::Working;
            again.mode = WriterMode::Fix;
            again.status = String::new();
            writer_shot(
                &mut cx,
                format!("escritura-{tag}-rehaciendo"),
                with(again),
                false,
                &mut written,
            )?;
            writer_shot(
                &mut cx,
                format!("escritura-{tag}-copiando"),
                with(WriterSnapshot {
                    phase: WriterPhase::Capturing,
                    request_id: 2,
                    target_language: "en".into(),
                    ..Default::default()
                }),
                false,
                &mut written,
            )?;
            let mut menu = writer_fixed(Lang::Es);
            menu.can_replace = false;
            writer_shot(
                &mut cx,
                format!("escritura-{tag}-idiomas"),
                with(menu),
                true,
                &mut written,
            )?;
            writer_shot(
                &mut cx,
                format!("escritura-{tag}-error"),
                with(WriterSnapshot {
                    phase: WriterPhase::Error,
                    request_id: 6,
                    mode: WriterMode::Fix,
                    target_language: "en".into(),
                    input: WRITE_INPUT.into(),
                    error: Some(
                        "DEMO: Grok no respondió a tiempo (504).\nTu texto sigue acá; probá de \
                         nuevo cuando quieras."
                            .into(),
                    ),
                    ..Default::default()
                }),
                false,
                &mut written,
            )?;
        }
    }
    for (name, theme, mode) in [
        ("app-senal-claro-escritura", Theme::Signal, ColorMode::Light),
        ("app-orbita-oscuro-escritura", Theme::Orbit, ColorMode::Dark),
    ] {
        if !wanted(name) {
            continue;
        }
        cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(theme, mode));
        let hub = cx.update(|cx| hub(base(theme, mode), LevelHistory::default(), cx));
        let window = cx.open_window(size(px(1040.), px(3000.)), |window, cx| {
            cx.new(|cx| Settings::new(hub, window, cx).with_section(Section::Writer))
        })?;
        let path = out.join(format!("{name}.png"));
        capture(&mut cx, window.into(), &path)?;
        written.push(path);
    }

    render_languages(&mut cx, out, &wanted, start, &mut written)?;

    // Animation fixture: each style at rest, during normal speech, loud,
    // and four consecutive 30 fps frames, readings at 10 Hz.
    for theme in Theme::ALL {
        cx.update(|cx| cx.global_mut::<UiState>().palette = Palette::new(theme, ColorMode::Light));
        let mut moments: Vec<(String, f32)> = vec![
            ("silencio".into(), 0.5),
            ("normal".into(), 2.0),
            ("alto".into(), 3.4),
        ];
        moments.extend((0..4).map(|f| (format!("seq{f}"), 1.8 + f as f32 / 30.)));
        for (moment, t) in moments {
            let mut rec = base(theme, ColorMode::Light);
            rec.phase = Phase::Recording;
            rec.elapsed_ms = (t * 1000.) as u64;
            rec.confirmed = "DEMO".into();
            overlay_shot(
                &mut cx,
                format!("anim-{}-{moment}", slug(theme)),
                rec,
                levels(t, 100, start),
                &mut written,
            )?;
        }
    }
    Ok(written)
}

/// `i18n-{es,en}-*`: overlay, reader, Proveedor with the consent, Lectura,
/// General and Inicio in both languages, Señal claro, so long strings in
/// buttons and labels can be reviewed side by side.
fn render_languages(
    cx: &mut HeadlessAppContext,
    out: &Path,
    wanted: &dyn Fn(&str) -> bool,
    start: Instant,
    written: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    use crate::session::Session;
    let theme = Theme::Signal;
    let mode = ColorMode::Light;
    for lang in [Lang::Es, Lang::En] {
        let code = lang.code();
        let en = lang == Lang::En;
        cx.update(|cx| {
            let s = cx.global_mut::<UiState>();
            s.lang = lang;
            s.palette = Palette::new(theme, mode);
            s.session = Session::Hyprland;
        });
        let snap = |cfg: &dyn Fn(&mut AppSnapshot)| {
            let mut s = base(theme, mode);
            s.config.ui_language = code.into();
            localize_demo(&mut s, lang);
            cfg(&mut s);
            s
        };
        let overlay = |cx: &mut HeadlessAppContext,
                       name: &str,
                       s: AppSnapshot,
                       levels: LevelHistory,
                       written: &mut Vec<PathBuf>|
         -> anyhow::Result<()> {
            let name = format!("i18n-{code}-{name}");
            if !wanted(&name) {
                return Ok(());
            }
            let height = overlay_height(&s);
            let hub = cx.update(|cx| hub(s, levels, cx));
            let placement = OverlayPlacement::from_config(&AppConfig::default());
            let window = cx.open_window(size(px(OVERLAY_WIDTH), px(height)), |window, cx| {
                cx.new(|cx| Overlay::new(hub, placement, window, cx))
            })?;
            let path = out.join(format!("{name}.png"));
            capture(cx, window.into(), &path)?;
            written.push(path);
            Ok(())
        };
        overlay(
            cx,
            "overlay-reposo",
            snap(&|s| s.status = if en { "Ready" } else { "Listo" }.into()),
            LevelHistory::default(),
            written,
        )?;
        overlay(
            cx,
            "overlay-conectar",
            snap(&|s| {
                s.authenticated = false;
                s.demo = false;
                s.status = if en {
                    "Save your xAI API key in Settings → Provider"
                } else {
                    "Guardá tu clave de API de xAI en Ajustes → Proveedor"
                }
                .into();
            }),
            LevelHistory::default(),
            written,
        )?;
        overlay(
            cx,
            "overlay-grabando",
            snap(&|s| {
                s.phase = Phase::Recording;
                s.elapsed_ms = 4_200;
                s.confirmed = if en {
                    "So, tomorrow at ten"
                } else {
                    "Che, mañana a las diez"
                }
                .into();
                s.partial = if en {
                    "we review the kernel"
                } else {
                    "revisamos el kernel"
                }
                .into();
            }),
            levels(2.2, 100, start),
            written,
        )?;
        overlay(
            cx,
            "overlay-listo",
            snap(&|s| {
                s.phase = Phase::Ready;
                let (original, edited, status) = if en {
                    (
                        "DEMO. We tested gee pee you eye on hyper land and the overlay now opens centered.",
                        "DEMO. We tested GPUI on Hyprland and the overlay now opens centered.",
                        "Copied to the clipboard",
                    )
                } else {
                    (
                        "DEMO. Probé ge pe u i en jiperland y el overlay ahora abre centrado.",
                        "DEMO. Probé GPUI en Hyprland y el overlay ahora abre centrado.",
                        "Copiado al portapapeles",
                    )
                };
                s.original_text = original.into();
                s.final_text = edited.into();
                s.status = status.into();
            }),
            LevelHistory::default(),
            written,
        )?;
        overlay(
            cx,
            "overlay-error",
            snap(&|s| {
                s.phase = Phase::Error;
                // The prefix is the controller's Spanish marker (see
                // `errors.rs`); the rest stands in for a localised message.
                s.error = Some(
                    if en {
                        "No se pudo capturar audio: DEMO: the default microphone is disconnected. \
                     Reconnect it or pick another one in Settings → Microphone."
                    } else {
                        "No se pudo capturar audio: DEMO: el micrófono predeterminado está \
                     desconectado. Reconectalo o elegí otro en Ajustes > Micrófono."
                    }
                    .into(),
                );
            }),
            LevelHistory::default(),
            written,
        )?;

        // Reader.
        let text = if en { READ_TEXT_EN } else { READ_TEXT };
        let reader = |cx: &mut HeadlessAppContext,
                      name: &str,
                      s: AppSnapshot,
                      written: &mut Vec<PathBuf>|
         -> anyhow::Result<()> {
            let name = format!("i18n-{code}-{name}");
            if !wanted(&name) {
                return Ok(());
            }
            let height = reader_height(&s, false);
            let playing = s.tts.phase == TtsPhase::Playing;
            let hub = cx.update(|cx| {
                let hub = hub(s, LevelHistory::default(), cx);
                if playing {
                    hub.update(cx, |h, _| h.tts_levels = reading_levels(2.4, start));
                }
                hub
            });
            let placement = OverlayPlacement::for_reader(&AppConfig::default());
            let window = cx.open_window(size(px(READER_WIDTH), px(height)), |window, cx| {
                cx.new(|cx| Reader::new(hub, placement, window, cx))
            })?;
            let path = out.join(format!("{name}.png"));
            capture(cx, window.into(), &path)?;
            written.push(path);
            Ok(())
        };
        reader(
            cx,
            "lector-leyendo",
            snap(&|s| {
                s.tts = tts(text, TtsPhase::Playing, 0.42, 0.7);
                s.tts.outputs = outputs();
            }),
            written,
        )?;
        reader(
            cx,
            "lector-compacto",
            snap(&|s| {
                s.tts = tts(text, TtsPhase::Paused, 0.62, 1.);
                s.config.tts.compact = true;
            }),
            written,
        )?;
        reader(
            cx,
            "lector-confirmar",
            snap(&|s| {
                s.tts = tts(&text.repeat(30), TtsPhase::Confirm, 0., 0.);
                s.tts.confirm = Some(TtsConfirm {
                    chars: 24_310,
                    estimated_usd: estimate_usd(24_310),
                    truncated: true,
                });
                s.tts.chars_sent = 0;
            }),
            written,
        )?;
        reader(
            cx,
            "lector-esperando",
            snap(&|s| {
                s.tts = TtsSnapshot {
                    phase: TtsPhase::Idle,
                    show: true,
                    ..Default::default()
                };
            }),
            written,
        )?;

        // Escritura, its explanations in the interface language.
        let name = format!("i18n-{code}-escritura-arreglado");
        if wanted(&name) {
            let mut s = snap(&|_| {});
            s.writer = writer_fixed(lang);
            let hub = cx.update(|cx| hub(s, LevelHistory::default(), cx));
            let placement = OverlayPlacement::for_writer(&AppConfig::default());
            let window = cx.open_window(
                size(px(WRITER_WIDTH), px(WRITER_MAX_HEIGHT)),
                |window, cx| cx.new(|cx| Writer::new(hub, placement, window, cx)),
            )?;
            let path = out.join(format!("{name}.png"));
            capture(cx, window.into(), &path)?;
            written.push(path);
        }

        // Main window pages.
        let page = |cx: &mut HeadlessAppContext,
                    name: &str,
                    (w, h): (f32, f32),
                    section: Section,
                    consent: Option<Consent>,
                    s: AppSnapshot,
                    written: &mut Vec<PathBuf>|
         -> anyhow::Result<()> {
            let name = format!("i18n-{code}-{name}");
            if !wanted(&name) {
                return Ok(());
            }
            let hub = cx.update(|cx| hub(s, LevelHistory::default(), cx));
            let window = cx.open_window(size(px(w), px(h)), |window, cx| {
                cx.new(|cx| {
                    let s = Settings::new(hub, window, cx).with_section(section);
                    match consent {
                        Some(c) => s.with_consent(c),
                        None => s,
                    }
                })
            })?;
            let path = out.join(format!("{name}.png"));
            capture(cx, window.into(), &path)?;
            written.push(path);
            Ok(())
        };
        let new_user = |s: &mut AppSnapshot| {
            s.authenticated = false;
            s.status = if en {
                "No API key saved yet"
            } else {
                "Todavía no guardaste una clave de API"
            }
            .into();
        };
        page(
            cx,
            "proveedor",
            (1040., 900.),
            Section::Provider,
            None,
            snap(&new_user),
            written,
        )?;
        page(
            cx,
            "proveedor-consentimiento",
            (1040., 1080.),
            Section::Provider,
            Some(Consent::Dictation),
            snap(&new_user),
            written,
        )?;
        page(
            cx,
            "proveedor-suscripcion",
            (1040., 900.),
            Section::Provider,
            None,
            snap(&|s| {
                // An existing user already on the subscription: a badge,
                // never a blocking dialog.
                s.config.provider = ProviderMode::Subscription;
                s.config.tts.provider = ProviderMode::Subscription;
                s.status = if en {
                    "Signed in with Grok"
                } else {
                    "Sesión iniciada con Grok"
                }
                .into();
            }),
            written,
        )?;
        page(
            cx,
            "lectura",
            (1040., 6200.),
            Section::Reading,
            Some(Consent::Reading),
            snap(&|s| {
                s.tts.voices = Arc::new(builtin_voices());
                s.config.tts.output_device = Some(outputs()[0].id.clone());
                s.config.tts.terminal_classes = vec!["com.raggesilver.BlackBox".into()];
                s.config.tts.pronunciations = vec![dicta_core::tts::Pronunciation {
                    from: "GPUI".into(),
                    to: "ye pe u i".into(),
                }];
            }),
            written,
        )?;
        page(
            cx,
            "general",
            (1040., 760.),
            Section::General,
            None,
            snap(&|_| {}),
            written,
        )?;
        page(
            cx,
            "atajos",
            (1040., 900.),
            Section::Shortcuts,
            None,
            snap(&|s| s.config.shortcut_mode = dicta_core::ShortcutMode::Hold),
            written,
        )?;
        page(
            cx,
            "inicio",
            (1040., 1400.),
            Section::Home,
            None,
            snap(&|_| {}),
            written,
        )?;
        page(
            cx,
            "inicio-angosto",
            (crate::MAIN_MIN_W, 1500.),
            Section::Home,
            None,
            snap(&|_| {}),
            written,
        )?;
    }
    cx.update(|cx| cx.global_mut::<UiState>().lang = Lang::Es);
    Ok(())
}
