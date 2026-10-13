//! D-25: prueba consciente con la cuenta real de Mars voice. Hace una lectura
//! corta de dos locuciones por WebSocket y una muestra REST, y muestra solo
//! tiempos, tamaños y la forma de las marcas. No imprime tokens ni texto ajeno.
//! El audio queda en `mars-voice-probe.wav`, en la carpeta temporal del
//! sistema (`std::env::temp_dir()`), para escucharlo.
//!
//!   cargo run -p dicta-provider --example tts_probe [subscription|api_key]
use dicta_core::{AppPaths, ProviderMode};
use dicta_provider::{CancellationToken, Credentials, TtsEvent, TtsRequest, XaiProvider};
use std::time::Instant;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), String> {
    let paths = AppPaths::discover().map_err(|e| e.to_string())?;
    let config = paths.load().map_err(|e| e.to_string())?;
    let mode = match std::env::args().nth(1).as_deref() {
        Some("api_key") => ProviderMode::ApiKey,
        Some("subscription") => ProviderMode::Subscription,
        _ => config.tts.provider,
    };
    println!(
        "Modo: {mode:?} · voz {} · idioma {}",
        config.tts.voice, config.tts.language
    );
    let cancel = CancellationToken::new();
    let bearer = Credentials::new()
        .bearer_for(&config, mode, false, &cancel)
        .await
        .map_err(|e| e.to_string())?;
    let provider = XaiProvider::new().map_err(|e| e.to_string())?;

    match provider.list_voices(&bearer, &cancel).await {
        Ok(voices) => println!(
            "Voces: {} ({})",
            voices.len(),
            voices
                .iter()
                .take(8)
                .map(|v| format!("{}:{}{}", v.id, v.language, if v.custom { "*" } else { "" }))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(e) => println!("Voces: error: {e}"),
    }

    let request = TtsRequest {
        settings: config.tts.voice_settings(),
        with_timestamps: true,
    };
    let (utterances_tx, utterances) = tokio::sync::mpsc::channel(2);
    let (events_tx, mut events) = tokio::sync::mpsc::channel(64);
    utterances_tx
        .send((0, "Hola, esto es una prueba de Mars voice.".to_string()))
        .await
        .unwrap();
    utterances_tx
        .send((1, "Segunda frase, más corta.".to_string()))
        .await
        .unwrap();
    drop(utterances_tx);
    let started = Instant::now();
    let session = {
        let provider = provider.clone();
        let request = request.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            provider
                .tts_session(&request, &bearer, utterances, events_tx, cancel)
                .await
        })
    };
    let mut pcm = Vec::new();
    let mut first_audio = None;
    while let Some(event) = events.recv().await {
        let t = started.elapsed().as_millis();
        match event {
            TtsEvent::Ready => println!("{t:>5} ms  listo"),
            TtsEvent::Audio { id, pcm: bytes } => {
                if first_audio.is_none() {
                    first_audio = Some(t);
                    println!("{t:>5} ms  primer audio (locución {id})");
                }
                pcm.extend_from_slice(&bytes);
            }
            TtsEvent::Marks { id, marks } => {
                let preview: Vec<String> = marks
                    .iter()
                    .take(4)
                    .map(|m| format!("{:?}@{}-{}", m.ch, m.start_ms, m.end_ms))
                    .collect();
                let last = marks.last().map(|m| m.end_ms).unwrap_or_default();
                println!(
                    "{t:>5} ms  marcas locución {id}: {} · {} … fin {last} ms",
                    marks.len(),
                    preview.join(" ")
                );
            }
            TtsEvent::Done { id } => println!(
                "{t:>5} ms  fin locución {id} · audio acumulado {} ms",
                pcm.len() as u64 / 2 * 1000 / u64::from(request.settings.sample_rate)
            ),
        }
    }
    match session.await {
        Ok(result) => println!("Sesión: {result:?}"),
        Err(e) => println!("Sesión: tarea falló: {e}"),
    }
    let mut wav = Vec::with_capacity(44 + pcm.len());
    let rate = request.settings.sample_rate;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    let path = std::env::temp_dir().join("mars-voice-probe.wav");
    std::fs::write(&path, wav).map_err(|e| e.to_string())?;
    println!("Audio: {}", path.display());
    Ok(())
}
