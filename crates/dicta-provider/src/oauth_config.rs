//! Native OAuth configuration. No package discovery, filesystem reads or subprocesses.
use crate::Result;
use dicta_core::AppConfig;

// Public application identifier, NOT an account secret or a Dicta-exclusive client.
// Official source, Apache-2.0, verified 2026-10-09:
// https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/crates/codegen/xai-grok-login/src/config.rs#L380
const GROK_PUBLIC_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// Resolve Dicta's native OAuth settings. A valid advanced config/env override
/// wins; otherwise use the built-in public Grok CLI compatibility identifier.
/// This never inspects another application's installation or credentials.
/// Availability and subscription coverage still depend on xAI's authorization.
pub fn resolve_oauth_client_id(config: &AppConfig) -> Result<String> {
    let env_id = std::env::var("DICTA_OAUTH_CLIENT_ID").ok();
    Ok(resolve_with_override(config, env_id.as_deref()))
}
fn resolve_with_override(config: &AppConfig, env_id: Option<&str>) -> String {
    for override_id in [Some(config.oauth_client_id.as_str()), env_id]
        .into_iter()
        .flatten()
    {
        let id = override_id.trim();
        if valid_override(id) {
            return id.to_owned();
        }
    }
    GROK_PUBLIC_CLIENT_ID.to_owned()
}
fn valid_override(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_configuration_resolves_without_any_installation_or_files() {
        // No home, installation path or filesystem fixture is supplied or needed.
        assert_eq!(
            resolve_with_override(&AppConfig::default(), None),
            "b1a00492-073a-47ea-816f-4c329264a828"
        );
    }
    #[test]
    fn advanced_config_wins_over_environment() {
        let config = AppConfig {
            oauth_client_id: " configured-client ".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve_with_override(&config, Some("env-client")),
            "configured-client"
        );
    }
    #[test]
    fn environment_override_works_with_default_configuration() {
        assert_eq!(
            resolve_with_override(&AppConfig::default(), Some(" env-client ")),
            "env-client"
        );
    }
    #[test]
    fn invalid_overrides_fall_back_to_native_default() {
        for value in [
            "",
            "   ",
            "bad\nclient",
            "bad/client",
            "client?token=bad",
            "cliente-ñ",
        ] {
            let config = AppConfig {
                oauth_client_id: value.into(),
                ..Default::default()
            };
            assert_eq!(
                resolve_with_override(&config, Some(value)),
                GROK_PUBLIC_CLIENT_ID
            );
        }
        let config = AppConfig {
            oauth_client_id: "x".repeat(257),
            ..Default::default()
        };
        assert_eq!(resolve_with_override(&config, None), GROK_PUBLIC_CLIENT_ID);
    }
    #[test]
    fn invalid_config_does_not_mask_valid_environment_override() {
        let config = AppConfig {
            oauth_client_id: "invalid\nvalue".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve_with_override(&config, Some("valid-client")),
            "valid-client"
        );
    }
}
