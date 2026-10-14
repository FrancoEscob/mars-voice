//! Systems without a desktop backend yet (macOS until phase 3).
//! Every action fails with an actionable Spanish error before touching
//! anything; placement is a no-op, so windows stay where the system put them.
//! Also built for Linux tests, so this code is checked on every platform.
use super::{Backend, Clipboard, FocusTracker, Injector, SelectionReader, WindowPlacer};
use crate::desktop::{FloatingWindow, PastePlan, PasteTarget};
use crate::selection::{CopiedSelection, CopyOptions, SelectionSource};
use anyhow::{Result, bail};

pub(super) struct Unsupported;

pub(super) const BACKEND: Backend = Backend {
    name: "unsupported",
    clipboard: &Unsupported,
    focus: &Unsupported,
    injector: &Unsupported,
    selection: &Unsupported,
    placement: &Unsupported,
};

const PASTE: &str =
    "Pegar en otra app todavía no está disponible en este sistema; pegá el texto manualmente";

impl Clipboard for Unsupported {
    fn copy(&self, _text: &str) -> Result<()> {
        bail!("Copiar al portapapeles todavía no está disponible en este sistema")
    }
}

impl FocusTracker for Unsupported {
    fn active_target(&self) -> Result<PasteTarget> {
        bail!("Detectar la ventana activa todavía no está disponible en este sistema")
    }
}

impl Injector for Unsupported {
    fn paste(&self, _text: &str, _expected: Option<&PasteTarget>, _keys: &str) -> Result<()> {
        bail!(PASTE)
    }
    fn prepare_paste(&self, _remembered: Option<&PasteTarget>) -> Result<PastePlan> {
        bail!(PASTE)
    }
    fn paste_prepared(
        &self,
        _text: &str,
        _plan: &PastePlan,
        _keys: &str,
        _is_current: &dyn Fn() -> bool,
    ) -> Result<()> {
        bail!(PASTE)
    }
    fn paste_into(
        &self,
        _text: &str,
        _target: &PasteTarget,
        _keys: &str,
        _extra_terminal_classes: &[String],
        _is_current: &dyn Fn() -> bool,
    ) -> Result<()> {
        bail!(PASTE)
    }
}

impl SelectionReader for Unsupported {
    fn read_text(&self, source: SelectionSource, _max_bytes: usize) -> Result<Option<String>> {
        match source {
            SelectionSource::Primary => {
                bail!("Leer la selección todavía no está disponible en este sistema")
            }
            SelectionSource::Clipboard => {
                bail!("Leer el portapapeles todavía no está disponible en este sistema")
            }
        }
    }
    fn copy_selection(&self, _options: CopyOptions<'_>) -> Result<Option<String>> {
        bail!(
            "Copiar la selección todavía no está disponible en este sistema; pegá el texto en la ventana de lectura"
        )
    }
    fn copy_selection_from(&self, _options: CopyOptions<'_>) -> Result<CopiedSelection> {
        bail!(
            "Copiar la selección todavía no está disponible en este sistema; pegá el texto en la ventana de Escritura"
        )
    }
}

impl WindowPlacer for Unsupported {
    fn set_placement_enabled(&self, _window: FloatingWindow, _enabled: bool) {}
    fn place(&self, _window: FloatingWindow, _x: f32, _y: f32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_fails_with_an_actionable_spanish_error() {
        let backend = &BACKEND;
        let target = PasteTarget {
            address: "0x1".into(),
            pid: 1,
            class: "editor".into(),
            initial_class: "editor".into(),
            stable_id: Some("a".into()),
        };
        let options = CopyOptions {
            max_bytes: 64,
            restore_clipboard: true,
            extra_terminal_classes: &[],
        };
        let errors = [
            backend.clipboard.copy("contenido-privado").unwrap_err(),
            backend.focus.active_target().unwrap_err(),
            backend
                .injector
                .paste("contenido-privado", Some(&target), "CTRL+V")
                .unwrap_err(),
            backend.injector.prepare_paste(Some(&target)).unwrap_err(),
            backend
                .selection
                .read_text(SelectionSource::Primary, 64)
                .unwrap_err(),
            backend
                .selection
                .read_text(SelectionSource::Clipboard, 64)
                .unwrap_err(),
            backend
                .injector
                .paste_into("contenido-privado", &target, "CTRL+V", &[], &|| true)
                .unwrap_err(),
            backend
                .selection
                .copy_selection_from(CopyOptions {
                    max_bytes: 64,
                    restore_clipboard: true,
                    extra_terminal_classes: &[],
                })
                .unwrap_err(),
            backend.selection.copy_selection(options).unwrap_err(),
        ];
        for error in errors {
            let error = error.to_string();
            assert!(
                error.contains("todavía no está disponible en este sistema"),
                "{error}"
            );
            assert!(!error.contains("contenido-privado"), "{error}");
        }
        // Placement is silently skipped: windows stay where the system put them.
        backend
            .placement
            .set_placement_enabled(FloatingWindow::Overlay, true);
        backend.placement.place(FloatingWindow::Reader, 10., 20.);
        assert_eq!(backend.name, "unsupported");
    }
}
