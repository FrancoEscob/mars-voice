//! Shortcut strings from the config ("Ctrl+Space", "Ctrl+Alt+V") as key
//! combinations for native global-hotkey backends (Windows today), and what
//! each one sends to the controller. Platform-neutral, so it is tested on
//! every system; the Linux portal keeps its own trigger format.
use dicta_core::{AppConfig, Command};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The Windows key (Super/Logo/Meta on other systems).
    pub super_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// `A`–`Z`, stored uppercase.
    Letter(char),
    /// `0`–`9`.
    Digit(u8),
    Space,
    /// `F1`–`F24`.
    Function(u8),
}

impl Key {
    /// The W3C `KeyboardEvent.code` name (`KeyA`, `Digit1`, `Space`, `F5`),
    /// which `global-hotkey` parses.
    pub fn code_name(self) -> String {
        match self {
            Self::Letter(letter) => format!("Key{letter}"),
            Self::Digit(digit) => format!("Digit{digit}"),
            Self::Space => "Space".into(),
            Self::Function(number) => format!("F{number}"),
        }
    }
}

/// One key with its modifiers. At least one modifier is always present, so a
/// global shortcut never swallows plain typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Combo {
    pub modifiers: Modifiers,
    pub key: Key,
}

/// Parses `Mod+Mod+Key`, case-insensitive, spaces allowed around `+`.
/// Modifiers: Ctrl/Control, Alt, Shift, Super/Win/Logo/Meta. Keys: a letter,
/// a digit, Space or F1–F24. Errors are Spanish and name what to change.
pub fn parse(shortcut: &str) -> Result<Combo, String> {
    let mut parts: Vec<&str> = shortcut.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    if parts.is_empty() || key.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err("El atajo requiere modificador y tecla, por ejemplo Ctrl+Space".into());
    }
    let mut modifiers = Modifiers::default();
    for part in parts {
        let slot = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => &mut modifiers.ctrl,
            "alt" => &mut modifiers.alt,
            "shift" => &mut modifiers.shift,
            "super" | "win" | "windows" | "logo" | "meta" => &mut modifiers.super_key,
            _ => {
                return Err(format!(
                    "«{part}» no es un modificador; usá Ctrl, Alt, Shift o Win"
                ));
            }
        };
        if *slot {
            return Err("El modificador está repetido".into());
        }
        *slot = true;
    }
    Ok(Combo {
        modifiers,
        key: parse_key(key)?,
    })
}

fn parse_key(key: &str) -> Result<Key, String> {
    let unsupported = || "Atajo no compatible: usá una letra, un número, Space o F1–F24".to_owned();
    if key.eq_ignore_ascii_case("space") || key.eq_ignore_ascii_case("espacio") {
        return Ok(Key::Space);
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' | 'A'..='Z' => Ok(Key::Letter(c.to_ascii_uppercase())),
            '0'..='9' => Ok(Key::Digit(c as u8 - b'0')),
            _ => Err(unsupported()),
        };
    }
    if let Some(digits) = key.strip_prefix(['F', 'f'])
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
        && let Ok(number @ 1..=24) = digits.parse::<u8>()
    {
        return Ok(Key::Function(number));
    }
    Err(unsupported())
}

/// What a global shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Toggle,
    Paste,
    Read,
    Pause,
    Stop,
    /// Escritura: arreglar o traducir la selección.
    Write,
}

impl Action {
    /// Same names as the portal shortcuts and the settings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Toggle => "Iniciar / detener dictado",
            Self::Paste => "Pegar último dictado",
            Self::Read => "Leer selección en voz alta",
            Self::Pause => "Pausar / reanudar lectura",
            Self::Stop => "Detener lectura",
            Self::Write => "Arreglar o traducir la selección",
        }
    }

    /// The command for a key edge. Only dictation uses the release, so hold
    /// to talk and toggle both go through the controller's shortcut state.
    /// `at` is [`super::event_stamp`] taken when the edge arrived.
    pub fn command(self, pressed: bool, at: u64) -> Option<Command> {
        match (self, pressed) {
            (Self::Toggle, true) => Some(Command::ShortcutPressed { at, token: None }),
            (Self::Toggle, false) => Some(Command::ShortcutReleased { at, token: None }),
            (_, false) => None,
            (Self::Paste, true) => Some(Command::Paste),
            (Self::Read, true) => Some(Command::ReadSelection),
            (Self::Pause, true) => Some(Command::TtsToggle),
            (Self::Stop, true) => Some(Command::TtsStop),
            (Self::Write, true) => Some(Command::WriterSelection),
        }
    }
}

/// A shortcut to register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    /// As written in the config, for notices.
    pub shortcut: String,
    pub combo: Combo,
}

/// The configured shortcuts that can be registered, plus one Spanish notice
/// per shortcut that cannot (invalid, or the same combination as an earlier
/// action). Empty shortcuts are simply not registered.
pub fn bindings(config: &AppConfig) -> (Vec<Binding>, Vec<String>) {
    let configured = [
        (Action::Toggle, &config.toggle_shortcut),
        (Action::Paste, &config.paste_shortcut),
        (Action::Read, &config.tts.read_shortcut),
        (Action::Pause, &config.tts.pause_shortcut),
        (Action::Stop, &config.tts.stop_shortcut),
        (Action::Write, &config.writer.shortcut),
    ];
    let mut bindings: Vec<Binding> = Vec::new();
    let mut notices = Vec::new();
    for (action, shortcut) in configured {
        let shortcut = shortcut.trim();
        if shortcut.is_empty() {
            continue;
        }
        match parse(shortcut) {
            Err(error) => notices.push(format!(
                "«{shortcut}» ({}) no se registró: {error}. Cambialo en Ajustes",
                action.label()
            )),
            Ok(combo) => {
                if let Some(other) = bindings.iter().find(|binding| binding.combo == combo) {
                    notices.push(format!(
                        "«{}» y «{}» usan la misma combinación; elegí otra en Ajustes",
                        action.label(),
                        other.action.label()
                    ));
                    continue;
                }
                bindings.push(Binding {
                    action,
                    shortcut: shortcut.to_owned(),
                    combo,
                });
            }
        }
    }
    (bindings, notices)
}

/// Notice for a combination the system refused (taken by another app, or
/// reserved by Windows).
pub fn taken_notice(binding: &Binding) -> String {
    format!(
        "No se pudo registrar «{}» ({}): otra aplicación o Windows ya usa esa combinación. Elegí otra en Ajustes",
        binding.shortcut,
        binding.action.label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(ctrl: bool, alt: bool, shift: bool, super_key: bool, key: Key) -> Combo {
        Combo {
            modifiers: Modifiers {
                ctrl,
                alt,
                shift,
                super_key,
            },
            key,
        }
    }

    #[test]
    fn parses_the_default_and_common_shortcuts() {
        assert_eq!(
            parse("Ctrl+Space").unwrap(),
            combo(true, false, false, false, Key::Space)
        );
        assert_eq!(
            parse(" control + alt + v ").unwrap(),
            combo(true, true, false, false, Key::Letter('V'))
        );
        assert_eq!(
            parse("Super+Shift+D").unwrap(),
            combo(false, false, true, true, Key::Letter('D'))
        );
        assert_eq!(
            parse("Win+Alt+7").unwrap(),
            combo(false, true, false, true, Key::Digit(7))
        );
        assert_eq!(
            parse("Ctrl+Alt+Space").unwrap(),
            combo(true, true, false, false, Key::Space)
        );
        assert_eq!(parse("Shift+F12").unwrap().key, Key::Function(12));
        assert_eq!(parse("ctrl+f1").unwrap().key, Key::Function(1));
        assert_eq!(parse("Alt+F24").unwrap().key, Key::Function(24));
    }

    #[test]
    fn keys_map_to_w3c_code_names() {
        let names: Vec<String> = ["Ctrl+v", "Ctrl+0", "Ctrl+Space", "Ctrl+F9"]
            .into_iter()
            .map(|shortcut| parse(shortcut).unwrap().key.code_name())
            .collect();
        assert_eq!(names, ["KeyV", "Digit0", "Space", "F9"]);
    }

    #[test]
    fn rejects_what_cannot_be_registered_safely() {
        for (shortcut, expected) in [
            ("D", "requiere modificador"),
            ("Space", "requiere modificador"),
            ("Ctrl+", "requiere modificador"),
            ("", "requiere modificador"),
            ("Ctrl++V", "requiere modificador"),
            ("Ctrl+Shift", "no compatible"),
            ("Space+Ctrl", "no es un modificador"),
            ("Hyper+V", "no es un modificador"),
            ("Ctrl+Control+V", "repetido"),
            ("Ctrl+Enter", "no compatible"),
            ("Ctrl+F0", "no compatible"),
            ("Ctrl+F25", "no compatible"),
            ("Ctrl+F05", "no compatible"),
            ("Ctrl+Ñ", "no compatible"),
            ("Ctrl+VV", "no compatible"),
        ] {
            let error = parse(shortcut).unwrap_err();
            assert!(error.contains(expected), "{shortcut}: {error}");
        }
    }

    #[test]
    fn only_dictation_uses_the_release_edge() {
        assert!(matches!(
            Action::Toggle.command(true, 7),
            Some(Command::ShortcutPressed { at: 7, token: None })
        ));
        assert!(matches!(
            Action::Toggle.command(false, 9),
            Some(Command::ShortcutReleased { at: 9, token: None })
        ));
        assert!(matches!(
            Action::Paste.command(true, 1),
            Some(Command::Paste)
        ));
        assert!(matches!(
            Action::Read.command(true, 1),
            Some(Command::ReadSelection)
        ));
        assert!(matches!(
            Action::Pause.command(true, 1),
            Some(Command::TtsToggle)
        ));
        assert!(matches!(
            Action::Stop.command(true, 1),
            Some(Command::TtsStop)
        ));
        assert!(matches!(
            Action::Write.command(true, 1),
            Some(Command::WriterSelection)
        ));
        for action in [
            Action::Paste,
            Action::Read,
            Action::Pause,
            Action::Stop,
            Action::Write,
        ] {
            assert!(action.command(false, 2).is_none());
        }
    }

    #[test]
    fn bindings_skip_empty_and_report_invalid_or_duplicated_shortcuts() {
        let mut config = AppConfig {
            toggle_shortcut: "Ctrl+Space".into(),
            paste_shortcut: "Ctrl+Alt+V".into(),
            ..Default::default()
        };
        config.tts.read_shortcut = "Ctrl+Alt+Space".into();
        config.tts.pause_shortcut = String::new();
        config.tts.stop_shortcut = "  ".into();
        config.writer.shortcut = String::new();
        let (bindings, notices) = bindings(&config);
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(
            bindings
                .iter()
                .map(|b| (b.action, b.shortcut.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Action::Toggle, "Ctrl+Space"),
                (Action::Paste, "Ctrl+Alt+V"),
                (Action::Read, "Ctrl+Alt+Space"),
            ]
        );

        config.tts.read_shortcut = "control+SPACE".into();
        config.tts.pause_shortcut = "Ctrl+Alt+Enter".into();
        config.tts.stop_shortcut = "Ctrl+Alt+S".into();
        let (bindings, notices) = super::bindings(&config);
        assert_eq!(
            bindings.iter().map(|b| b.action).collect::<Vec<_>>(),
            vec![Action::Toggle, Action::Paste, Action::Stop]
        );
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(notices[0].contains("misma combinación"), "{}", notices[0]);
        assert!(notices[0].contains("Leer selección en voz alta"));
        assert!(notices[0].contains("Iniciar / detener dictado"));
        assert!(notices[1].contains("«Ctrl+Alt+Enter»"), "{}", notices[1]);
        assert!(notices[1].contains("Pausar / reanudar lectura"));
    }

    #[test]
    fn writing_uses_its_own_shortcut_and_reports_collisions_by_name() {
        let mut config = AppConfig {
            toggle_shortcut: "Ctrl+Space".into(),
            paste_shortcut: "Ctrl+Alt+V".into(),
            ..Default::default()
        };
        config.tts.read_shortcut = "Ctrl+Alt+Space".into();
        let (found, notices) = bindings(&config);
        assert!(notices.is_empty(), "{notices:?}");
        let write = found.iter().find(|b| b.action == Action::Write).unwrap();
        assert_eq!(write.shortcut, "Ctrl+Alt+E");
        assert_eq!(
            write.combo,
            combo(true, true, false, false, Key::Letter('E'))
        );
        config.writer.shortcut = "ctrl+alt+v".into();
        let (found, notices) = bindings(&config);
        assert!(found.iter().all(|b| b.action != Action::Write));
        assert!(
            notices[0].contains("Arreglar o traducir la selección"),
            "{notices:?}"
        );
    }

    #[test]
    fn taken_notice_names_the_shortcut_and_action() {
        let (bindings, _) = bindings(&AppConfig {
            toggle_shortcut: "Ctrl+Space".into(),
            ..Default::default()
        });
        let notice = taken_notice(&bindings[0]);
        assert!(notice.contains("«Ctrl+Space»"), "{notice}");
        assert!(notice.contains("Iniciar / detener dictado"), "{notice}");
    }
}
