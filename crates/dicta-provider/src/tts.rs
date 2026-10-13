//! Lectura en voz alta con xAI: WebSocket multi-locución, REST y lista de voces.
//! Nunca registra ni copia en errores el texto, el audio, el bearer ni cuerpos
//! remotos. No reintenta pedidos cobrados ni cambia de suscripción a API key.
use crate::{
    ProviderError as Error, Result, Secret, Timeouts, XaiProvider,
    stream::{bounded, validate_endpoint},
};
use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use dicta_core::tts::{
    API_MAX_CHARS, CharTime, GENERATION_SPEED_MAX, GENERATION_SPEED_MIN, MAX_PRONUNCIATIONS,
    Pronunciation, TtsVoice, VoiceSettings,
};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::HashSet, fmt, io::ErrorKind, time::Duration};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until, timeout},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        self, Message, client::IntoClientRequest, error::ProtocolError, protocol::WebSocketConfig,
    },
};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Sample rates documented for `output_format.sample_rate` and the WS query.
const SAMPLE_RATES: [u32; 6] = [8_000, 16_000, 22_050, 24_000, 44_100, 48_000];
/// Audio deltas are base64; leave room for long chunks at 48 kHz.
const MAX_WS_MESSAGE: usize = 16 << 20;
/// Upper bound on `GET /v1/custom-voices` pages (1000 voices each).
const CUSTOM_VOICE_PAGES: usize = 10;
/// Accepts base64 with or without padding.
const B64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// What to synthesize with. The text travels separately (per utterance).
#[derive(Debug, Clone, PartialEq)]
pub struct TtsRequest {
    pub settings: VoiceSettings,
    /// Ask xAI for per-character marks (`graph_chars`/`graph_times`).
    pub with_timestamps: bool,
}

/// Streaming events. `Debug` shows sizes only, never audio or text.
#[derive(Clone, PartialEq)]
pub enum TtsEvent {
    /// Socket open and configured (after `session.updated` if `replace` was sent).
    Ready,
    /// PCM s16le mono at `settings.sample_rate` for utterance `id`, in arrival
    /// order. Always an even number of bytes.
    Audio { id: u64, pcm: Vec<u8> },
    /// Per-character marks relative to the start of utterance `id`'s audio, in ms.
    Marks { id: u64, marks: Vec<CharTime> },
    /// `audio.done` for utterance `id`.
    Done { id: u64 },
}
impl fmt::Debug for TtsEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => f.write_str("Ready"),
            Self::Audio { id, pcm } => write!(f, "Audio {{ id: {id}, bytes: {} }}", pcm.len()),
            Self::Marks { id, marks } => {
                write!(f, "Marks {{ id: {id}, count: {} }}", marks.len())
            }
            Self::Done { id } => write!(f, "Done {{ id: {id} }}"),
        }
    }
}

/// How a streaming session ended without error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsSessionEnd {
    /// `utterances` was closed and every utterance taken got its `Done`.
    Finished,
    /// The socket closed (or was rotated before the 600 s permit TTL) while no
    /// utterance was in flight. Reconnect to continue; nothing was half-sent.
    ServerClosed,
}

/// REST result. `Debug` shows sizes only.
#[derive(Clone, PartialEq, Default)]
pub struct TtsAudio {
    /// PCM s16le mono at `settings.sample_rate`, even length.
    pub pcm: Vec<u8>,
    /// Relative to the start of `pcm`, in ms. Empty without timestamps.
    pub marks: Vec<CharTime>,
}
impl fmt::Debug for TtsAudio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TtsAudio {{ bytes: {}, marks: {} }}",
            self.pcm.len(),
            self.marks.len()
        )
    }
}

impl XaiProvider {
    /// WebSocket session (`wss://api.x.ai/v1/tts`). For each `(id, text)` taken
    /// from `utterances`, in order: sends `text.delta` (style-wrapped text, split
    /// at 60 000 characters) and `text.done`, forwards audio and marks, emits
    /// `Done{id}` on `audio.done`, and only then takes the next utterance. Empty
    /// text gets `Done{id}` without touching the network.
    ///
    /// Closing `utterances` ends cleanly after the in-flight utterance
    /// (`Finished`). A close while idle, or the idle rotation before xAI's 600 s
    /// permit TTL, returns `ServerClosed`; utterances still queued in the channel
    /// are dropped unsent, so the controller resends every id without `Done`.
    /// `cancel` sends a best-effort `text.clear`, closes and returns `Cancelled`.
    /// An `error` event, or a close with an utterance in flight, returns an
    /// actionable error; events already delivered stay delivered.
    pub async fn tts_session(
        &self,
        request: &TtsRequest,
        bearer: &Secret,
        utterances: mpsc::Receiver<(u64, String)>,
        events: mpsc::Sender<TtsEvent>,
        cancel: CancellationToken,
    ) -> Result<TtsSessionEnd> {
        let replace = checked_settings(&request.settings)?;
        let mut upgrade = self
            .tts_url(request)?
            .as_str()
            .into_client_request()
            .map_err(|_| Error::Configuration("URL de lectura inválida"))?;
        let mut header =
            tungstenite::http::HeaderValue::from_str(&format!("Bearer {}", bearer.expose()))
                .map_err(|_| Error::InvalidCredential)?;
        header.set_sensitive(true);
        upgrade.headers_mut().insert("Authorization", header);
        let ws_config = WebSocketConfig::default()
            .max_message_size(Some(MAX_WS_MESSAGE))
            .max_frame_size(Some(MAX_WS_MESSAGE));
        let (mut ws, _) = bounded(
            &cancel,
            self.timeouts.connect,
            "conexión de lectura",
            async {
                connect_async_with_config(upgrade, Some(ws_config), false)
                    .await
                    .map_err(ws_error)
            },
        )
        .await?;
        let session = Session {
            timeouts: &self.timeouts,
            request,
            events: &events,
            cancel: &cancel,
        };
        let result = session.run(&mut ws, replace, utterances).await;
        // Best effort only: never delays the caller beyond a short write.
        let quick = self.timeouts.write.min(Duration::from_secs(2));
        if result == Err(Error::Cancelled) {
            let _ = timeout(
                quick,
                ws.send(Message::Text(r#"{"type":"text.clear"}"#.into())),
            )
            .await;
        }
        let _ = timeout(quick, ws.close(None)).await;
        result
    }

    /// REST (`POST /v1/tts`), codec pcm. With `request.with_timestamps` the JSON
    /// envelope is parsed for marks. For voice previews, and as an alternative
    /// transport when the WebSocket upgrade failed before any text was sent.
    /// Empty text returns empty audio without network. No retries.
    pub async fn synthesize(
        &self,
        request: &TtsRequest,
        bearer: &Secret,
        text: &str,
        cancel: &CancellationToken,
    ) -> Result<TtsAudio> {
        let replace = checked_settings(&request.settings)?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if text.trim().is_empty() {
            return Ok(TtsAudio::default());
        }
        let settings = &request.settings;
        let wrapped = settings.style.wrap(text);
        if wrapped.chars().count() > API_MAX_CHARS {
            return Err(Error::TtsTextTooLong);
        }
        let mut body = json!({
            "text": wrapped,
            "language": settings.language.trim(),
            "voice_id": settings.voice.trim(),
            "speed": settings.speed,
            "output_format": {"codec": "pcm", "sample_rate": settings.sample_rate},
            "optimize_streaming_latency": u8::from(settings.low_latency),
            "text_normalization": settings.text_normalization,
            "with_timestamps": request.with_timestamps,
        });
        if let Some(map) = replace {
            body["replace"] = Value::Object(map);
        }
        let total = self.timeouts.tts_rest;
        bounded(cancel, total, "lectura", async {
            let response = self
                .http
                .post(&self.endpoints.tts)
                .timeout(total)
                .bearer_auth(bearer.expose())
                .json(&body)
                .send()
                .await
                .map_err(|_| Error::Network)?;
            if !response.status().is_success() {
                // The body is never read: it could echo text or account data.
                return Err(Error::tts_http(response.status().as_u16()));
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let bytes = response.bytes().await.map_err(|_| Error::Network)?;
            if content_type.starts_with("application/json") {
                return parse_envelope(&bytes);
            }
            reject_non_pcm(&content_type)?;
            let mut pcm = bytes.to_vec();
            if pcm.len() % 2 == 1 {
                pcm.pop();
            }
            Ok(TtsAudio { pcm, marks: vec![] })
        })
        .await
    }

    /// Built-in (`GET /v1/tts/voices`) plus custom voices (`GET /v1/custom-voices`,
    /// `custom: true`), sorted by name. Any custom-voice failure (403 for teams
    /// without access, 404, network, bad JSON) yields no custom voices instead of
    /// failing the listing; only cancellation propagates from it.
    pub async fn list_voices(
        &self,
        bearer: &Secret,
        cancel: &CancellationToken,
    ) -> Result<Vec<TtsVoice>> {
        let (builtin, custom) = tokio::try_join!(
            self.builtin_voices(bearer, cancel),
            self.custom_voices(bearer, cancel)
        )?;
        let mut seen = HashSet::new();
        let mut voices: Vec<TtsVoice> = builtin
            .into_iter()
            .chain(custom)
            .filter(|v| seen.insert(v.id.to_ascii_lowercase()))
            .collect();
        voices.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(voices)
    }

    fn tts_url(&self, request: &TtsRequest) -> Result<Url> {
        let s = &request.settings;
        let mut url = validate_endpoint(&self.endpoints.tts_ws, true)?;
        url.query_pairs_mut()
            .append_pair("language", s.language.trim())
            .append_pair("voice", s.voice.trim())
            .append_pair("codec", "pcm")
            .append_pair("sample_rate", &s.sample_rate.to_string())
            .append_pair("speed", &s.speed.to_string())
            .append_pair(
                "optimize_streaming_latency",
                if s.low_latency { "1" } else { "0" },
            )
            .append_pair("text_normalization", bool_str(s.text_normalization))
            .append_pair("with_timestamps", bool_str(request.with_timestamps));
        Ok(url)
    }
    async fn builtin_voices(
        &self,
        bearer: &Secret,
        cancel: &CancellationToken,
    ) -> Result<Vec<TtsVoice>> {
        let value = self
            .get_json(&self.endpoints.tts_voices, bearer, cancel)
            .await?;
        let list = value
            .get("voices")
            .and_then(Value::as_array)
            .ok_or(Error::Protocol("lista de voces inválida"))?;
        Ok(list.iter().filter_map(|v| voice(v, false)).collect())
    }
    async fn custom_voices(
        &self,
        bearer: &Secret,
        cancel: &CancellationToken,
    ) -> Result<Vec<TtsVoice>> {
        let mut voices = vec![];
        let mut token: Option<String> = None;
        for _ in 0..CUSTOM_VOICE_PAGES {
            let Ok(mut url) = validate_endpoint(&self.endpoints.custom_voices, false) else {
                break;
            };
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("limit", "1000");
                if let Some(token) = &token {
                    query.append_pair("pagination_token", token);
                }
            }
            let page = match self.get_json(url.as_str(), bearer, cancel).await {
                Ok(page) => page,
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => break,
            };
            if let Some(list) = page.get("voices").and_then(Value::as_array) {
                voices.extend(list.iter().filter_map(|v| voice(v, true)));
            }
            match page
                .get("pagination_token")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty() && t.len() <= 1024)
            {
                Some(next) if token.as_deref() != Some(next) => token = Some(next.to_owned()),
                _ => break,
            }
        }
        Ok(voices)
    }
    async fn get_json(
        &self,
        url: &str,
        bearer: &Secret,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        bounded(cancel, self.timeouts.http, "lista de voces", async {
            let response = self
                .http
                .get(url)
                .bearer_auth(bearer.expose())
                .send()
                .await
                .map_err(|_| Error::Network)?;
            match response.status().as_u16() {
                200..=299 => {}
                403 => return Err(Error::TtsForbidden),
                status => return Err(Error::http(status)),
            }
            response
                .json()
                .await
                .map_err(|_| Error::Protocol("lista de voces inválida"))
        })
        .await
    }
}

trait Socket:
    Stream<Item = std::result::Result<Message, tungstenite::Error>>
    + Sink<Message, Error = tungstenite::Error>
    + Unpin
{
}
impl<T> Socket for T where
    T: Stream<Item = std::result::Result<Message, tungstenite::Error>>
        + Sink<Message, Error = tungstenite::Error>
        + Unpin
{
}

struct Session<'a> {
    timeouts: &'a Timeouts,
    request: &'a TtsRequest,
    events: &'a mpsc::Sender<TtsEvent>,
    cancel: &'a CancellationToken,
}
enum Idle {
    Cancel,
    Rotate,
    Frame(Option<std::result::Result<Message, tungstenite::Error>>),
    Next(Option<(u64, String)>),
}
impl Session<'_> {
    async fn run(
        &self,
        ws: &mut impl Socket,
        replace: Option<Map<String, Value>>,
        mut utterances: mpsc::Receiver<(u64, String)>,
    ) -> Result<TtsSessionEnd> {
        let rotate_at = Instant::now() + self.timeouts.tts_session;
        if let Some(map) = replace {
            // Applies to every utterance whose first text arrives after it.
            self.send(
                ws,
                json!({"type": "session.update", "replace": map}).to_string(),
            )
            .await?;
            self.await_session_updated(ws).await?;
        }
        self.emit(TtsEvent::Ready).await?;
        let sample_rate = self.request.settings.sample_rate;
        // PCM bytes of finished utterances: the session-relative mark offset.
        let mut delivered: u64 = 0;
        loop {
            // Server frames win over new text so a close is noticed before sending.
            let step = tokio::select! { biased;
                _ = self.cancel.cancelled() => Idle::Cancel,
                frame = ws.next() => Idle::Frame(frame),
                _ = sleep_until(rotate_at) => Idle::Rotate,
                next = utterances.recv() => Idle::Next(next),
            };
            let (id, text) = match step {
                Idle::Cancel => return Err(Error::Cancelled),
                Idle::Rotate => return Ok(TtsSessionEnd::ServerClosed),
                Idle::Next(None) => return Ok(TtsSessionEnd::Finished),
                Idle::Next(Some(utterance)) => utterance,
                Idle::Frame(None) => return Ok(TtsSessionEnd::ServerClosed),
                Idle::Frame(Some(Err(error))) => {
                    return if closed(&error) {
                        Ok(TtsSessionEnd::ServerClosed)
                    } else {
                        Err(ws_error(error))
                    };
                }
                Idle::Frame(Some(Ok(message))) => {
                    match message {
                        Message::Text(text) => match parse(&text)? {
                            // Moderation can fail a finished utterance afterwards.
                            Wire::Error {
                                status,
                                code,
                                message,
                            } => return Err(wire_error(status, code, message)),
                            Wire::Delta { .. } => {
                                return Err(Error::Protocol("audio de lectura sin texto enviado"));
                            }
                            _ => {}
                        },
                        Message::Ping(data) => self.send_frame(ws, Message::Pong(data)).await?,
                        Message::Close(_) => return Ok(TtsSessionEnd::ServerClosed),
                        _ => {}
                    }
                    continue;
                }
            };
            if text.trim().is_empty() {
                self.emit(TtsEvent::Done { id }).await?;
                continue;
            }
            let wrapped = self.request.settings.style.wrap(&text);
            drop(text);
            for chunk in char_chunks(&wrapped, API_MAX_CHARS) {
                self.send(
                    ws,
                    json!({"type": "text.delta", "delta": chunk}).to_string(),
                )
                .await?;
            }
            drop(wrapped);
            self.send(ws, r#"{"type":"text.done"}"#.to_owned()).await?;
            delivered += self
                .receive(ws, id, bytes_to_ms(delivered, sample_rate))
                .await?;
        }
    }
    /// Forwards one utterance until `audio.done`. Returns the PCM bytes delivered.
    async fn receive(&self, ws: &mut impl Socket, id: u64, offset_ms: u64) -> Result<u64> {
        let mut deadline = Instant::now() + self.timeouts.tts_first_audio;
        let mut stage = "primer audio de lectura";
        let mut pcm = PcmJoiner::default();
        // Decided on the first mark: xAI documents session-relative times, but an
        // utterance that starts near 0 means the server already rebased them.
        let mut rebase: Option<bool> = None;
        loop {
            let frame = tokio::select! { biased;
                _ = self.cancel.cancelled() => return Err(Error::Cancelled),
                frame = ws.next() => frame,
                _ = sleep_until(deadline) => return Err(Error::Timeout(stage)),
            };
            let message = frame.ok_or(Error::TtsDisconnected)?.map_err(ws_error)?;
            match message {
                Message::Text(text) => match parse(&text)? {
                    Wire::Delta {
                        delta,
                        audio_timestamps,
                    } => {
                        deadline = Instant::now() + self.timeouts.tts_gap;
                        stage = "audio de lectura";
                        let chunk = pcm.push(&delta)?;
                        if !chunk.is_empty() {
                            self.emit(TtsEvent::Audio { id, pcm: chunk }).await?;
                        }
                        let mut marks = audio_timestamps.as_ref().map(marks).unwrap_or_default();
                        if let Some(first) = marks.first() {
                            let shift = *rebase.get_or_insert(
                                offset_ms > 0 && first.start_ms.saturating_mul(2) >= offset_ms,
                            );
                            if shift {
                                for mark in &mut marks {
                                    mark.start_ms = mark.start_ms.saturating_sub(offset_ms);
                                    mark.end_ms = mark.end_ms.saturating_sub(offset_ms);
                                }
                            }
                            self.emit(TtsEvent::Marks { id, marks }).await?;
                        }
                    }
                    Wire::Done => {
                        // A trailing odd byte cannot be a sample; it is dropped.
                        self.emit(TtsEvent::Done { id }).await?;
                        return Ok(pcm.total);
                    }
                    Wire::Error {
                        status,
                        code,
                        message,
                    } => return Err(wire_error(status, code, message)),
                    // Only expected after our own text.clear, which ends the session.
                    Wire::Clear => return Err(Error::TtsUpstream),
                    Wire::SessionUpdated | Wire::Unknown => {}
                },
                Message::Ping(data) => self.send_frame(ws, Message::Pong(data)).await?,
                Message::Close(_) => return Err(Error::TtsDisconnected),
                _ => {}
            }
        }
    }
    async fn await_session_updated(&self, ws: &mut impl Socket) -> Result<()> {
        let deadline = Instant::now() + self.timeouts.ready;
        loop {
            let message = bounded(
                self.cancel,
                deadline.saturating_duration_since(Instant::now()),
                "configuración de lectura",
                async {
                    ws.next()
                        .await
                        .ok_or(Error::TtsDisconnected)?
                        .map_err(ws_error)
                },
            )
            .await?;
            match message {
                Message::Text(text) => match parse(&text)? {
                    Wire::SessionUpdated => return Ok(()),
                    Wire::Error {
                        status,
                        code,
                        message,
                    } => return Err(wire_error(status, code, message)),
                    _ => {}
                },
                Message::Ping(data) => self.send_frame(ws, Message::Pong(data)).await?,
                Message::Close(_) => return Err(Error::TtsDisconnected),
                _ => {}
            }
        }
    }
    async fn send(&self, ws: &mut impl Socket, text: String) -> Result<()> {
        self.send_frame(ws, Message::Text(text.into())).await
    }
    async fn send_frame(&self, ws: &mut impl Socket, message: Message) -> Result<()> {
        bounded(
            self.cancel,
            self.timeouts.write,
            "envío de texto de lectura",
            async { ws.send(message).await.map_err(ws_error) },
        )
        .await
    }
    async fn emit(&self, event: TtsEvent) -> Result<()> {
        bounded(
            self.cancel,
            self.timeouts.tts_gap,
            "entrega de audio de lectura",
            async {
                self.events
                    .send(event)
                    .await
                    .map_err(|_| Error::ConsumerClosed)
            },
        )
        .await
    }
}

/// Joins base64 deltas into whole s16le samples, carrying an odd trailing byte.
#[derive(Default)]
struct PcmJoiner {
    carry: Option<u8>,
    total: u64,
}
impl PcmJoiner {
    fn push(&mut self, base64: &str) -> Result<Vec<u8>> {
        let decoded = B64
            .decode(base64.trim())
            .map_err(|_| Error::Protocol("audio de lectura inválido"))?;
        let mut out = Vec::with_capacity(decoded.len() + 1);
        out.extend(self.carry.take());
        out.extend_from_slice(&decoded);
        if out.len() % 2 == 1 {
            self.carry = out.pop();
        }
        self.total += out.len() as u64;
        Ok(out)
    }
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Wire {
    #[serde(rename = "audio.delta")]
    Delta {
        #[serde(default)]
        delta: String,
        #[serde(default)]
        audio_timestamps: Option<Value>,
    },
    #[serde(rename = "audio.done")]
    Done,
    #[serde(rename = "audio.clear")]
    Clear,
    #[serde(rename = "session.updated")]
    SessionUpdated,
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        status: Option<Value>,
        #[serde(default)]
        code: Option<Value>,
        #[serde(default)]
        message: Option<Value>,
    },
    #[serde(other)]
    Unknown,
}
fn parse(text: &str) -> Result<Wire> {
    serde_json::from_str(text).map_err(|_| Error::Protocol("evento de lectura inválido"))
}
/// Classifies an `error` event. The upstream message is inspected, never kept.
fn wire_error(status: Option<Value>, code: Option<Value>, message: Option<Value>) -> Error {
    if let Some(status) = status
        .as_ref()
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
    {
        return Error::tts_http(status);
    }
    let code = code
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match code.as_str() {
        "unauthorized" | "invalid_api_key" | "authentication_error" => return Error::Unauthorized,
        "forbidden" | "permission_denied" => return Error::TtsForbidden,
        "rate_limit_exceeded" | "rate_limited" | "insufficient_quota" => {
            return Error::RateLimited;
        }
        _ => {}
    }
    let message = message
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| message.contains(k));
    if has(&["replace"]) {
        Error::TtsPronunciations
    } else if has(&["rate limit", "too many requests", "quota"]) {
        Error::RateLimited
    } else if has(&["moderat", "content policy", "safety", "violat"]) {
        Error::TtsModerated
    } else {
        Error::TtsUpstream
    }
}
fn closed(error: &tungstenite::Error) -> bool {
    match error {
        tungstenite::Error::ConnectionClosed
        | tungstenite::Error::AlreadyClosed
        | tungstenite::Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => true,
        tungstenite::Error::Io(io) => matches!(
            io.kind(),
            ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe
                | ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}
fn ws_error(error: tungstenite::Error) -> Error {
    if closed(&error) {
        return Error::TtsDisconnected;
    }
    match error {
        tungstenite::Error::Http(response) => Error::tts_http(response.status().as_u16()),
        tungstenite::Error::Capacity(_) => Error::Protocol("fragmento de audio demasiado grande"),
        _ => Error::Network,
    }
}

/// Validates before any network use and builds the `replace` map, if any.
fn checked_settings(settings: &VoiceSettings) -> Result<Option<Map<String, Value>>> {
    if !valid_voice_id(settings.voice.trim()) {
        return Err(Error::Configuration(
            "voz de lectura inválida; elegí otra en Ajustes → Lectura",
        ));
    }
    let language = settings.language.trim();
    let valid_language = language == "auto"
        || (!language.is_empty()
            && language.len() <= 16
            && language
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    if !valid_language {
        return Err(Error::Configuration(
            "idioma de lectura inválido; usá auto o un código como es-ES",
        ));
    }
    if !settings.speed.is_finite()
        || !(GENERATION_SPEED_MIN..=GENERATION_SPEED_MAX).contains(&settings.speed)
    {
        return Err(Error::Configuration(
            "la velocidad de generación debe estar entre 0,7 y 1,5",
        ));
    }
    if !SAMPLE_RATES.contains(&settings.sample_rate) {
        return Err(Error::Configuration(
            "calidad de audio de lectura no admitida por xAI",
        ));
    }
    replace_map(&settings.pronunciations)
}
/// Documented `replace` limits: up to 200 entries, keys of letters, digits,
/// apostrophes and spaces (≤ 100 chars), values ≤ 128 chars, keys distinct as
/// phrases ignoring case and whitespace.
fn replace_map(pronunciations: &[Pronunciation]) -> Result<Option<Map<String, Value>>> {
    if pronunciations.is_empty() {
        return Ok(None);
    }
    if pronunciations.len() > MAX_PRONUNCIATIONS {
        return Err(Error::Configuration("usá hasta 200 pronunciaciones"));
    }
    let mut seen = HashSet::new();
    let mut map = Map::new();
    for entry in pronunciations {
        let from = entry.from.trim();
        let to = entry.to.trim();
        if from.is_empty() || to.is_empty() {
            return Err(Error::Configuration(
                "cada pronunciación necesita texto y reemplazo",
            ));
        }
        if from.chars().count() > 100 || to.chars().count() > 128 {
            return Err(Error::Configuration(
                "pronunciación demasiado larga: hasta 100 caracteres de origen y 128 de reemplazo",
            ));
        }
        if !from
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '\'' | '’'))
        {
            return Err(Error::Configuration(
                "en las pronunciaciones, el texto original solo admite letras, números, apóstrofos y espacios",
            ));
        }
        if to.chars().any(char::is_control) {
            return Err(Error::Configuration(
                "la pronunciación no puede incluir caracteres de control",
            ));
        }
        let phrase = from
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !seen.insert(phrase) {
            return Err(Error::Configuration(
                "hay dos pronunciaciones para el mismo texto",
            ));
        }
        map.insert(from.to_owned(), Value::String(to.to_owned()));
    }
    Ok(Some(map))
}
fn valid_voice_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn voice(value: &Value, custom: bool) -> Option<TtsVoice> {
    let id = value.get("voice_id")?.as_str()?.trim();
    if !valid_voice_id(id) {
        return None;
    }
    let clean = |s: &str, max: usize| -> String {
        s.trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(max)
            .collect()
    };
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(|n| clean(n, 80))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_owned());
    let language = value
        .get("language")
        .and_then(Value::as_str)
        .map(|l| clean(l, 32))
        .unwrap_or_default();
    Some(TtsVoice {
        id: id.to_owned(),
        name,
        language,
        custom,
    })
}

/// Accepts `graph_times` as `[start, end]` pairs (guide, WS schema) or
/// `{start, end}` objects (REST reference), in seconds. Malformed entries are
/// skipped so a bad mark never costs the audio.
fn marks(timestamps: &Value) -> Vec<CharTime> {
    let chars = timestamps.get("graph_chars").and_then(Value::as_array);
    let times = timestamps.get("graph_times").and_then(Value::as_array);
    let (Some(chars), Some(times)) = (chars, times) else {
        return vec![];
    };
    chars
        .iter()
        .zip(times)
        .filter_map(|(ch, time)| {
            let ch = ch.as_str()?.to_owned();
            let (start, end) = match time {
                Value::Array(pair) if pair.len() >= 2 => (pair[0].as_f64()?, pair[1].as_f64()?),
                Value::Object(span) => (span.get("start")?.as_f64()?, span.get("end")?.as_f64()?),
                _ => return None,
            };
            let start_ms = seconds_to_ms(start);
            Some(CharTime {
                ch,
                start_ms,
                end_ms: seconds_to_ms(end).max(start_ms),
            })
        })
        .collect()
}
fn seconds_to_ms(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        // `as` saturates on overflow.
        (seconds * 1000.0).round() as u64
    } else {
        0
    }
}
fn bytes_to_ms(bytes: u64, sample_rate: u32) -> u64 {
    bytes.saturating_mul(1000) / (2 * u64::from(sample_rate.max(1)))
}
fn parse_envelope(bytes: &[u8]) -> Result<TtsAudio> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| Error::Protocol("respuesta de lectura inválida"))?;
    let audio = value
        .get("audio")
        .and_then(Value::as_str)
        .ok_or(Error::Protocol("respuesta de lectura sin audio"))?;
    if let Some(content_type) = value.get("content_type").and_then(Value::as_str) {
        reject_non_pcm(&content_type.to_ascii_lowercase())?;
    }
    let mut pcm = B64
        .decode(audio.trim())
        .map_err(|_| Error::Protocol("audio de lectura inválido"))?;
    if pcm.len() % 2 == 1 {
        pcm.pop();
    }
    let marks = value.get("audio_timestamps").map(marks).unwrap_or_default();
    Ok(TtsAudio { pcm, marks })
}
/// Documented MIME types of the other codecs; `pcm` is `audio/pcm`.
fn reject_non_pcm(content_type: &str) -> Result<()> {
    const OTHER: [&str; 7] = [
        "audio/mpeg",
        "audio/mp3",
        "audio/wav",
        "audio/x-wav",
        "audio/wave",
        "audio/basic",
        "audio/alaw",
    ];
    if OTHER.iter().any(|other| content_type.starts_with(other)) {
        return Err(Error::Protocol(
            "xAI devolvió un formato de audio distinto de PCM",
        ));
    }
    Ok(())
}
fn bool_str(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
/// Splits at char boundaries into pieces of at most `max` characters.
fn char_chunks(text: &str, max: usize) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest.char_indices().nth(max).map_or(rest.len(), |(i, _)| i);
        let (head, tail) = rest.split_at(end);
        rest = tail;
        Some(head)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_respect_char_boundaries_and_limit() {
        let text = "añoé".repeat(5);
        let chunks: Vec<_> = char_chunks(&text, 3).collect();
        assert!(chunks.iter().all(|c| c.chars().count() <= 3));
        assert_eq!(chunks.concat(), text);
        assert_eq!(char_chunks("", 3).count(), 0);
    }
    #[test]
    fn invalid_marks_are_skipped_and_times_are_clamped() {
        let value = json!({
            "graph_chars": ["a", "b", 7, "c", "d"],
            "graph_times": [[0.1, 0.05], {"start": -1.0, "end": 0.2}, [0.2, 0.3], "x", [0.4]]
        });
        assert_eq!(
            marks(&value),
            vec![
                CharTime {
                    ch: "a".into(),
                    start_ms: 100,
                    end_ms: 100
                },
                CharTime {
                    ch: "b".into(),
                    start_ms: 0,
                    end_ms: 200
                },
            ]
        );
    }
    #[test]
    fn pronunciation_keys_follow_documented_limits() {
        let p = |from: &str, to: &str| Pronunciation {
            from: from.into(),
            to: to.into(),
        };
        let map = replace_map(&[p(" Alumni ", "Álumni"), p("O'Neil", "/oʊˈniːl/")])
            .unwrap()
            .unwrap();
        assert_eq!(map["Alumni"], "Álumni");
        assert!(replace_map(&[p("C++", "ce más más")]).is_err());
        assert!(replace_map(&[p("Acme  Mobile", "a"), p("acme mobile", "b")]).is_err());
        assert!(replace_map(&vec![p("a", "b"); 201]).is_err());
    }
    #[test]
    fn upstream_error_messages_are_classified_but_never_echoed() {
        let message = |m: &str| Some(json!(m));
        for (text, expected) in [
            (
                "replace key \"C++\" may not contain punctuation",
                Error::TtsPronunciations,
            ),
            ("Content moderation: secret-fixture", Error::TtsModerated),
            ("Rate limit exceeded", Error::RateLimited),
            ("TTS failed: internal error", Error::TtsUpstream),
        ] {
            let error = wire_error(None, None, message(text));
            assert_eq!(error, expected);
            assert!(!error.to_string().contains("secret-fixture"));
        }
        assert_eq!(
            wire_error(Some(json!(404)), None, None),
            Error::TtsVoiceNotFound
        );
    }
}
