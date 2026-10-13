//! System tray. On Linux, a StatusNotifierItem (ksni) that needs a host such
//! as waybar, KDE Plasma or GNOME's AppIndicator extension. Uses the
//! desktop's microphone icon until an approved brand icon is supplied by the
//! design owner. Other systems have no tray yet: [`spawn`] returns an error
//! and the application keeps running with IPC/desktop launcher.
use dicta_core::Command;
#[cfg(target_os = "linux")]
use ksni::TrayMethods;
use std::sync::mpsc::Sender;

#[cfg(target_os = "linux")]
pub struct DictaTray {
    commands: Sender<Command>,
}
/// Keeps the tray alive; dropping it removes the icon.
#[cfg(target_os = "linux")]
pub type TrayHandle = ksni::Handle<DictaTray>;
#[cfg(target_os = "linux")]
impl ksni::Tray for DictaTray {
    fn id(&self) -> String {
        crate::desktop::APP_ID.into()
    }
    fn title(&self) -> String {
        "Mars voice".into()
    }
    fn icon_name(&self) -> String {
        "io.github.francoescob.Dicta".into()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.commands.send(Command::ShowSettings);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Iniciar / detener dictado".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Mostrar overlay".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::ShowOverlay);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Abrir Mars voice".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::ShowSettings);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Salir".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
/// Keep the handle for the resident application's lifetime. Missing tray hosts
/// return an error; the application can keep running with IPC/desktop launcher.
#[cfg(target_os = "linux")]
pub async fn spawn(commands: Sender<Command>) -> anyhow::Result<ksni::Handle<DictaTray>> {
    Ok(DictaTray { commands }.spawn().await?)
}

#[cfg(not(target_os = "linux"))]
pub use unsupported::{TrayHandle, spawn};

/// No tray backend on this system yet. Also built for Linux tests.
#[cfg(any(not(target_os = "linux"), test))]
mod unsupported {
    use super::{Command, Sender};

    /// Placeholder: there is never a live tray to keep.
    pub struct TrayHandle(());

    pub async fn spawn(_commands: Sender<Command>) -> anyhow::Result<TrayHandle> {
        anyhow::bail!("La bandeja todavía no está disponible en este sistema")
    }

    #[cfg(test)]
    mod tests {
        #[tokio::test]
        async fn systems_without_tray_report_it_without_sending_commands() {
            let (tx, rx) = std::sync::mpsc::channel();
            let error = super::spawn(tx).await.err().unwrap().to_string();
            assert!(error.contains("todavía no está disponible"), "{error}");
            assert!(rx.try_recv().is_err());
        }
    }
}
