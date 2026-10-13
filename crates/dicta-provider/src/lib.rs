//! xAI transport (STT, TTS) and credentials for Dicta. No automatic provider fallback, retry of
//! audio, logging of payloads, login on startup, or credentials from other clients.
//! See README.md for integration and the Stop/Cancel distinction.

// keyring 3 falls back to an in-memory mock on targets without a native store,
// which would silently lose every login. Only build where Cargo.toml wires one.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!(
    "dicta-provider supports Linux (Secret Service), macOS (Keychain) and Windows \
     (Credential Manager) only; other targets would store credentials in memory"
);

mod auth;
mod error;
mod oauth_config;
mod secret;
mod stream;
mod tts;

pub use auth::{Credentials, DeviceCode, KeyringStore, OAuthClient, SecretStore, TokenSet};
pub use error::{ProviderError, Result};
pub use oauth_config::resolve_oauth_client_id;
pub use secret::Secret;
pub use stream::{Endpoints, PolishResult, StreamEvent, Timeouts, XaiProvider};
pub use tokio_util::sync::CancellationToken;
pub use tts::{TtsAudio, TtsEvent, TtsRequest, TtsSessionEnd};
