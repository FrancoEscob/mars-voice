use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::any,
};
use dicta_core::{AppConfig, ProviderMode};
use dicta_provider::{
    CancellationToken, Credentials, Endpoints, OAuthClient, ProviderError, Result, Secret,
    SecretStore, Timeouts, TokenSet, XaiProvider,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;

#[derive(Clone)]
struct Mock {
    queue: Arc<Mutex<VecDeque<(u16, Value)>>>,
    requests: Arc<Mutex<Vec<(Instant, String, Vec<u8>, String)>>>,
}
async fn handler(State(mock): State<Mock>, request: Request<Body>) -> impl IntoResponse {
    let uri = request.uri().to_string();
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    mock.requests
        .lock()
        .unwrap()
        .push((Instant::now(), uri, body.to_vec(), authorization));
    let (status, value) = mock
        .queue
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or((500, json!({"error":"unexpected_request"})));
    (StatusCode::from_u16(status).unwrap(), axum::Json(value))
}
struct Server {
    mock: Mock,
    endpoints: Endpoints,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(queue: Vec<(u16, Value)>) -> Server {
    let mock = Mock {
        queue: Arc::new(Mutex::new(queue.into())),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(any(handler))
        .with_state(mock.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        mock,
        task,
        endpoints: Endpoints {
            device_code: format!("{base}/device"),
            token: format!("{base}/token"),
            responses: format!("{base}/responses"),
            ..Default::default()
        },
    }
}
fn device(expires: u64) -> Value {
    json!({"device_code":"private-device-fixture","user_code":"ABCD","verification_uri":"https://auth.x.ai/device","interval":1,"expires_in":expires})
}
fn tokens() -> Value {
    json!({"access_token":"new-access-fixture","refresh_token":"new-refresh-fixture","expires_in":3600,"token_type":"Bearer"})
}
fn oauth(server: &Server) -> OAuthClient {
    OAuthClient::with_endpoints(
        "dicta-own-client",
        server.endpoints.clone(),
        Timeouts::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn device_login_accepts_xai_accounts_verification_urls() {
    // Shape returned by auth.x.ai on 2026-10-09. No real codes/tokens.
    let mut response = device(300);
    response["verification_uri"] = json!("https://accounts.x.ai/oauth2/device");
    response["verification_uri_complete"] =
        json!("https://accounts.x.ai/oauth2/device?user_code=ABCD");
    let s = server(vec![(200, response)]).await;
    let result = oauth(&s)
        .begin_device_login(&CancellationToken::new())
        .await;
    assert!(result.is_ok(), "official account URL rejected: {result:?}");
}

#[tokio::test]
async fn device_login_rejects_untrusted_verification_urls() {
    for uri in [
        "https://accounts.x.ai.attacker.example/oauth2/device",
        "https://attacker.example/accounts.x.ai",
        "http://accounts.x.ai/oauth2/device",
        "https://user@accounts.x.ai/oauth2/device",
        "https://accounts.x.ai:8443/oauth2/device",
    ] {
        for field in ["verification_uri", "verification_uri_complete"] {
            let mut response = device(300);
            response[field] = json!(uri);
            let s = server(vec![(200, response)]).await;
            assert!(
                matches!(
                    oauth(&s)
                        .begin_device_login(&CancellationToken::new())
                        .await,
                    Err(ProviderError::Protocol(_))
                ),
                "accepted untrusted {field}: {uri}"
            );
        }
    }
}

#[tokio::test]
async fn device_flow_honors_pending_and_slow_down_then_refresh_rotation() {
    let s = server(vec![
        (200, device(30)),
        (400, json!({"error":"authorization_pending"})),
        (400, json!({"error":"slow_down"})),
        (200, tokens()),
        (
            200,
            json!({"access_token":"refreshed-fixture","expires_in":3600}),
        ),
    ])
    .await;
    let client = oauth(&s);
    let cancel = CancellationToken::new();
    let device = client.begin_device_login(&cancel).await.unwrap();
    assert!(!format!("{device:?}").contains("private-device-fixture"));
    let tokens = client.poll_device_login(&device, &cancel).await.unwrap();
    assert_eq!(tokens.client_id, "dicta-own-client");
    let updated = client.refresh(&tokens, &cancel).await.unwrap();
    // xAI can omit refresh_token when it does not rotate it.
    assert_eq!(updated.refresh_token.expose(), "new-refresh-fixture");
    assert_eq!(updated.access_token.expose(), "refreshed-fixture");
    let requests = s.mock.requests.lock().unwrap();
    assert!(requests[3].0.duration_since(requests[2].0) >= Duration::from_secs(6));
    let first = url::form_urlencoded::parse(&requests[0].2).collect::<HashMap<_, _>>();
    assert_eq!(first["client_id"], "dicta-own-client");
    assert_eq!(first["referrer"], "dicta");
    let refresh = url::form_urlencoded::parse(&requests[4].2).collect::<HashMap<_, _>>();
    assert_eq!(refresh["grant_type"], "refresh_token");
    assert_eq!(refresh["refresh_token"], "new-refresh-fixture");
}

#[tokio::test]
async fn cancel_and_expiry_stop_polling_without_token_request() {
    for cancel_immediately in [true, false] {
        let s = server(vec![(200, device(1))]).await;
        let client = oauth(&s);
        let cancel = CancellationToken::new();
        let code = client.begin_device_login(&cancel).await.unwrap();
        if cancel_immediately {
            cancel.cancel();
        }
        let result = client.poll_device_login(&code, &cancel).await;
        assert_eq!(
            result.unwrap_err(),
            if cancel_immediately {
                ProviderError::Cancelled
            } else {
                ProviderError::DeviceCodeExpired
            }
        );
        assert_eq!(s.mock.requests.lock().unwrap().len(), 1);
    }
}

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

impl Memory {
    fn raw(&self, key: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .map(|v| v.expose().to_owned())
    }
}
/// Windows Credential Manager rule as applied by keyring: a secret is stored as
/// UTF-16 and may take at most 2560 bytes.
#[derive(Default)]
struct Limited(Memory);
impl SecretStore for Limited {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        self.0.get(key)
    }
    fn set(&self, key: &str, value: &Secret) -> Result<()> {
        if value.expose().encode_utf16().count() * 2 > 2560 {
            return Err(ProviderError::SecretTooLong);
        }
        self.0.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.0.delete(key)
    }
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn subscription() -> AppConfig {
    AppConfig {
        provider: ProviderMode::Subscription,
        oauth_client_id: "dicta-own-client".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn long_access_token_stays_in_memory_when_the_store_is_size_limited() {
    let first = format!("A{}", "x".repeat(4095));
    let second = format!("B{}", "y".repeat(4095));
    let s = server(vec![(
        200,
        json!({"access_token":second,"refresh_token":"rotated-refresh-fixture","expires_in":3600,"token_type":"Bearer"}),
    )])
    .await;
    let oauth = oauth(&s);
    let store = Arc::new(Limited::default());
    let cancel = CancellationToken::new();
    let config = subscription();
    let credentials = Credentials::with_store(store.clone());
    credentials
        .store_tokens(TokenSet {
            access_token: Secret::new(first.clone()),
            refresh_token: Secret::new("refresh-fixture"),
            expires_at: unix_now() + 3600,
            client_id: "dicta-own-client".into(),
        })
        .await
        .unwrap();
    let primary = store.0.raw("xai-oauth").unwrap();
    assert!(primary.len() < 1280 && !primary.contains(&first));
    assert_eq!(store.0.raw("xai-oauth-access"), None);
    let bearer = credentials
        .bearer_with_oauth(&config, false, &cancel, Some(&oauth))
        .await
        .unwrap();
    assert_eq!(bearer.expose(), first);
    assert_eq!(s.mock.requests.lock().unwrap().len(), 0);

    // Next start: the access token was never stored, so refresh once.
    let restarted = Credentials::with_store(store.clone());
    assert!(
        restarted
            .has_credentials(ProviderMode::Subscription, false)
            .await
            .unwrap()
    );
    for _ in 0..2 {
        let bearer = restarted
            .bearer_with_oauth(&config, false, &cancel, Some(&oauth))
            .await
            .unwrap();
        assert_eq!(bearer.expose(), second);
    }
    let requests = s.mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let form = url::form_urlencoded::parse(&requests[0].2).collect::<HashMap<_, _>>();
    assert_eq!(form["refresh_token"], "refresh-fixture");
    assert!(
        store
            .0
            .raw("xai-oauth")
            .unwrap()
            .contains("rotated-refresh-fixture")
    );
    restarted.clear(ProviderMode::Subscription).await.unwrap();
    assert_eq!(store.0.raw("xai-oauth"), None);
    assert_eq!(
        restarted
            .bearer_with_oauth(&config, false, &cancel, Some(&oauth))
            .await
            .unwrap_err(),
        ProviderError::MissingCredential
    );
}

#[tokio::test]
async fn refresh_token_too_long_for_the_store_keeps_the_previous_login() {
    let s = server(vec![]).await;
    let oauth = oauth(&s);
    let store = Arc::new(Limited::default());
    let credentials = Credentials::with_store(store.clone());
    let previous = TokenSet {
        access_token: Secret::new("previous-access-fixture"),
        refresh_token: Secret::new("previous-refresh-fixture"),
        expires_at: unix_now() + 3600,
        client_id: "dicta-own-client".into(),
    };
    credentials.store_tokens(previous).await.unwrap();
    let stored = (store.0.raw("xai-oauth"), store.0.raw("xai-oauth-access"));
    let error = credentials
        .store_tokens(TokenSet {
            access_token: Secret::new("x".repeat(4096)),
            refresh_token: Secret::new("r".repeat(4096)),
            expires_at: unix_now() + 7200,
            client_id: "dicta-own-client".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error, ProviderError::SecretTooLong);
    assert_eq!(
        (store.0.raw("xai-oauth"), store.0.raw("xai-oauth-access")),
        stored
    );
    let bearer = credentials
        .bearer_with_oauth(
            &subscription(),
            false,
            &CancellationToken::new(),
            Some(&oauth),
        )
        .await
        .unwrap();
    assert_eq!(bearer.expose(), "previous-access-fixture");
    assert_eq!(s.mock.requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn legacy_combined_entry_authenticates_and_is_split_on_next_write() {
    let s = server(vec![(200, tokens())]).await;
    let oauth = oauth(&s);
    let store = Arc::new(Memory::default());
    let cancel = CancellationToken::new();
    let config = subscription();
    // Exactly what earlier versions wrote: the whole TokenSet in one entry.
    let legacy = |expires_at: u64| {
        Secret::new(json!({"access_token":"legacy-access-fixture","refresh_token":"legacy-refresh-fixture","expires_at":expires_at,"client_id":"dicta-own-client"}).to_string())
    };
    store.set("xai-oauth", &legacy(unix_now() + 3600)).unwrap();
    let before = store.raw("xai-oauth");
    let credentials = Credentials::with_store(store.clone());
    assert!(
        credentials
            .has_credentials(ProviderMode::Subscription, false)
            .await
            .unwrap()
    );
    let bearer = credentials
        .bearer_with_oauth(&config, false, &cancel, Some(&oauth))
        .await
        .unwrap();
    assert_eq!(bearer.expose(), "legacy-access-fixture");
    assert_eq!(store.raw("xai-oauth"), before, "reading must not rewrite");
    assert_eq!(store.raw("xai-oauth-access"), None);
    assert_eq!(s.mock.requests.lock().unwrap().len(), 0);

    // An expiring legacy entry refreshes with its refresh token and is
    // rewritten in the split layout.
    store.set("xai-oauth", &legacy(1)).unwrap();
    let bearer = credentials
        .bearer_with_oauth(&config, false, &cancel, Some(&oauth))
        .await
        .unwrap();
    assert_eq!(bearer.expose(), "new-access-fixture");
    let requests = s.mock.requests.lock().unwrap().clone();
    let form = url::form_urlencoded::parse(&requests[0].2).collect::<HashMap<_, _>>();
    assert_eq!(form["refresh_token"], "legacy-refresh-fixture");
    let primary: Value = serde_json::from_str(&store.raw("xai-oauth").unwrap()).unwrap();
    assert!(primary.get("access_token").is_none());
    assert_eq!(primary["refresh_token"], "new-refresh-fixture");
    let access: Value = serde_json::from_str(&store.raw("xai-oauth-access").unwrap()).unwrap();
    assert_eq!(access["access_token"], "new-access-fixture");
    assert_eq!(access["expires_at"], primary["expires_at"]);

    let restarted = Credentials::with_store(store.clone());
    let bearer = restarted
        .bearer_with_oauth(&config, false, &cancel, Some(&oauth))
        .await
        .unwrap();
    assert_eq!(bearer.expose(), "new-access-fixture");
    assert_eq!(s.mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stale_access_entry_is_never_paired_with_a_newer_login() {
    let s = server(vec![(200, tokens())]).await;
    let oauth = oauth(&s);
    let store = Arc::new(Memory::default());
    let expires_at = unix_now() + 3600;
    store
        .set(
            "xai-oauth",
            &Secret::new(json!({"refresh_token":"refresh-fixture","expires_at":expires_at,"client_id":"dicta-own-client"}).to_string()),
        )
        .unwrap();
    store
        .set(
            "xai-oauth-access",
            &Secret::new(
                json!({"access_token":"stale-access-fixture","expires_at":expires_at - 600})
                    .to_string(),
            ),
        )
        .unwrap();
    let bearer = Credentials::with_store(store.clone())
        .bearer_with_oauth(
            &subscription(),
            false,
            &CancellationToken::new(),
            Some(&oauth),
        )
        .await
        .unwrap();
    assert_eq!(bearer.expose(), "new-access-fixture");
    assert_eq!(s.mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn provider_choice_never_falls_back_and_refresh_is_serialized() {
    let s = server(vec![(200, tokens())]).await;
    let oauth = oauth(&s);
    let credentials = Credentials::with_store(Arc::new(Memory::default()));
    credentials
        .save_api_key(Secret::new("paid-key-fixture"))
        .await
        .unwrap();
    let config = AppConfig {
        provider: ProviderMode::Subscription,
        oauth_client_id: "dicta-own-client".into(),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    assert_eq!(
        credentials
            .bearer_with_oauth(&config, false, &cancel, Some(&oauth))
            .await
            .unwrap_err(),
        ProviderError::MissingCredential
    );
    assert_eq!(s.mock.requests.lock().unwrap().len(), 0);
    credentials
        .store_tokens(TokenSet {
            access_token: Secret::new("expired-fixture"),
            refresh_token: Secret::new("refresh-fixture"),
            expires_at: 1,
            client_id: "dicta-own-client".into(),
        })
        .await
        .unwrap();
    assert!(
        credentials
            .has_credentials(ProviderMode::Subscription, false)
            .await
            .unwrap()
    );
    assert_eq!(
        s.mock.requests.lock().unwrap().len(),
        0,
        "presence must not trigger refresh"
    );
    let (a, b) = tokio::join!(
        credentials.bearer_with_oauth(&config, false, &cancel, Some(&oauth)),
        credentials.bearer_with_oauth(&config, false, &cancel, Some(&oauth))
    );
    assert_eq!(a.unwrap().expose(), "new-access-fixture");
    assert_eq!(b.unwrap().expose(), "new-access-fixture");
    assert_eq!(s.mock.requests.lock().unwrap().len(), 1);
    credentials.clear(ProviderMode::Subscription).await.unwrap();
    assert!(
        !credentials
            .has_credentials(ProviderMode::Subscription, false)
            .await
            .unwrap()
    );
    let api = AppConfig {
        provider: ProviderMode::ApiKey,
        ..config
    };
    assert_eq!(
        credentials
            .bearer(&api, false, &cancel)
            .await
            .unwrap()
            .expose(),
        "paid-key-fixture"
    );
}

#[tokio::test]
async fn rejected_refresh_does_not_use_paid_key_or_replace_stored_tokens() {
    let s = server(vec![(
        400,
        json!({"error":"invalid_grant","error_description":"secret-fixture-do-not-log"}),
    )])
    .await;
    let oauth = oauth(&s);
    let credentials = Credentials::with_store(Arc::new(Memory::default()));
    credentials
        .save_api_key(Secret::new("paid-fixture"))
        .await
        .unwrap();
    credentials
        .store_tokens(TokenSet {
            access_token: Secret::new("expired"),
            refresh_token: Secret::new("expired-refresh"),
            expires_at: 1,
            client_id: "dicta-own-client".into(),
        })
        .await
        .unwrap();
    let error = credentials
        .bearer_with_oauth(
            &AppConfig {
                provider: ProviderMode::Subscription,
                ..Default::default()
            },
            false,
            &CancellationToken::new(),
            Some(&oauth),
        )
        .await
        .unwrap_err();
    assert_eq!(error, ProviderError::Unauthorized);
    assert!(!format!("{error:?}").contains("secret-fixture"));
    assert_eq!(s.mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn polish_is_opt_in_uses_same_bearer_and_does_not_accept_truncated_results() {
    let s=server(vec![(200,json!({"status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","content":[{"type":"output_text","text":"Hola, GPUI."}]}]})),(200,json!({"status":"incomplete","output":[{"type":"message","content":[{"type":"output_text","text":"cortado"}]}]}))]).await;
    let provider = XaiProvider::with_endpoints(s.endpoints.clone(), Timeouts::default()).unwrap();
    let cancel = CancellationToken::new();
    let secret = Secret::new("same-credential-fixture");
    let mut config = AppConfig::default();
    assert_eq!(
        provider
            .polish(&config, &secret, "hola GPUI", &cancel)
            .await
            .unwrap(),
        "hola GPUI"
    );
    assert_eq!(s.mock.requests.lock().unwrap().len(), 0);
    config.polish = true;
    assert_eq!(
        provider
            .polish(&config, &secret, "hola GPUI", &cancel)
            .await
            .unwrap(),
        "Hola, GPUI."
    );
    let body: Value = serde_json::from_slice(&s.mock.requests.lock().unwrap()[0].2).unwrap();
    assert_eq!(body["reasoning"]["effort"], "none");
    assert_eq!(
        s.mock.requests.lock().unwrap()[0].3,
        "Bearer same-credential-fixture"
    );
    assert_eq!(body["store"], false);
    assert_eq!(body["input"][1]["content"], "hola GPUI");
    assert!(body.get("tools").is_none());
    assert_eq!(
        provider
            .polish(&config, &secret, "hola GPUI", &cancel)
            .await
            .unwrap_err(),
        ProviderError::Protocol("corrección incompleta")
    );
}

#[tokio::test]
async fn unavailable_keyring_is_an_error_not_plaintext_fallback() {
    struct Broken;
    impl SecretStore for Broken {
        fn get(&self, _: &str) -> Result<Option<Secret>> {
            Err(ProviderError::KeyringUnavailable)
        }
        fn set(&self, _: &str, _: &Secret) -> Result<()> {
            Err(ProviderError::KeyringUnavailable)
        }
        fn delete(&self, _: &str) -> Result<()> {
            Err(ProviderError::KeyringUnavailable)
        }
    }
    let credentials = Credentials::with_store(Arc::new(Broken));
    assert_eq!(
        credentials.save_api_key(Secret::new("fixture")).await,
        Err(ProviderError::KeyringUnavailable)
    );
    let config = AppConfig {
        provider: ProviderMode::ApiKey,
        ..Default::default()
    };
    assert_eq!(
        credentials
            .bearer(&config, false, &CancellationToken::new())
            .await
            .unwrap_err(),
        ProviderError::KeyringUnavailable
    );
}

#[tokio::test]
async fn polish_usage_is_only_the_servers_reported_usage() {
    let output = json!([{"type":"message","content":[{"type":"output_text","text":"Hola."}]}]);
    let s=server(vec![
        (200,json!({"status":"completed","output":output,"usage":{"input_tokens":73,"output_tokens":4}})),
        (200,json!({"status":"completed","output":output})),
    ]).await;
    let provider = XaiProvider::with_endpoints(s.endpoints.clone(), Timeouts::default()).unwrap();
    let config = AppConfig {
        polish: true,
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let secret = Secret::new("fixture");
    let first = provider
        .polish_with_usage(&config, &secret, "hola", &cancel)
        .await
        .unwrap();
    assert_eq!(
        (first.input_tokens, first.output_tokens),
        (Some(73), Some(4))
    );
    assert_eq!(first.text, "Hola.");
    let second = provider
        .polish_with_usage(&config, &secret, "hola", &cancel)
        .await
        .unwrap();
    assert_eq!((second.input_tokens, second.output_tokens), (None, None));
}
