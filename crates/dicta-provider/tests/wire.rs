use dicta_core::AppConfig;
use dicta_provider::{
    CancellationToken, Endpoints, ProviderError, Secret, StreamEvent, Timeouts, XaiProvider,
};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_tungstenite::{
    accept_async, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

fn client(addr: std::net::SocketAddr, timeouts: Timeouts) -> XaiProvider {
    XaiProvider::with_endpoints(
        Endpoints {
            stt: format!("ws://{addr}/v1/stt"),
            ..Default::default()
        },
        timeouts,
    )
    .unwrap()
}
fn short_timeouts() -> Timeouts {
    Timeouts {
        ready: Duration::from_millis(100),
        finalize: Duration::from_millis(100),
        ..Default::default()
    }
}

#[tokio::test]
async fn waits_for_created_sends_binary_then_audio_done_and_preserves_wire_flags() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_hdr_async(tcp, |req: &Request, response: Response| {
            assert_eq!(req.headers()["authorization"], "Bearer fixture-only");
            let url = url::Url::parse(&format!("ws://localhost{}", req.uri())).unwrap();
            let pairs = url.query_pairs().collect::<Vec<_>>();
            assert!(
                pairs
                    .iter()
                    .any(|(k, v)| k == "sample_rate" && v == "16000")
            );
            assert!(
                pairs
                    .iter()
                    .any(|(k, v)| k == "interim_results" && v == "true")
            );
            assert!(pairs.iter().all(|(k, _)| k != "smart_turn"));
            assert_eq!(
                pairs
                    .iter()
                    .filter(|(k, _)| k == "keyterm")
                    .map(|(_, v)| v.to_string())
                    .collect::<Vec<_>>(),
                ["C++ & Rust", "Ñandú"]
            );
            Ok(response)
        })
        .await
        .unwrap();
        // Audio is already queued at the client; none may leave before created.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), ws.next())
                .await
                .is_err()
        );
        ws.send(Message::Text(
            r#"{"type":"transcript.created","id":"local"}"#.into(),
        ))
        .await
        .unwrap();
        match ws.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => assert_eq!(bytes.len(), 3200),
            other => panic!("expected binary, got {other:?}"),
        }
        ws.send(Message::Text(
            r#"{"type":"transcript.partial","text":"Ho","is_final":false,"speech_final":false}"#
                .into(),
        ))
        .await
        .unwrap();
        ws.send(Message::Text(r#"{"type":"transcript.partial","text":"Hola","is_final":true,"speech_final":false,"words":[{"start":0.1,"end":0.4}]}"#.into())).await.unwrap();
        ws.send(Message::Text(r#"{"type":"transcript.partial","text":"Hola mundo","is_final":true,"speech_final":true,"start":0.1,"duration":0.8}"#.into())).await.unwrap();
        let done = ws.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(done.to_text().unwrap()).unwrap()["type"],
            "audio.done"
        );
        ws.send(Message::Text(
            r#"{"type":"transcript.done","text":"","duration":1.0}"#.into(),
        ))
        .await
        .unwrap();
    });
    let provider = client(addr, Timeouts::default());
    let (audio_tx, audio_rx) = mpsc::channel(2);
    audio_tx.send(vec![0; 3200]).await.unwrap();
    drop(audio_tx);
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let config = AppConfig {
        keyterms: vec!["C++ & Rust".into(), "Ñandú".into()],
        ..Default::default()
    };
    provider
        .stream(
            &config,
            &Secret::new("fixture-only"),
            audio_rx,
            events_tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(events_rx.recv().await, Some(StreamEvent::Started));
    assert!(matches!(
        events_rx.recv().await,
        Some(StreamEvent::Partial {
            is_final: false,
            speech_final: false,
            ..
        })
    ));
    assert!(matches!(
        events_rx.recv().await,
        Some(StreamEvent::Partial {
            is_final: true,
            speech_final: false,
            start: Some(0.1),
            ..
        })
    ));
    assert!(matches!(
        events_rx.recv().await,
        Some(StreamEvent::Partial {
            is_final: true,
            speech_final: true,
            ..
        })
    ));
    assert_eq!(
        events_rx.recv().await,
        Some(StreamEvent::Final {
            text: String::new(),
            duration: Some(1.0)
        })
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancel_drops_session_without_audio_done_even_with_audio_queued() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();
        ws.send(Message::Text(r#"{"type":"transcript.created"}"#.into()))
            .await
            .unwrap();
        while let Some(Ok(frame)) = ws.next().await {
            assert!(
                !matches!(frame, Message::Text(_)),
                "Cancel must not send audio.done"
            );
        }
    });
    let (audio_tx, audio_rx) = mpsc::channel(2);
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        client(addr, Timeouts::default())
            .stream(
                &AppConfig::default(),
                &Secret::new("fixture"),
                audio_rx,
                events_tx,
                task_cancel,
            )
            .await
    });
    assert_eq!(events_rx.recv().await, Some(StreamEvent::Started));
    cancel.cancel();
    drop(audio_tx);
    assert_eq!(task.await.unwrap(), Err(ProviderError::Cancelled));
    assert!(events_rx.recv().await.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn handshake_statuses_are_actionable_and_do_not_echo_response_secrets() {
    for (status, expected) in [
        (401, ProviderError::Unauthorized),
        (403, ProviderError::Forbidden),
        (429, ProviderError::RateLimited),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            stream.read(&mut buf).await.unwrap();
            stream.write_all(format!("HTTP/1.1 {status} Failed\r\nContent-Length: 18\r\nConnection: close\r\n\r\nsuper-secret-token").as_bytes()).await.unwrap();
        });
        let (_audio_tx, audio_rx) = mpsc::channel(2);
        let (events_tx, _events_rx) = mpsc::channel(8);
        let error = client(addr, Timeouts::default())
            .stream(
                &AppConfig::default(),
                &Secret::new("fixture"),
                audio_rx,
                events_tx,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().contains("super-secret"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn missing_created_and_missing_final_are_bounded() {
    for ready in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            if ready {
                ws.send(Message::Text(r#"{"type":"transcript.created"}"#.into()))
                    .await
                    .unwrap();
                assert_eq!(
                    ws.next().await.unwrap().unwrap().to_text().unwrap(),
                    r#"{"type":"audio.done"}"#
                );
            }
            let _ = ws.next().await;
        });
        let (audio_tx, audio_rx) = mpsc::channel(2);
        drop(audio_tx);
        let (events_tx, _events_rx) = mpsc::channel(8);
        let error = client(addr, short_timeouts())
            .stream(
                &AppConfig::default(),
                &Secret::new("fixture"),
                audio_rx,
                events_tx,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ProviderError::Timeout(if ready {
                "resultado final STT"
            } else {
                "inicio STT"
            })
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn disconnect_before_final_is_not_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();
        ws.send(Message::Text(r#"{"type":"transcript.created"}"#.into()))
            .await
            .unwrap();
        let _ = ws.next().await;
        ws.close(None).await.unwrap();
    });
    let (audio_tx, audio_rx) = mpsc::channel(1);
    drop(audio_tx);
    // Keep events receiver alive so we test disconnect, not consumer closure.
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let result = client(addr, Timeouts::default())
        .stream(
            &AppConfig::default(),
            &Secret::new("fixture"),
            audio_rx,
            events_tx,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result, Err(ProviderError::Disconnected));
    assert_eq!(events_rx.recv().await, Some(StreamEvent::Started));
    server.await.unwrap();
}
