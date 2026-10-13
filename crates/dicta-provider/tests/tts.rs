//! TTS transport against loopback mocks only: no real xAI, keyring or audio.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request as HttpRequest, StatusCode, header},
    response::{IntoResponse, Response as HttpResponse},
    routing::any,
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use dicta_core::{
    AppConfig, ProviderMode,
    tts::{CharTime, Pronunciation, TtsStyle, VoiceSettings},
};
use dicta_provider::{
    CancellationToken, Credentials, Endpoints, OAuthClient, ProviderError, Result, Secret,
    SecretStore, Timeouts, TokenSet, TtsEvent, TtsRequest, TtsSessionEnd, XaiProvider,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

const BEARER: &str = "fixture-bearer";

fn settings() -> VoiceSettings {
    VoiceSettings {
        voice: "carina".into(),
        language: "auto".into(),
        speed: 1.25,
        style: TtsStyle::Normal,
        text_normalization: false,
        low_latency: true,
        sample_rate: 24_000,
        pronunciations: vec![],
    }
}
fn request(settings: VoiceSettings) -> TtsRequest {
    TtsRequest {
        settings,
        with_timestamps: true,
    }
}
fn endpoints(addr: SocketAddr) -> Endpoints {
    Endpoints {
        tts_ws: format!("ws://{addr}/v1/tts"),
        tts: format!("http://{addr}/v1/tts"),
        tts_voices: format!("http://{addr}/v1/tts/voices"),
        custom_voices: format!("http://{addr}/v1/custom-voices"),
        device_code: format!("http://{addr}/device"),
        token: format!("http://{addr}/token"),
        ..Default::default()
    }
}
fn provider(addr: SocketAddr, timeouts: Timeouts) -> XaiProvider {
    XaiProvider::with_endpoints(endpoints(addr), timeouts).unwrap()
}

// ---------------------------------------------------------------------------
// WebSocket helpers
// ---------------------------------------------------------------------------

type Ws = WebSocketStream<TcpStream>;
async fn listen() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}
async fn accept(listener: TcpListener) -> Ws {
    let (tcp, _) = listener.accept().await.unwrap();
    accept_async(tcp).await.unwrap()
}
async fn recv_json(ws: &mut Ws) -> Value {
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Close(_) => panic!("client closed while the server expected a message"),
            _ => {}
        }
    }
}
async fn send_json(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
/// Reads text.delta + text.done and returns the delta the client sent.
async fn recv_utterance(ws: &mut Ws) -> String {
    let delta = recv_json(ws).await;
    assert_eq!(delta["type"], "text.delta");
    assert_eq!(recv_json(ws).await, json!({"type": "text.done"}));
    delta["delta"].as_str().unwrap().to_owned()
}
async fn assert_silent(ws: &mut Ws) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), ws.next())
            .await
            .is_err(),
        "client sent a frame too early"
    );
}
/// Waits until the client closes; anything but a close or EOF is a failure.
async fn expect_close(ws: &mut Ws) {
    match ws.next().await {
        None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {}
        Some(Ok(other)) => panic!("expected close, got {other:?}"),
    }
}
fn delta(pcm: &[u8]) -> Value {
    json!({"type": "audio.delta", "delta": STANDARD.encode(pcm)})
}
fn delta_marks(pcm: &[u8], timestamps: Value) -> Value {
    let mut value = delta(pcm);
    value["audio_timestamps"] = timestamps;
    value["audio_duration"] = json!(0.1);
    value
}
fn done() -> Value {
    json!({"type": "audio.done", "trace_id": "fixture-trace"})
}
fn mark(ch: &str, start_ms: u64, end_ms: u64) -> CharTime {
    CharTime {
        ch: ch.into(),
        start_ms,
        end_ms,
    }
}

struct Running {
    utterances: mpsc::Sender<(u64, String)>,
    events: mpsc::Receiver<TtsEvent>,
    cancel: CancellationToken,
    task: JoinHandle<Result<TtsSessionEnd>>,
}
fn start(provider: XaiProvider, request: TtsRequest) -> Running {
    let (utterances, utterances_rx) = mpsc::channel(8);
    let (events_tx, events) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        provider
            .tts_session(
                &request,
                &Secret::new(BEARER),
                utterances_rx,
                events_tx,
                token,
            )
            .await
    });
    Running {
        utterances,
        events,
        cancel,
        task,
    }
}
impl Running {
    async fn say(&self, id: u64, text: &str) {
        self.utterances.send((id, text.into())).await.unwrap();
    }
    async fn next(&mut self) -> TtsEvent {
        tokio::time::timeout(Duration::from_secs(5), self.events.recv())
            .await
            .expect("event timeout")
            .expect("events closed")
    }
}

#[tokio::test]
async fn handshake_carries_config_and_bearer_and_replace_is_acknowledged_before_text() {
    let (listener, addr) = listen().await;
    let (gate_tx, gate_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_hdr_async(tcp, |req: &Request, response: Response| {
            assert_eq!(req.headers()["authorization"], "Bearer fixture-bearer");
            assert_eq!(req.uri().path(), "/v1/tts");
            let url = url::Url::parse(&format!("ws://localhost{}", req.uri())).unwrap();
            let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
            let expected = [
                ("language", "auto"),
                ("voice", "carina"),
                ("codec", "pcm"),
                ("sample_rate", "24000"),
                ("speed", "1.25"),
                ("optimize_streaming_latency", "1"),
                ("text_normalization", "false"),
                ("with_timestamps", "true"),
            ];
            assert_eq!(query.len(), expected.len(), "unexpected query: {query:?}");
            for (key, value) in expected {
                assert_eq!(query[key], value, "query parameter {key}");
            }
            Ok(response)
        })
        .await
        .unwrap();
        assert_eq!(
            recv_json(&mut ws).await,
            json!({"type": "session.update", "replace": {"Alumni": "Álumni"}})
        );
        // The utterance is already queued; no text may leave before the ack.
        assert_silent(&mut ws).await;
        gate_tx.send(()).unwrap();
        send_json(
            &mut ws,
            json!({"type": "session.updated", "replace": {"Alumni": "Álumni"}}),
        )
        .await;
        // The style is applied by the provider, not by the controller.
        assert_eq!(recv_utterance(&mut ws).await, "<soft>Hola Alumni.</soft>");
        send_json(&mut ws, delta(&[1, 0, 2, 0])).await;
        send_json(&mut ws, done()).await;
        expect_close(&mut ws).await;
    });
    let mut settings = settings();
    settings.style = TtsStyle::Soft;
    settings.pronunciations = vec![Pronunciation {
        from: " Alumni ".into(),
        to: "Álumni".into(),
    }];
    let mut session = start(provider(addr, Timeouts::default()), request(settings));
    session.say(7, "Hola Alumni.").await;
    gate_rx.await.unwrap();
    assert!(
        session.events.try_recv().is_err(),
        "Ready before session.updated"
    );
    assert_eq!(session.next().await, TtsEvent::Ready);
    assert_eq!(
        session.next().await,
        TtsEvent::Audio {
            id: 7,
            pcm: vec![1, 0, 2, 0]
        }
    );
    assert_eq!(session.next().await, TtsEvent::Done { id: 7 });
    drop(session.utterances);
    assert_eq!(session.task.await.unwrap(), Ok(TtsSessionEnd::Finished));
    server.await.unwrap();
}

#[tokio::test]
async fn utterances_are_sent_one_at_a_time_in_order_with_done_per_id() {
    let (listener, addr) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(listener).await;
        // No pronunciations: no session.update, text goes first.
        assert_eq!(recv_utterance(&mut ws).await, "Uno.");
        // The second utterance is queued but must wait for audio.done.
        assert_silent(&mut ws).await;
        send_json(&mut ws, delta(&[1, 0])).await;
        send_json(&mut ws, done()).await;
        assert_eq!(recv_utterance(&mut ws).await, "Dos.");
        send_json(&mut ws, delta(&[2, 0])).await;
        send_json(&mut ws, delta(&[3, 0])).await;
        send_json(&mut ws, done()).await;
        // The blank third utterance never reaches the server.
        expect_close(&mut ws).await;
    });
    let mut session = start(provider(addr, Timeouts::default()), request(settings()));
    session.say(1, "Uno.").await;
    session.say(2, "Dos.").await;
    session.say(3, "  \n").await;
    drop(session.utterances);
    let mut events = vec![];
    while let Some(event) = session.events.recv().await {
        events.push(event);
    }
    assert_eq!(
        events,
        [
            TtsEvent::Ready,
            TtsEvent::Audio {
                id: 1,
                pcm: vec![1, 0]
            },
            TtsEvent::Done { id: 1 },
            TtsEvent::Audio {
                id: 2,
                pcm: vec![2, 0]
            },
            TtsEvent::Audio {
                id: 2,
                pcm: vec![3, 0]
            },
            TtsEvent::Done { id: 2 },
            TtsEvent::Done { id: 3 },
        ]
    );
    assert_eq!(session.task.await.unwrap(), Ok(TtsSessionEnd::Finished));
    server.await.unwrap();
}

#[tokio::test]
async fn base64_pcm_is_reassembled_into_whole_samples_with_odd_byte_carry() {
    let (listener, addr) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(listener).await;
        recv_utterance(&mut ws).await;
        send_json(&mut ws, delta(&[1, 2, 3])).await;
        // Unpadded base64 is accepted too.
        send_json(
            &mut ws,
            json!({"type": "audio.delta", "delta": STANDARD_NO_PAD.encode([4, 5, 6])}),
        )
        .await;
        send_json(&mut ws, delta(&[7])).await;
        send_json(&mut ws, done()).await;
        expect_close(&mut ws).await;
    });
    let mut session = start(provider(addr, Timeouts::default()), request(settings()));
    session.say(1, "Hola.").await;
    assert_eq!(session.next().await, TtsEvent::Ready);
    assert_eq!(
        session.next().await,
        TtsEvent::Audio {
            id: 1,
            pcm: vec![1, 2]
        }
    );
    assert_eq!(
        session.next().await,
        TtsEvent::Audio {
            id: 1,
            pcm: vec![3, 4, 5, 6]
        }
    );
    // The lone trailing byte cannot be a sample and is not delivered.
    assert_eq!(session.next().await, TtsEvent::Done { id: 1 });
    drop(session.utterances);
    assert_eq!(session.task.await.unwrap(), Ok(TtsSessionEnd::Finished));
    server.await.unwrap();
}

/// xAI documents session-relative seconds; some servers may already rebase per
/// utterance. Both must end up relative to the utterance's own audio, in ms.
#[tokio::test]
async fn marks_are_normalized_per_utterance_in_both_timestamp_modes_and_shapes() {
    for session_relative in [true, false] {
        let (listener, addr) = listen().await;
        let base = if session_relative { 1.0 } else { 0.0 };
        let server = tokio::spawn(async move {
            let mut ws = accept(listener).await;
            recv_utterance(&mut ws).await;
            // Exactly one second of 24 kHz s16le audio.
            let marks = json!({"graph_chars": ["U", "n"], "graph_times": [[0.0, 0.1], [0.1, 0.2]]});
            send_json(&mut ws, delta_marks(&vec![0; 48_000], marks)).await;
            send_json(&mut ws, done()).await;
            recv_utterance(&mut ws).await;
            // REST-reference shape: objects.
            let marks = json!({"graph_chars": ["D", "o"], "graph_times": [
                {"start": base, "end": base + 0.1},
                {"start": base + 0.1, "end": base + 0.25}
            ]});
            send_json(&mut ws, delta_marks(&[0, 0], marks)).await;
            // Later chunk of the same utterance keeps the first decision.
            let marks = json!({"graph_chars": ["s"], "graph_times": [[base + 0.6, base + 0.7]]});
            send_json(&mut ws, delta_marks(&[0, 0], marks)).await;
            send_json(&mut ws, done()).await;
            expect_close(&mut ws).await;
        });
        let mut session = start(provider(addr, Timeouts::default()), request(settings()));
        session.say(1, "Un").await;
        session.say(2, "Dos").await;
        drop(session.utterances);
        let mut marks = vec![];
        while let Some(event) = session.events.recv().await {
            if let TtsEvent::Marks { id, marks: m } = event {
                marks.push((id, m));
            }
        }
        assert_eq!(
            marks,
            [
                (1, vec![mark("U", 0, 100), mark("n", 100, 200)]),
                (2, vec![mark("D", 0, 100), mark("o", 100, 250)]),
                (2, vec![mark("s", 600, 700)]),
            ],
            "session_relative={session_relative}"
        );
        assert_eq!(session.task.await.unwrap(), Ok(TtsSessionEnd::Finished));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancel_mid_utterance_sends_text_clear_and_returns_cancelled() {
    let (listener, addr) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(listener).await;
        recv_utterance(&mut ws).await;
        send_json(&mut ws, delta(&[1, 0])).await;
        assert_eq!(recv_json(&mut ws).await, json!({"type": "text.clear"}));
        expect_close(&mut ws).await;
    });
    let mut session = start(provider(addr, Timeouts::default()), request(settings()));
    session.say(1, "Un texto largo.").await;
    assert_eq!(session.next().await, TtsEvent::Ready);
    assert!(matches!(
        session.next().await,
        TtsEvent::Audio { id: 1, .. }
    ));
    session.cancel.cancel();
    assert_eq!(session.task.await.unwrap(), Err(ProviderError::Cancelled));
    server.await.unwrap();
}

#[tokio::test]
async fn error_event_mid_stream_fails_after_delivering_received_audio() {
    let (listener, addr) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(listener).await;
        recv_utterance(&mut ws).await;
        send_json(&mut ws, delta(&[1, 0, 2, 0])).await;
        // Moderation runs after audio was sent (documented fail-open).
        send_json(
            &mut ws,
            json!({"type": "error", "message": "Content moderation flagged secret-fixture"}),
        )
        .await;
        let _ = ws.next().await;
    });
    let mut session = start(provider(addr, Timeouts::default()), request(settings()));
    session.say(1, "Texto.").await;
    assert_eq!(session.next().await, TtsEvent::Ready);
    assert_eq!(
        session.next().await,
        TtsEvent::Audio {
            id: 1,
            pcm: vec![1, 0, 2, 0]
        }
    );
    let error = session.task.await.unwrap().unwrap_err();
    assert_eq!(error, ProviderError::TtsModerated);
    assert!(!error.to_string().contains("secret-fixture"));
    assert!(!format!("{error:?}").contains("flagged"));
    // Already delivered audio stays delivered; no Done is invented.
    assert!(session.events.recv().await.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn server_close_is_server_closed_when_idle_and_an_error_in_flight() {
    // Idle: nothing was half-sent; the controller reconnects later.
    let (listener, addr) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(listener).await;
        ws.close(None).await.unwrap();
        let _ = ws.next().await;
    });
    let mut session = start(provider(addr, Timeouts::default()), request(settings()));
    assert_eq!(session.next().await, TtsEvent::Ready);
    assert_eq!(session.task.await.unwrap(), Ok(TtsSessionEnd::ServerClosed));
    drop(session.utterances);
    server.await.unwrap();

    // In flight: the utterance is incomplete, which is an actionable error.
    let (listener, addr) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(listener).await;
        recv_utterance(&mut ws).await;
        send_json(&mut ws, delta(&[1, 0])).await;
        ws.close(None).await.unwrap();
        let _ = ws.next().await;
    });
    let mut session = start(provider(addr, Timeouts::default()), request(settings()));
    session.say(1, "Texto.").await;
    assert_eq!(session.next().await, TtsEvent::Ready);
    assert!(matches!(
        session.next().await,
        TtsEvent::Audio { id: 1, .. }
    ));
    assert_eq!(
        session.task.await.unwrap(),
        Err(ProviderError::TtsDisconnected)
    );
    server.await.unwrap();
}

#[tokio::test]
async fn missing_first_audio_is_bounded() {
    let (listener, addr) = listen().await;
    let server = tokio::spawn(async move {
        let mut ws = accept(listener).await;
        recv_utterance(&mut ws).await;
        let _ = ws.next().await;
    });
    let timeouts = Timeouts {
        tts_first_audio: Duration::from_millis(100),
        ..Default::default()
    };
    let session = start(provider(addr, timeouts), request(settings()));
    session.say(1, "Texto.").await;
    assert_eq!(
        session.task.await.unwrap(),
        Err(ProviderError::Timeout("primer audio de lectura"))
    );
    server.await.unwrap();
}

#[tokio::test]
async fn upgrade_statuses_are_tts_specific_and_never_echo_the_body() {
    for (status, expected) in [
        (400, ProviderError::TtsRejected),
        (401, ProviderError::Unauthorized),
        (403, ProviderError::TtsForbidden),
        (404, ProviderError::TtsVoiceNotFound),
        (429, ProviderError::RateLimited),
    ] {
        let (listener, addr) = listen().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Failed\r\nContent-Length: 18\r\nConnection: close\r\n\r\nsuper-secret-token"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let session = start(provider(addr, Timeouts::default()), request(settings()));
        let error = session.task.await.unwrap().unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("super-secret"));
        server.await.unwrap();
    }
    // Reading has its own 403 guidance, distinct from dictation's STT message.
    let forbidden = ProviderError::TtsForbidden.to_string();
    assert_ne!(forbidden, ProviderError::Forbidden.to_string());
    assert!(forbidden.contains("Ajustes → Lectura") && forbidden.contains("API key"));
    assert!(forbidden.contains("no se cambió a API paga"));
}

// ---------------------------------------------------------------------------
// REST mock
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: String,
    body: Vec<u8>,
    authorization: String,
}
#[derive(Clone, Default)]
struct Mock {
    routes: Arc<Mutex<HashMap<String, VecDeque<HttpResponse>>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}
impl Mock {
    fn push(&self, path: &str, response: HttpResponse) {
        self.routes
            .lock()
            .unwrap()
            .entry(path.into())
            .or_default()
            .push_back(response);
    }
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}
async fn handler(State(mock): State<Mock>, request: HttpRequest<Body>) -> HttpResponse {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(request.into_body(), 4 << 20)
        .await
        .unwrap()
        .to_vec();
    mock.seen.lock().unwrap().push(Seen {
        path: path.clone(),
        query,
        body,
        authorization,
    });
    let next = mock
        .routes
        .lock()
        .unwrap()
        .get_mut(&path)
        .and_then(VecDeque::pop_front);
    next.unwrap_or_else(|| json_response(500, json!({"error": "unexpected request"})))
}
fn json_response(status: u16, value: Value) -> HttpResponse {
    (StatusCode::from_u16(status).unwrap(), axum::Json(value)).into_response()
}
fn raw_response(content_type: &'static str, body: Vec<u8>) -> HttpResponse {
    (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], body).into_response()
}
struct Server {
    mock: Mock,
    addr: SocketAddr,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn http_server() -> Server {
    let mock = Mock::default();
    let (listener, addr) = listen().await;
    let app = Router::new()
        .fallback(any(handler))
        .with_state(mock.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { mock, addr, task }
}

#[tokio::test]
async fn synthesize_sends_the_documented_body_and_parses_both_envelope_shapes() {
    let s = http_server().await;
    s.mock.push(
        "/v1/tts",
        json_response(
            200,
            json!({
                "audio": STANDARD.encode([1, 0, 2, 0, 3]),
                "content_type": "audio/pcm",
                "duration": 0.12,
                "audio_timestamps": {
                    "graph_chars": ["H", "i"],
                    "graph_times": [{"start": 0.0, "end": 0.06}, {"start": 0.06, "end": 0.12}]
                }
            }),
        ),
    );
    s.mock.push(
        "/v1/tts",
        json_response(
            200,
            json!({
                "audio": STANDARD.encode([9, 0]),
                "content_type": "audio/pcm",
                "duration": 0.5,
                "audio_timestamps": {"graph_chars": ["!"], "graph_times": [[0.0, 0.5]]}
            }),
        ),
    );
    s.mock
        .push("/v1/tts", raw_response("audio/pcm", vec![5, 0, 6, 0]));
    let provider = provider(s.addr, Timeouts::default());
    let cancel = CancellationToken::new();
    let bearer = Secret::new(BEARER);
    let mut settings = settings();
    settings.style = TtsStyle::Whisper;
    settings.language = "es-ES".into();
    settings.sample_rate = 48_000;
    settings.low_latency = false;
    settings.text_normalization = true;
    settings.pronunciations = vec![Pronunciation {
        from: "Alumni".into(),
        to: "Álumni".into(),
    }];
    let mut req = request(settings);

    let audio = provider
        .synthesize(&req, &bearer, "Hi", &cancel)
        .await
        .unwrap();
    // The odd trailing byte is not a sample.
    assert_eq!(audio.pcm, [1, 0, 2, 0]);
    assert_eq!(audio.marks, [mark("H", 0, 60), mark("i", 60, 120)]);
    let seen = s.mock.seen();
    assert_eq!(seen[0].authorization, "Bearer fixture-bearer");
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(
        body,
        json!({
            "text": "<whisper>Hi</whisper>",
            "language": "es-ES",
            "voice_id": "carina",
            "speed": 1.25,
            "output_format": {"codec": "pcm", "sample_rate": 48000},
            "optimize_streaming_latency": 0,
            "text_normalization": true,
            "with_timestamps": true,
            "replace": {"Alumni": "Álumni"}
        })
    );

    let audio = provider
        .synthesize(&req, &bearer, "Hi!", &cancel)
        .await
        .unwrap();
    assert_eq!(audio.pcm, [9, 0]);
    assert_eq!(audio.marks, [mark("!", 0, 500)]);

    // Without timestamps the body is raw PCM.
    req.with_timestamps = false;
    let audio = provider
        .synthesize(&req, &bearer, "Hi", &cancel)
        .await
        .unwrap();
    assert_eq!(audio.pcm, [5, 0, 6, 0]);
    assert!(audio.marks.is_empty());
    let body: Value = serde_json::from_slice(&s.mock.seen()[2].body).unwrap();
    assert_eq!(body["with_timestamps"], false);
}

#[tokio::test]
async fn synthesize_rejects_bad_input_before_network_and_maps_statuses() {
    let s = http_server().await;
    let provider = provider(s.addr, Timeouts::default());
    let cancel = CancellationToken::new();
    let bearer = Secret::new(BEARER);
    let mut bad = settings();
    bad.pronunciations = vec![Pronunciation {
        from: "C++".into(),
        to: "ce más más".into(),
    }];
    assert!(matches!(
        provider
            .synthesize(&request(bad), &bearer, "C++", &cancel)
            .await,
        Err(ProviderError::Configuration(_))
    ));
    assert_eq!(
        provider
            .synthesize(&request(settings()), &bearer, &"a".repeat(60_001), &cancel)
            .await,
        Err(ProviderError::TtsTextTooLong)
    );
    assert!(
        s.mock.seen().is_empty(),
        "invalid input must not reach the network"
    );

    for (status, expected) in [
        (400, ProviderError::TtsRejected),
        (401, ProviderError::Unauthorized),
        (403, ProviderError::TtsForbidden),
        (404, ProviderError::TtsVoiceNotFound),
        (429, ProviderError::RateLimited),
        (503, ProviderError::Http(503)),
    ] {
        s.mock.push(
            "/v1/tts",
            json_response(status, json!({"error": "secret-fixture-do-not-echo"})),
        );
        let error = provider
            .synthesize(&request(settings()), &bearer, "Hola", &cancel)
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("secret-fixture"));
    }
    // One request per call: no automatic paid retries.
    assert_eq!(s.mock.seen().len(), 6);
}

#[tokio::test]
async fn voices_merge_paginated_custom_voices_sorted_and_survive_custom_403() {
    let s = http_server().await;
    s.mock.push(
        "/v1/tts/voices",
        json_response(
            200,
            json!({"voices": [
                {"voice_id": "rex", "name": "Rex", "language": "en"},
                {"voice_id": "ara", "name": "Ara", "language": null},
                {"voice_id": "bad id!", "name": "Inválida"}
            ]}),
        ),
    );
    s.mock.push(
        "/v1/custom-voices",
        json_response(
            200,
            json!({"voices": [{"voice_id": "nlbqfwie", "name": "Friendly Narrator", "language": "en"}],
                   "pagination_token": "page-2"}),
        ),
    );
    s.mock.push(
        "/v1/custom-voices",
        json_response(
            200,
            json!({"voices": [{"voice_id": "zzzzzzzz", "name": null}], "pagination_token": null}),
        ),
    );
    let provider = provider(s.addr, Timeouts::default());
    let cancel = CancellationToken::new();
    let bearer = Secret::new(BEARER);
    let voices = provider.list_voices(&bearer, &cancel).await.unwrap();
    let got: Vec<_> = voices
        .iter()
        .map(|v| {
            (
                v.id.as_str(),
                v.name.as_str(),
                v.language.as_str(),
                v.custom,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("ara", "Ara", "", false),
            ("nlbqfwie", "Friendly Narrator", "en", true),
            ("rex", "Rex", "en", false),
            ("zzzzzzzz", "zzzzzzzz", "", true),
        ]
    );
    let seen = s.mock.seen();
    assert!(
        seen.iter()
            .all(|r| r.authorization == "Bearer fixture-bearer")
    );
    let custom: Vec<_> = seen
        .iter()
        .filter(|r| r.path == "/v1/custom-voices")
        .map(|r| r.query.as_str())
        .collect();
    assert_eq!(custom, ["limit=1000", "limit=1000&pagination_token=page-2"]);

    // Teams without custom voices get 403 there; the listing still works.
    s.mock.push(
        "/v1/tts/voices",
        json_response(
            200,
            json!({"voices": [{"voice_id": "eve", "name": "Eve", "language": "en"}]}),
        ),
    );
    s.mock.push(
        "/v1/custom-voices",
        json_response(403, json!({"error": "forbidden"})),
    );
    let voices = provider.list_voices(&bearer, &cancel).await.unwrap();
    assert_eq!(
        voices.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        ["eve"]
    );

    // The built-in list is required.
    s.mock.push(
        "/v1/tts/voices",
        json_response(403, json!({"error": "forbidden"})),
    );
    s.mock.push(
        "/v1/custom-voices",
        json_response(200, json!({"voices": []})),
    );
    assert_eq!(
        provider.list_voices(&bearer, &cancel).await.unwrap_err(),
        ProviderError::TtsForbidden
    );
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Memory(Mutex<HashMap<String, Secret>>);
impl SecretStore for Memory {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &Secret) -> Result<()> {
        self.0.lock().unwrap().insert(key.into(), value.clone());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
fn tokens(access: &str, expires_at: u64) -> TokenSet {
    TokenSet {
        access_token: Secret::new(access),
        refresh_token: Secret::new("refresh-fixture"),
        expires_at,
        client_id: "dicta-own-client".into(),
    }
}
fn far_future() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

#[tokio::test]
async fn bearer_for_uses_the_requested_mode_and_never_falls_back() {
    let s = http_server().await;
    let oauth =
        OAuthClient::with_endpoints("dicta-own-client", endpoints(s.addr), Timeouts::default())
            .unwrap();
    let credentials = Credentials::with_store(Arc::new(Memory::default()));
    let cancel = CancellationToken::new();
    let dictation_api = AppConfig {
        provider: ProviderMode::ApiKey,
        oauth_client_id: "dicta-own-client".into(),
        ..Default::default()
    };
    let dictation_subscription = AppConfig {
        provider: ProviderMode::Subscription,
        ..dictation_api.clone()
    };
    credentials
        .save_api_key(Secret::new("paid-key-fixture"))
        .await
        .unwrap();
    credentials
        .store_tokens(tokens("oauth-access-fixture", far_future()))
        .await
        .unwrap();

    // The reading mode wins over the dictation mode in config.provider.
    for config in [&dictation_api, &dictation_subscription] {
        for (mode, expected) in [
            (ProviderMode::Subscription, "oauth-access-fixture"),
            (ProviderMode::ApiKey, "paid-key-fixture"),
        ] {
            let bearer = credentials
                .bearer_for_with_oauth(config, mode, false, &cancel, Some(&oauth))
                .await
                .unwrap();
            assert_eq!(bearer.expose(), expected);
        }
    }

    // Subscription without tokens never uses the stored paid key...
    credentials.clear(ProviderMode::Subscription).await.unwrap();
    assert_eq!(
        credentials
            .bearer_for_with_oauth(
                &dictation_api,
                ProviderMode::Subscription,
                false,
                &cancel,
                Some(&oauth)
            )
            .await
            .unwrap_err(),
        ProviderError::MissingCredential
    );
    // ...and API key mode without a key never uses the subscription.
    credentials.clear(ProviderMode::ApiKey).await.unwrap();
    credentials
        .store_tokens(tokens("oauth-access-fixture", far_future()))
        .await
        .unwrap();
    assert_eq!(
        credentials
            .bearer_for(
                &dictation_subscription,
                ProviderMode::ApiKey,
                false,
                &cancel
            )
            .await
            .unwrap_err(),
        ProviderError::MissingCredential
    );
    assert!(s.mock.seen().is_empty(), "no refresh was needed");

    // Dictation and reading share one serialized refresh.
    credentials
        .store_tokens(tokens("expired-fixture", 1))
        .await
        .unwrap();
    s.mock.push(
        "/token",
        json_response(
            200,
            json!({"access_token": "refreshed-fixture", "refresh_token": "r2", "expires_in": 3600, "token_type": "Bearer"}),
        ),
    );
    let (dictation, reading) = tokio::join!(
        credentials.bearer_with_oauth(&dictation_subscription, false, &cancel, Some(&oauth)),
        credentials.bearer_for_with_oauth(
            &dictation_api,
            ProviderMode::Subscription,
            false,
            &cancel,
            Some(&oauth)
        )
    );
    assert_eq!(dictation.unwrap().expose(), "refreshed-fixture");
    assert_eq!(reading.unwrap().expose(), "refreshed-fixture");
    assert_eq!(s.mock.seen().len(), 1);
}
