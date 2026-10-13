use crate::{
    Endpoints, ProviderError as Error, Result, Secret, Timeouts,
    stream::{bounded, http_client, validate_endpoint},
};
use dicta_core::{AppConfig, ProviderMode};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;

/// Keyring service name. Kept from Dicta so existing logins keep working.
const SERVICE: &str = "io.dicta.Dicta";
const API_KEY: &str = "xai-api-key";
const OAUTH: &str = "xai-oauth";
const OAUTH_ACCESS: &str = "xai-oauth-access";
const KEYRING_TIMEOUT: Duration = Duration::from_secs(30);
const OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: Secret,
    pub refresh_token: Secret,
    /// UNIX seconds, with no skew included. bearer() refreshes 60s early.
    pub expires_at: u64,
    pub client_id: String,
}
impl TokenSet {
    pub fn needs_refresh(&self) -> bool {
        expiring(self.expires_at)
    }
}
fn expiring(expires_at: u64) -> bool {
    expires_at <= now().saturating_add(60)
}

/// Stored `xai-oauth` entry. It is written without `access_token` so it fits
/// Windows' 2560-byte limit (1280 UTF-16 units) next to a long JWT. Entries
/// written before the split carry the access token inline (the TokenSet JSON);
/// they are still read and get rewritten in this layout on the next write.
#[derive(Serialize, Deserialize)]
struct StoredOAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<Secret>,
    refresh_token: Secret,
    expires_at: u64,
    client_id: String,
}
/// Best-effort `xai-oauth-access` entry. `expires_at` binds it to the primary
/// entry: a stale one left by a failed write is ignored, never paired with a
/// newer refresh token.
#[derive(Serialize, Deserialize)]
struct StoredAccess {
    access_token: Secret,
    expires_at: u64,
}

/// Contains device credentials. Debug redacts the secret. Show only verification
/// URI and user code in the UI; never serialize this to logs or disk.
#[derive(Clone)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval: Duration,
    pub expires_in: Duration,
    device_code: Secret,
    deadline: Instant,
}
impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceCode([REDACTED])")
    }
}
#[derive(Clone)]
pub struct OAuthClient {
    client_id: String,
    endpoints: Endpoints,
    timeouts: Timeouts,
    http: reqwest::Client,
}
impl OAuthClient {
    /// Native OAuth uses a built-in public Grok compatibility identifier.
    /// Advanced config/env overrides win. Account tokens always belong to Dicta.
    pub fn from_config(config: &AppConfig) -> Result<Self> {
        let client_id = crate::resolve_oauth_client_id(config)?;
        Self::with_endpoints(client_id, Endpoints::default(), Timeouts::default())
    }
    pub fn with_endpoints(
        client_id: impl Into<String>,
        endpoints: Endpoints,
        timeouts: Timeouts,
    ) -> Result<Self> {
        validate_endpoint(&endpoints.device_code, false)?;
        validate_endpoint(&endpoints.token, false)?;
        let client_id = client_id.into();
        if client_id.trim().is_empty() {
            return Err(Error::Configuration(
                "configuración OAuth avanzada inválida; restablecé los ajustes de conexión",
            ));
        }
        Ok(Self {
            client_id: client_id.trim().to_owned(),
            http: http_client(timeouts.http)?,
            endpoints,
            timeouts,
        })
    }
    pub async fn begin_device_login(&self, cancel: &CancellationToken) -> Result<DeviceCode> {
        let started = Instant::now();
        let (status, value) = self
            .form(
                &self.endpoints.device_code,
                &[
                    ("client_id", &self.client_id),
                    ("scope", OAUTH_SCOPE),
                    ("referrer", "dicta"),
                ],
                cancel,
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(auth_error(status, &value));
        }
        let wire: DeviceResponse = serde_json::from_value(value)
            .map_err(|_| Error::Protocol("código de dispositivo inválido"))?;
        if wire.device_code.expose().is_empty()
            || wire.user_code.is_empty()
            || wire.expires_in == 0
            || wire.expires_in > 86400
            || wire.interval == 0
            || wire.interval > 300
        {
            return Err(Error::Protocol("código de dispositivo incompleto"));
        }
        validate_verification_uri(&wire.verification_uri)?;
        if let Some(uri) = &wire.verification_uri_complete {
            validate_verification_uri(uri)?;
        }
        Ok(DeviceCode {
            user_code: wire.user_code,
            verification_uri: wire.verification_uri,
            verification_uri_complete: wire.verification_uri_complete,
            interval: Duration::from_secs(wire.interval),
            expires_in: Duration::from_secs(wire.expires_in),
            device_code: wire.device_code,
            deadline: started + Duration::from_secs(wire.expires_in),
        })
    }
    /// Wait before the first poll, honor pending and RFC 8628 slow_down, and stop
    /// immediately on cancellation. This does not persist tokens automatically.
    pub async fn poll_device_login(
        &self,
        device: &DeviceCode,
        cancel: &CancellationToken,
    ) -> Result<TokenSet> {
        let mut interval = device.interval;
        loop {
            tokio::select! {biased;
                _=cancel.cancelled()=>return Err(Error::Cancelled),
                _=tokio::time::sleep_until(device.deadline)=>return Err(Error::DeviceCodeExpired),
                _=tokio::time::sleep(interval)=>{},
            }
            if Instant::now() >= device.deadline {
                return Err(Error::DeviceCodeExpired);
            }
            let fields = [
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", self.client_id.as_str()),
                ("device_code", device.device_code.expose()),
            ];
            let request = self.form(&self.endpoints.token, &fields, cancel);
            let (status, value) = tokio::select! {biased;
                _=cancel.cancelled()=>return Err(Error::Cancelled),
                _=tokio::time::sleep_until(device.deadline)=>return Err(Error::DeviceCodeExpired),
                result=request=>result?,
            };
            if (200..300).contains(&status) {
                return self.parse_tokens(value, None);
            }
            match value.get("error").and_then(|s| s.as_str()) {
                Some("authorization_pending") => {}
                Some("slow_down") => {
                    interval = interval.saturating_add(Duration::from_secs(5));
                    if let Some(n) = value.get("interval").and_then(|v| v.as_u64()) {
                        interval = interval.max(Duration::from_secs(n.min(86400)));
                    }
                }
                _ => return Err(auth_error(status, &value)),
            }
        }
    }
    pub async fn refresh(&self, tokens: &TokenSet, cancel: &CancellationToken) -> Result<TokenSet> {
        self.refresh_grant(&tokens.client_id, &tokens.refresh_token, cancel)
            .await
    }
    /// Refresh needs no access token, so it also works when that token could
    /// not be persisted (see OAUTH_ACCESS).
    async fn refresh_grant(
        &self,
        client_id: &str,
        refresh_token: &Secret,
        cancel: &CancellationToken,
    ) -> Result<TokenSet> {
        if client_id != self.client_id {
            return Err(client_changed());
        }
        let (status, value) = self
            .form(
                &self.endpoints.token,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", &self.client_id),
                    ("refresh_token", refresh_token.expose()),
                ],
                cancel,
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(auth_error(status, &value));
        }
        self.parse_tokens(value, Some(refresh_token))
    }
    fn parse_tokens(
        &self,
        value: serde_json::Value,
        previous: Option<&Secret>,
    ) -> Result<TokenSet> {
        let wire: TokenResponse = serde_json::from_value(value)
            .map_err(|_| Error::Protocol("respuesta OAuth inválida"))?;
        let refresh = wire
            .refresh_token
            .or_else(|| previous.cloned())
            .ok_or(Error::Protocol("OAuth no entregó refresh_token"))?;
        if wire.access_token.expose().is_empty()
            || refresh.expose().is_empty()
            || wire.expires_in == 0
            || wire.expires_in > 31536000
            || wire
                .token_type
                .as_deref()
                .is_some_and(|v| !v.eq_ignore_ascii_case("bearer"))
        {
            return Err(Error::Protocol("credenciales OAuth incompletas"));
        }
        Ok(TokenSet {
            access_token: wire.access_token,
            refresh_token: refresh,
            expires_at: now().saturating_add(wire.expires_in),
            client_id: self.client_id.clone(),
        })
    }
    async fn form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        cancel: &CancellationToken,
    ) -> Result<(u16, serde_json::Value)> {
        bounded(cancel, self.timeouts.http, "autorización OAuth", async {
            let response = self
                .http
                .post(url)
                .form(fields)
                .send()
                .await
                .map_err(|_| Error::Network)?;
            let status = response.status().as_u16();
            // Auth status remains actionable even if a proxy returns HTML.
            if matches!(status, 401 | 403 | 429) {
                return Err(Error::http(status));
            }
            let body = response
                .json()
                .await
                .map_err(|_| Error::Protocol("respuesta OAuth no es JSON"))?;
            Ok((status, body))
        })
        .await
    }
}
#[derive(Deserialize)]
struct DeviceResponse {
    device_code: Secret,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "poll_interval")]
    interval: u64,
}
fn poll_interval() -> u64 {
    5
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: Secret,
    refresh_token: Option<Secret>,
    #[serde(default = "token_lifetime")]
    expires_in: u64,
    token_type: Option<String>,
}
fn token_lifetime() -> u64 {
    3600
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn validate_verification_uri(uri: &str) -> Result<()> {
    let u = url::Url::parse(uri).map_err(|_| Error::Protocol("URL de autorización inválida"))?;
    if u.scheme() != "https"
        || !matches!(u.host_str(), Some("auth.x.ai" | "accounts.x.ai"))
        || u.port_or_known_default() != Some(443)
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err(Error::Protocol(
            "URL de autorización fuera de los dominios oficiales de xAI",
        ));
    }
    Ok(())
}
fn client_changed() -> Error {
    Error::Configuration(
        "la configuración de autorización cambió; desconectá y volvé a autorizar Grok",
    )
}
fn auth_error(status: u16, value: &serde_json::Value) -> Error {
    match value.get("error").and_then(|s| s.as_str()) {
        Some("access_denied" | "authorization_denied") => Error::AuthorizationDenied,
        Some("expired_token") => Error::DeviceCodeExpired,
        Some("invalid_grant") => Error::Unauthorized,
        Some("invalid_client" | "unauthorized_client" | "invalid_scope") => Error::OAuthRejected,
        _ => Error::http(status),
    }
}

/// Synchronous persistence interface. Credentials runs it on spawn_blocking so
/// unlocking a session keyring cannot stall Tokio/audio. Tests use an in-memory
/// implementation and never access the real user's keyring. `set` returns
/// `ProviderError::SecretTooLong` when the backend refuses a value for its size
/// (Windows Credential Manager: 2560 bytes), so Credentials can degrade.
pub trait SecretStore: Send + Sync + 'static {
    fn get(&self, key: &str) -> Result<Option<Secret>>;
    fn set(&self, key: &str, value: &Secret) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}
/// The OS store under the service `io.dicta.Dicta`: Secret Service on Linux,
/// Keychain on macOS, Credential Manager on Windows (see Cargo.toml). There is
/// no plaintext fallback; a failing store is an error for the user.
#[derive(Clone, Default)]
pub struct KeyringStore;
impl KeyringStore {
    fn entry(key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, key).map_err(|_| Error::KeyringUnavailable)
    }
}
impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        match Self::entry(key)?.get_password() {
            Ok(s) => Ok(Some(Secret::new(s))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(Error::KeyringUnavailable),
        }
    }
    fn set(&self, key: &str, value: &Secret) -> Result<()> {
        Self::entry(key)?
            .set_password(value.expose())
            .map_err(|e| match e {
                keyring::Error::TooLong(..) => Error::SecretTooLong,
                _ => Error::KeyringUnavailable,
            })
    }
    fn delete(&self, key: &str) -> Result<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(Error::KeyringUnavailable),
        }
    }
}
#[derive(Clone)]
pub struct Credentials {
    store: Arc<dyn SecretStore>,
    /// Serializes OAuth reads, refreshes and writes across clones, and keeps the
    /// last token set seen. That copy lets an access token the store could not
    /// keep (too long for Windows) serve this session; it is used only while it
    /// matches the stored `xai-oauth` entry, which stays the source of truth.
    session: Arc<Mutex<Option<TokenSet>>>,
}
impl Credentials {
    pub fn new() -> Self {
        Self::with_store(Arc::new(KeyringStore))
    }
    pub fn with_store(store: Arc<dyn SecretStore>) -> Self {
        Self {
            store,
            session: Arc::new(Mutex::new(None)),
        }
    }
    /// Presence only: no network, refresh, OAuth login or writes. True does NOT
    /// imply the service accepted the credential or that subscription covers STT.
    pub async fn has_credentials(
        &self,
        mode: ProviderMode,
        allow_env_api_key: bool,
    ) -> Result<bool> {
        if mode == ProviderMode::ApiKey && allow_env_api_key && env_api_key().is_some() {
            return Ok(true);
        }
        Ok(self
            .read(match mode {
                ProviderMode::ApiKey => API_KEY,
                ProviderMode::Subscription => OAUTH,
            })
            .await?
            .is_some())
    }
    pub async fn save_api_key(&self, key: Secret) -> Result<()> {
        if key.expose().trim().is_empty() || key.expose().contains(['\r', '\n']) {
            return Err(Error::InvalidCredential);
        }
        self.write(API_KEY, key).await
    }
    pub async fn store_tokens(&self, tokens: TokenSet) -> Result<()> {
        let mut session = self.session.lock().await;
        self.write_tokens(&mut session, tokens).await
    }
    /// `xai-oauth` without the access token first, then the access token on its
    /// own. If that second write fails (too long for the OS store, or anything
    /// else), the token lives only in memory and the next start refreshes once.
    async fn write_tokens(&self, session: &mut Option<TokenSet>, tokens: TokenSet) -> Result<()> {
        let primary = StoredOAuth {
            access_token: None,
            refresh_token: tokens.refresh_token.clone(),
            expires_at: tokens.expires_at,
            client_id: tokens.client_id.clone(),
        };
        self.write(OAUTH, to_secret(&primary)?).await?;
        let access = StoredAccess {
            access_token: tokens.access_token.clone(),
            expires_at: tokens.expires_at,
        };
        if self.write(OAUTH_ACCESS, to_secret(&access)?).await.is_err() {
            // An older companion no longer matches expires_at and would be
            // ignored; removing it just avoids leaving a live token behind.
            let _ = self.remove(OAUTH_ACCESS).await;
        }
        *session = Some(tokens);
        Ok(())
    }
    pub async fn clear(&self, mode: ProviderMode) -> Result<()> {
        let mut session = self.session.lock().await;
        match mode {
            ProviderMode::ApiKey => self.remove(API_KEY).await,
            ProviderMode::Subscription => {
                *session = None;
                // Companion first: if a delete fails, the stored login stays whole.
                self.remove(OAUTH_ACCESS).await?;
                self.remove(OAUTH).await
            }
        }
    }
    pub async fn bearer(
        &self,
        config: &AppConfig,
        allow_env_api_key: bool,
        cancel: &CancellationToken,
    ) -> Result<Secret> {
        self.bearer_for_with_oauth(config, config.provider, allow_env_api_key, cancel, None)
            .await
    }
    /// Inject the OAuth transport for local integration tests. Production callers
    /// normally use bearer(), which resolves the client ID from current config.
    pub async fn bearer_with_oauth(
        &self,
        config: &AppConfig,
        allow_env_api_key: bool,
        cancel: &CancellationToken,
        oauth: Option<&OAuthClient>,
    ) -> Result<Secret> {
        self.bearer_for_with_oauth(config, config.provider, allow_env_api_key, cancel, oauth)
            .await
    }
    /// Same as bearer() but for an explicit mode: reading aloud has its own
    /// selector (`config.tts.provider`). `config.provider` is ignored here; the
    /// config is used only for the OAuth client ID. Same keyring entries and the
    /// same serialized OAuth refresh as dictation. Never falls back from
    /// subscription to API key or vice versa.
    pub async fn bearer_for(
        &self,
        config: &AppConfig,
        mode: ProviderMode,
        allow_env_api_key: bool,
        cancel: &CancellationToken,
    ) -> Result<Secret> {
        self.bearer_for_with_oauth(config, mode, allow_env_api_key, cancel, None)
            .await
    }
    /// bearer_for() with an injected OAuth transport, for local integration tests.
    pub async fn bearer_for_with_oauth(
        &self,
        config: &AppConfig,
        mode: ProviderMode,
        allow_env_api_key: bool,
        cancel: &CancellationToken,
        oauth: Option<&OAuthClient>,
    ) -> Result<Secret> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if mode == ProviderMode::ApiKey {
            if allow_env_api_key && let Some(key) = env_api_key() {
                return Ok(key);
            }
            return bounded(cancel, KEYRING_TIMEOUT, "llavero", async {
                self.read(API_KEY).await?.ok_or(Error::MissingCredential)
            })
            .await;
        }
        let oauth = match oauth {
            Some(oauth) => oauth.clone(),
            None => OAuthClient::from_config(config)?,
        };
        let mut session = tokio::select! {biased;_=cancel.cancelled()=>return Err(Error::Cancelled),guard=self.session.lock()=>guard};
        let stored = bounded(
            cancel,
            KEYRING_TIMEOUT,
            "llavero",
            self.read_tokens(&mut session),
        )
        .await?;
        if stored.client_id != oauth.client_id {
            return Err(client_changed());
        }
        if let Some(access) = stored.access_token.filter(|_| !expiring(stored.expires_at)) {
            return Ok(access);
        }
        // Expiring, or the access token was not kept (see write_tokens).
        let tokens = oauth
            .refresh_grant(&stored.client_id, &stored.refresh_token, cancel)
            .await?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let access = tokens.access_token.clone();
        self.write_tokens(&mut session, tokens).await?;
        Ok(access)
    }
    /// The stored OAuth entry with `access_token` taken from, in order: the
    /// legacy inline field, this session's copy, or the companion entry. None
    /// means a refresh is needed.
    async fn read_tokens(&self, session: &mut Option<TokenSet>) -> Result<StoredOAuth> {
        let raw = self.read(OAUTH).await?.ok_or(Error::MissingCredential)?;
        let mut stored: StoredOAuth =
            serde_json::from_str(raw.expose()).map_err(|_| Error::InvalidCredential)?;
        if stored.access_token.is_some() {
            // Legacy combined entry: rewritten in the split layout on the next write.
            return Ok(stored);
        }
        if let Some(known) = session.as_ref().filter(|known| {
            known.expires_at == stored.expires_at
                && known.client_id == stored.client_id
                && known.refresh_token.expose() == stored.refresh_token.expose()
        }) {
            stored.access_token = Some(known.access_token.clone());
            return Ok(stored);
        }
        let companion = self
            .read(OAUTH_ACCESS)
            .await?
            .and_then(|raw| serde_json::from_str::<StoredAccess>(raw.expose()).ok())
            .filter(|a| a.expires_at == stored.expires_at && !a.access_token.expose().is_empty());
        if let Some(companion) = companion {
            *session = Some(TokenSet {
                access_token: companion.access_token.clone(),
                refresh_token: stored.refresh_token.clone(),
                expires_at: stored.expires_at,
                client_id: stored.client_id.clone(),
            });
            stored.access_token = Some(companion.access_token);
        }
        Ok(stored)
    }
    async fn read(&self, key: &'static str) -> Result<Option<Secret>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.get(key))
            .await
            .map_err(|_| Error::KeyringUnavailable)?
    }
    async fn write(&self, key: &'static str, value: Secret) -> Result<()> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.set(key, &value))
            .await
            .map_err(|_| Error::KeyringUnavailable)?
    }
    async fn remove(&self, key: &'static str) -> Result<()> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.delete(key))
            .await
            .map_err(|_| Error::KeyringUnavailable)?
    }
}
impl Default for Credentials {
    fn default() -> Self {
        Self::new()
    }
}
fn to_secret(value: &impl Serialize) -> Result<Secret> {
    serde_json::to_string(value)
        .map(Secret::new)
        .map_err(|_| Error::InvalidCredential)
}
fn env_api_key() -> Option<Secret> {
    std::env::var("DICTA_XAI_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(Secret::new)
}
