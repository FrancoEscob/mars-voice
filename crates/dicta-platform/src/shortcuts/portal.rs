//! Linux backend for [`super::run`]: the opt-in XDG GlobalShortcuts portal
//! (ashpd). May display compositor authorization UI. If unsupported, offer
//! [`super::HYPRLAND_BIND_EXAMPLE`] rather than writing user config.
use super::event_stamp;
use anyhow::{Context, Result, anyhow, bail, ensure};
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use dicta_core::{AppConfig, Command};
use futures_util::StreamExt;
use std::sync::mpsc::Sender;
use tokio::sync::watch;

/// Run until stop changes to true / its sender is dropped. Portal support and
/// user permission are required. Errors should be shown with the bind fallback.
/// Do not bind portal and manual compositor shortcuts to the same combination.
pub(super) async fn run(
    config: &AppConfig,
    commands: Sender<Command>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    if *stop.borrow() {
        return Ok(());
    }
    let proxy = GlobalShortcuts::new()
        .await
        .context("El portal GlobalShortcuts no está disponible; usá los binds de Hyprland")?;
    let session = proxy.create_session().await?;
    // ashpd 0.11 exposes the object path via Serialize, not a public getter.
    let session_path = serde_json::to_value(&session)?;
    let session_path = session_path.as_str().context("Ruta de sesión inválida")?;
    let specs = portal_bindings(config)?;
    let bindings: Vec<_> = specs
        .iter()
        .map(|spec| {
            NewShortcut::new(spec.id, spec.description).preferred_trigger(spec.trigger.as_str())
        })
        .collect();
    let mut events = proxy.receive_activated().await?;
    let mut releases = proxy.receive_deactivated().await?;
    let mut portal_order = PortalOrder::default();
    let result=async {
        let request = tokio::select! {
            request = proxy.bind_shortcuts(&session,&bindings,None) => request?,
            _ = cancelled(&mut stop) => return Ok(()),
        };
        let bound=request.response()?;
        ensure!(bound.shortcuts().iter().any(|s|s.id()=="toggle") && bound.shortcuts().iter().any(|s|s.id()=="paste"),"El compositor no registró todos los atajos; usá los binds de Hyprland");
        loop {
            tokio::select! {
                changed=stop.changed()=>{if changed.is_err() || *stop.borrow() {break;}},
                event=releases.next()=>{
                    let event=event.context("El portal cerró el flujo de liberación de atajos")?;
                    if event.session_handle().as_str()!=session_path || event.shortcut_id()!="toggle" {continue;}
                    if !portal_order.accept(event.timestamp(), false) {continue;}
                    if commands.send(Command::ShortcutReleased { at: event_stamp(), token: None }).is_err() {break;}
                }
                event=events.next()=>{
                    let event=event.context("El portal cerró el flujo de atajos")?;
                    // A portal can broadcast another application's session.
                    if event.session_handle().as_str()!=session_path {continue;}
                    let command=match event.shortcut_id() {
                        "toggle" if portal_order.accept(event.timestamp(), true)=>Command::ShortcutPressed { at: event_stamp(), token: None },
                        "paste"=>Command::Paste,
                        "tts-read"=>Command::ReadSelection,
                        "tts-pause"=>Command::TtsToggle,
                        "tts-stop"=>Command::TtsStop,
                        _=>continue
                    };
                    if commands.send(command).is_err() {break;}
                }
            }
        }
        Ok(())
    }.await;
    // Losing the portal must not leave a held microphone running forever.
    let _ = commands.send(Command::ShortcutReleased {
        at: event_stamp(),
        token: None,
    });
    let _ = session.close().await;
    result
}
#[derive(Debug, PartialEq, Eq)]
struct BindingSpec {
    id: &'static str,
    description: &'static str,
    trigger: String,
}

/// Dictation toggle/paste are mandatory. Reading shortcuts are registered only
/// when configured (non-empty); only toggle/paste must be confirmed as bound.
fn portal_bindings(config: &AppConfig) -> Result<Vec<BindingSpec>> {
    let required = [
        (
            "toggle",
            "Iniciar / detener dictado",
            &config.toggle_shortcut,
        ),
        ("paste", "Pegar último dictado", &config.paste_shortcut),
    ];
    let optional = [
        (
            "tts-read",
            "Leer selección en voz alta",
            &config.tts.read_shortcut,
        ),
        (
            "tts-pause",
            "Pausar / reanudar lectura",
            &config.tts.pause_shortcut,
        ),
        ("tts-stop", "Detener lectura", &config.tts.stop_shortcut),
    ];
    let mut specs: Vec<BindingSpec> = Vec::new();
    let configured = optional
        .into_iter()
        .filter(|(_, _, shortcut)| !shortcut.trim().is_empty());
    for (id, description, shortcut) in required.into_iter().chain(configured) {
        let trigger = trigger(shortcut).map_err(|e| anyhow!("{description}: {e}"))?;
        if let Some(other) = specs.iter().find(|spec| spec.trigger == trigger) {
            bail!(
                "«{description}» y «{}» usan la misma combinación; elegí otra en Ajustes",
                other.description
            );
        }
        specs.push(BindingSpec {
            id,
            description,
            trigger,
        });
    }
    Ok(specs)
}

// Activated and Deactivated use separate signal streams. A release selected
// first must suppress an older buffered activation, even for a very short tap.
#[derive(Default)]
struct PortalOrder {
    latest: Option<(std::time::Duration, bool)>,
}
impl PortalOrder {
    fn accept(&mut self, timestamp: std::time::Duration, press: bool) -> bool {
        if let Some((previous, was_press)) = self.latest {
            if timestamp < previous || (timestamp == previous && (!was_press || press)) {
                return false;
            }
        }
        self.latest = Some((timestamp, press));
        true
    }
}

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            break;
        }
    }
}
fn trigger(shortcut: &str) -> Result<String> {
    let mut parts: Vec<_> = shortcut.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    ensure!(!parts.is_empty(), "El atajo requiere modificador y tecla");
    let mut result = Vec::new();
    for part in parts {
        let modifier = match part.to_ascii_uppercase().as_str() {
            "SUPER" | "LOGO" => "LOGO",
            "CTRL" | "CONTROL" => "CTRL",
            "SHIFT" => "SHIFT",
            "ALT" => "ALT",
            _ => anyhow::bail!("Usá modificadores seguidos de una letra, número o Space"),
        };
        ensure!(
            !result.contains(&modifier.to_owned()),
            "El modificador está repetido"
        );
        result.push(modifier.to_owned());
    }
    let key = if key.eq_ignore_ascii_case("space") {
        "space".to_owned()
    } else {
        ensure!(
            key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric()),
            "Atajo no compatible: usá una letra, número o Space"
        );
        key.to_ascii_lowercase()
    };
    result.push(key);
    Ok(result.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portal_release_wins_over_buffered_or_same_tick_press() {
        let mut order = PortalOrder::default();
        let t = std::time::Duration::from_millis(10);
        assert!(order.accept(t, false));
        assert!(!order.accept(t, true));
        assert!(!order.accept(t - std::time::Duration::from_millis(1), true));
        let t = t + std::time::Duration::from_millis(1);
        assert!(order.accept(t, true));
        assert!(order.accept(t, false));
    }
    #[test]
    fn reading_shortcuts_register_only_when_configured() {
        let mut config = AppConfig {
            toggle_shortcut: "Ctrl+Space".into(),
            paste_shortcut: "Super+Shift+V".into(),
            ..Default::default()
        };
        config.tts.read_shortcut = "Ctrl+Alt+Space".into();
        config.tts.pause_shortcut = String::new();
        config.tts.stop_shortcut = "  ".into();
        let ids = |specs: Vec<BindingSpec>| -> Vec<_> {
            specs.into_iter().map(|s| (s.id, s.trigger)).collect()
        };
        assert_eq!(
            ids(portal_bindings(&config).unwrap()),
            vec![
                ("toggle", "CTRL+space".to_owned()),
                ("paste", "LOGO+SHIFT+v".to_owned()),
                ("tts-read", "CTRL+ALT+space".to_owned()),
            ]
        );
        config.tts.read_shortcut = String::new();
        config.tts.pause_shortcut = "Ctrl+Alt+P".into();
        config.tts.stop_shortcut = "Ctrl+Alt+S".into();
        assert_eq!(
            ids(portal_bindings(&config).unwrap()),
            vec![
                ("toggle", "CTRL+space".to_owned()),
                ("paste", "LOGO+SHIFT+v".to_owned()),
                ("tts-pause", "CTRL+ALT+p".to_owned()),
                ("tts-stop", "CTRL+ALT+s".to_owned()),
            ]
        );
    }
    #[test]
    fn invalid_or_colliding_reading_shortcut_names_the_offending_action() {
        let mut config = AppConfig {
            toggle_shortcut: "Ctrl+Space".into(),
            paste_shortcut: "Super+Shift+V".into(),
            ..Default::default()
        };
        config.tts.read_shortcut = "Ctrl+F5".into();
        let error = portal_bindings(&config).unwrap_err().to_string();
        assert!(error.starts_with("Leer selección en voz alta:"), "{error}");
        config.tts.read_shortcut = "Control+SPACE".into();
        let error = portal_bindings(&config).unwrap_err().to_string();
        assert!(error.contains("misma combinación"), "{error}");
        assert!(error.contains("Iniciar / detener dictado"), "{error}");
    }
    #[test]
    fn converts_portal_modifiers() {
        assert_eq!(trigger("Super+Shift+D").unwrap(), "LOGO+SHIFT+d");
        assert_eq!(trigger("Ctrl+Space").unwrap(), "CTRL+space");
        assert_eq!(trigger(" Control + SPACE ").unwrap(), "CTRL+space");
        assert!(trigger("Ctrl+Space+D").is_err());
        assert!(trigger("Space+Ctrl").is_err());
        assert!(trigger("Super+Shift").is_err());
        assert!(trigger("D").is_err());
    }
}
