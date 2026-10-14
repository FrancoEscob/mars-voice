//! System tray. On Linux, a StatusNotifierItem (ksni) that needs a host such
//! as waybar, KDE Plasma or GNOME's AppIndicator extension. Uses the
//! desktop's microphone icon until an approved brand icon is supplied by the
//! design owner. On Windows, a notification-area icon (`tray-icon`) on the
//! shell thread (`shell_windows.rs`) with the app icon embedded in the
//! executable: left click opens the settings, right click shows the menu.
//! macOS has no tray yet: [`spawn`] returns an error and the application
//! keeps running with IPC/desktop launcher.
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
                label: "Arreglar o traducir el portapapeles".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.commands.send(Command::WriterClipboard);
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

/// Keeps the tray icon; dropping it removes the icon.
#[cfg(windows)]
pub use crate::shell_windows::TrayHandle;

/// Shows the notification-area icon. Fails when the shell thread cannot start
/// or Windows refuses the icon; the application keeps running without it.
#[cfg(windows)]
pub async fn spawn(commands: Sender<Command>) -> anyhow::Result<TrayHandle> {
    crate::shell_windows::spawn_tray(commands)
}

/// Windows tray menu: item id and label. "Salir" comes after a separator.
#[cfg(any(windows, test))]
pub(crate) const MENU: &[(&str, &str)] = &[
    ("dicta-settings", "Abrir ajustes"),
    ("dicta-toggle", "Dictar / detener"),
    ("dicta-read", "Leer selección"),
    ("dicta-write", "Arreglar o traducir el portapapeles"),
    (QUIT_ID, "Salir"),
];
#[cfg(any(windows, test))]
pub(crate) const QUIT_ID: &str = "dicta-quit";

/// The command for a Windows tray menu item.
#[cfg(any(windows, test))]
pub(crate) fn menu_command(id: &str) -> Option<Command> {
    match id {
        "dicta-settings" => Some(Command::ShowSettings),
        "dicta-toggle" => Some(Command::Toggle),
        "dicta-read" => Some(Command::ReadSelection),
        // From the tray the active window is the taskbar, not the text: the
        // clipboard is the only meaningful source here.
        "dicta-write" => Some(Command::WriterClipboard),
        QUIT_ID => Some(Command::Quit),
        _ => None,
    }
}

/// A plain Mars-orange disc with a soft edge, used only if the executable's
/// icon resource cannot be loaded. Returns RGBA pixels and the side length.
#[cfg(any(windows, test))]
pub(crate) fn fallback_icon() -> (Vec<u8>, u32) {
    const SIDE: u32 = 32;
    let centre = (SIDE as f32 - 1.0) / 2.0;
    let radius = SIDE as f32 / 2.0 - 1.0;
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let distance = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            let alpha = (radius + 0.5 - distance).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[0xE0, 0x5A, 0x2B, (alpha * 255.0).round() as u8]);
        }
    }
    (rgba, SIDE)
}

#[cfg(test)]
mod windows_menu_tests {
    use super::*;

    #[test]
    fn every_windows_menu_item_sends_a_command_and_quit_is_last() {
        let commands: Vec<_> = MENU.iter().map(|(id, _)| menu_command(id)).collect();
        assert!(matches!(
            commands.as_slice(),
            [
                Some(Command::ShowSettings),
                Some(Command::Toggle),
                Some(Command::ReadSelection),
                Some(Command::WriterClipboard),
                Some(Command::Quit),
            ]
        ));
        assert_eq!(MENU.last().map(|(id, _)| *id), Some(QUIT_ID));
        assert!(menu_command("otro").is_none());
    }

    #[test]
    fn fallback_icon_is_an_opaque_disc_with_transparent_corners() {
        let (rgba, side) = fallback_icon();
        assert_eq!(rgba.len(), (side * side * 4) as usize);
        let alpha = |x: u32, y: u32| rgba[((y * side + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(side - 1, side - 1), 0);
        assert_eq!(alpha(side / 2, side / 2), 255);
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub use unsupported::{TrayHandle, spawn};

/// No tray backend on this system yet. Also built for Linux tests.
#[cfg(any(not(any(target_os = "linux", windows)), test))]
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
