use thiserror::Error;
pub type Result<T> = std::result::Result<T, ProviderError>;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const KEYRING_UNAVAILABLE: &str = "No se pudo acceder al llavero Secret Service. Desbloqueá o iniciá el llavero de la sesión; no se guardarán secretos en disco plano";
#[cfg(target_os = "macos")]
const KEYRING_UNAVAILABLE: &str = "No se pudo acceder al llavero de macOS (Keychain). Desbloqueá el llavero de inicio de sesión y permití el acceso cuando macOS lo pida; no se guardarán secretos en disco plano";
#[cfg(target_os = "windows")]
const KEYRING_UNAVAILABLE: &str = "No se pudo acceder al Administrador de credenciales de Windows. Revisá que tu sesión de usuario esté iniciada y que ninguna directiva bloquee el guardado de credenciales; no se guardarán secretos en disco plano";

/// Errors contain safe, actionable text. Never put a raw upstream response, URL,
/// token, transcript or reqwest/tungstenite diagnostic in an error variant.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    #[error("Operación cancelada")]
    Cancelled,
    #[error("Se agotó el tiempo de espera de {0}. Revisá la conexión y volvé a intentar")]
    Timeout(&'static str),
    #[error("No se pudo conectar con xAI. Revisá tu conexión y volvé a intentar")]
    Network,
    #[error(
        "La credencial fue rechazada (401). Volvé a conectar Grok o revisá la API key seleccionada"
    )]
    Unauthorized,
    #[error(
        "Tu cuenta o cliente no tiene permiso (403). Revisá autorización de OAuth y acceso a STT. No se cambió a API paga"
    )]
    Forbidden,
    #[error(
        "xAI rechazó la solicitud por cuota o límite (429). Revisá tu consumo o esperá; no se cambió a API paga"
    )]
    RateLimited,
    #[error("xAI devolvió HTTP {0}. Revisá el servicio y volvé a intentar")]
    Http(u16),
    #[error("Respuesta xAI inválida: {0}")]
    Protocol(&'static str),
    #[error(
        "xAI cerró el dictado antes de confirmar el texto final. Se conserva el texto recibido"
    )]
    Disconnected,
    #[error("xAI no pudo procesar el audio. Revisá el formato y volvé a intentar")]
    Upstream,
    #[error("Configuración inválida: {0}")]
    Configuration(&'static str),
    #[error("No hay credencial para el proveedor seleccionado. Conectá Grok o guardá una API key")]
    MissingCredential,
    /// Names the store of the current OS (see KEYRING_UNAVAILABLE).
    #[error("{}", KEYRING_UNAVAILABLE)]
    KeyringUnavailable,
    /// The OS store refused the value for its size (Windows: 2560 bytes per
    /// secret). Credentials degrades on its own for the access token; this only
    /// surfaces when a value that must persist does not fit.
    #[error(
        "La credencial supera el tamaño que admite el almacén del sistema. Usá una API key en Ajustes y reportá el problema; no se guardarán secretos en disco plano"
    )]
    SecretTooLong,
    #[error("La credencial guardada es inválida. Desconectá y volvé a iniciar sesión")]
    InvalidCredential,
    #[error(
        "La autorización de Grok fue denegada. Volvé a conectar si querés intentarlo nuevamente"
    )]
    AuthorizationDenied,
    #[error("Venció el código de autorización. Volvé a conectar Grok para obtener otro")]
    DeviceCodeExpired,
    #[error(
        "Grok rechazó la autorización de esta aplicación. Volvé a conectar; si persiste, actualizá Dicta o revisá el acceso de tu cuenta"
    )]
    OAuthRejected,
    #[error("El consumidor de eventos se cerró")]
    ConsumerClosed,
    /// Lectura en voz alta: nunca reutiliza el texto de 403 del dictado.
    #[error(
        "Tu cuenta no autorizó la lectura en voz alta (403). Si tenés una API key, elegí «API key» en Ajustes → Lectura; no se cambió a API paga automáticamente"
    )]
    TtsForbidden,
    #[error(
        "La voz elegida no existe o tu cuenta no tiene acceso (404). Elegí otra voz en Ajustes → Lectura"
    )]
    TtsVoiceNotFound,
    #[error(
        "xAI rechazó el pedido de lectura (400). Revisá idioma, voz y pronunciaciones en Ajustes → Lectura"
    )]
    TtsRejected,
    #[error(
        "xAI rechazó las pronunciaciones. Revisalas en Ajustes → Lectura: el texto original solo admite letras, números, apóstrofos y espacios"
    )]
    TtsPronunciations,
    #[error(
        "xAI cerró la lectura antes de terminar el audio. Se conserva lo ya generado; podés reintentar desde la frase actual"
    )]
    TtsDisconnected,
    #[error("xAI detuvo la lectura por su política de contenido. Se conserva el audio ya recibido")]
    TtsModerated,
    #[error(
        "xAI no pudo generar la lectura. Se conserva el audio ya recibido; podés reintentar desde la frase actual"
    )]
    TtsUpstream,
    #[error(
        "El fragmento supera los 60 000 caracteres que admite xAI por pedido. Dividí el texto y volvé a intentar"
    )]
    TtsTextTooLong,
}
impl ProviderError {
    pub(crate) fn http(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            429 => Self::RateLimited,
            n => Self::Http(n),
        }
    }
    /// HTTP status of a TTS synthesis request (REST or WebSocket upgrade).
    pub(crate) fn tts_http(status: u16) -> Self {
        match status {
            400 => Self::TtsRejected,
            401 => Self::Unauthorized,
            403 => Self::TtsForbidden,
            404 => Self::TtsVoiceNotFound,
            429 => Self::RateLimited,
            n => Self::Http(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderError;

    #[test]
    fn unavailable_keyring_names_this_systems_store_and_refuses_plaintext() {
        let message = ProviderError::KeyringUnavailable.to_string();
        let store = if cfg!(target_os = "macos") {
            "Keychain"
        } else if cfg!(target_os = "windows") {
            "Administrador de credenciales"
        } else {
            "Secret Service"
        };
        assert!(message.contains(store), "{message}");
        assert!(message.contains("no se guardarán secretos en disco plano"));
    }
}
