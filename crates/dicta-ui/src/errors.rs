//! Where a backend error comes from, so the UI can title it and offer the
//! right next step. `AppSnapshot::error` is plain, user-safe Spanish text
//! without a kind, so this only looks at markers Dicta itself writes:
//!
//! - Microphone: the audio worker prefixes capture failures with
//!   `No se pudo capturar audio:` and its actionable messages point to
//!   `Ajustes > Micrófono` (docs/tickets/12-audio-target.md).
//! - Credential: the controller sets [`CREDENTIAL_STATUS`] as the status of
//!   every failed sign-in, key save or refresh.
//!
//! Raw library text (PipeWire, HTTP) is never parsed. Anything unrecognised
//! falls back to a generic dictation or app error that keeps the full
//! message. A typed error kind in the snapshot would replace this.
//!
//! The markers stay in Spanish on purpose: they match what the controller
//! writes today, whatever the interface language. Only titles are localised.

use dicta_core::{AppSnapshot, Phase};

use crate::i18n::{Lang, Msg};

/// Status the controller sets when a credential operation fails.
pub const CREDENTIAL_STATUS: &str = "No se pudo actualizar la credencial";

const AUDIO_PREFIX: &str = "No se pudo capturar audio";
const MIC_SETTINGS: &str = "Ajustes > Micrófono";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorOrigin {
    /// The microphone could not be opened or vanished mid-dictation.
    Microphone,
    /// Sign-in, API key or credential refresh failed.
    Credential,
    /// A dictation failed for another reason (network, provider).
    Dictation,
    /// Anything else, e.g. saving the configuration.
    App,
}

impl ErrorOrigin {
    pub fn of(snap: &AppSnapshot) -> Option<Self> {
        let error = snap.error.as_deref()?;
        Some(
            if error.starts_with(AUDIO_PREFIX) || error.contains(MIC_SETTINGS) {
                Self::Microphone
            } else if snap.status == CREDENTIAL_STATUS {
                Self::Credential
            } else if snap.phase == Phase::Error {
                Self::Dictation
            } else {
                Self::App
            },
        )
    }

    /// Short heading for the overlay and the settings banner.
    pub fn title(self, l: Lang) -> &'static str {
        l.t(match self {
            Self::Microphone => Msg::ErrTitleMic,
            Self::Credential => Msg::ErrTitleCredential,
            Self::Dictation => Msg::ErrTitleDictation,
            Self::App => Msg::ErrTitleApp,
        })
    }

    /// The error text without the technical prefix the title already says.
    pub fn body(self, error: &str) -> String {
        match self {
            Self::Microphone => error
                .strip_prefix(AUDIO_PREFIX)
                .map(|rest| rest.trim_start_matches(':').trim())
                .filter(|rest| !rest.is_empty())
                .unwrap_or(error)
                .to_string(),
            _ => error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicta_core::AppConfig;

    fn snap(phase: Phase, status: &str, error: &str) -> AppSnapshot {
        let mut s = AppSnapshot::new(AppConfig::default());
        s.phase = phase;
        s.status = status.into();
        s.error = Some(error.into());
        s
    }

    #[test]
    fn audio_failure_during_dictation_is_a_microphone_error_not_a_credential_one() {
        // The reported case: credential available, dictation failed on audio.
        let mut s = snap(
            Phase::Error,
            "No se completó el dictado; los fragmentos quedan visibles",
            "No se pudo capturar audio: El micrófono predeterminado está desconectado o ya no \
             está disponible. Reconectalo o elegí uno disponible en Ajustes > Micrófono",
        );
        s.authenticated = true;
        let origin = ErrorOrigin::of(&s);
        assert_eq!(origin, Some(ErrorOrigin::Microphone));
        assert!(
            origin
                .unwrap()
                .body(s.error.as_deref().unwrap())
                .starts_with("El micrófono predeterminado")
        );
        // Unknown PipeWire causes keep their full text.
        let raw = "No se pudo capturar audio: PipeWire: algo nuevo";
        assert_eq!(ErrorOrigin::Microphone.body(raw), "PipeWire: algo nuevo");
    }

    #[test]
    fn other_errors_keep_their_origin() {
        let s = snap(Phase::Idle, CREDENTIAL_STATUS, "El código venció");
        assert_eq!(ErrorOrigin::of(&s), Some(ErrorOrigin::Credential));
        let s = snap(
            Phase::Error,
            "No se completó el dictado",
            "Grok no respondió",
        );
        assert_eq!(ErrorOrigin::of(&s), Some(ErrorOrigin::Dictation));
        let s = snap(Phase::Idle, "Listo", "No se pudo guardar la configuración");
        assert_eq!(ErrorOrigin::of(&s), Some(ErrorOrigin::App));
        let mut s = snap(Phase::Idle, "Listo", "x");
        s.error = None;
        assert_eq!(ErrorOrigin::of(&s), None);
    }
}
