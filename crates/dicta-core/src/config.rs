use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Signal,
    Regolith,
    Night,
    Orbit,
}
impl Theme {
    pub const ALL: [Self; 3] = [Self::Signal, Self::Regolith, Self::Orbit];
    pub fn label(self) -> &'static str {
        match self {
            Self::Signal => "Señal",
            Self::Regolith => "Regolito",
            Self::Night => "Noche",
            Self::Orbit => "Órbita",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    #[default]
    Light,
    Dark,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioRetention {
    #[default]
    Off,
    Days7,
    Days30,
    Forever,
}
impl AudioRetention {
    pub fn enabled(self) -> bool {
        self != Self::Off
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "No guardar audio",
            Self::Days7 => "Guardar 7 días",
            Self::Days30 => "Guardar 30 días",
            Self::Forever => "Hasta que lo borre",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMode {
    #[default]
    Subscription,
    ApiKey,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutMode {
    #[default]
    Toggle,
    Hold,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub theme: Theme,
    pub color_mode: ColorMode,
    pub history_enabled: bool,
    pub audio_retention: AudioRetention,
    pub provider: ProviderMode,
    pub language: String,
    pub copy_on_finish: bool,
    pub microphone: Option<String>,
    pub keyterms: Vec<String>,
    pub format_numbers: bool,
    pub filler_words: bool,
    pub endpointing_ms: u16,
    pub polish: bool,
    pub polish_model: String,
    pub instructions: String,
    pub oauth_client_id: String,
    pub toggle_shortcut: String,
    pub shortcut_mode: ShortcutMode,
    pub paste_shortcut: String,
    pub paste_keys: String,
    pub overlay_x: f32,
    pub overlay_y: f32,
    pub overlay_centered: bool,
    pub show_idle_overlay: bool,
    /// Idioma de la interfaz: `auto` sigue al sistema, o `es` / `en`.
    pub ui_language: String,
    /// El usuario aceptó el aviso de la suscripción OAuth experimental y no
    /// oficial. La UI lo pide antes de elegir Suscripción por primera vez.
    pub subscription_acknowledged: bool,
    /// Lectura en voz alta. Una sección propia para que su modo de cuenta y sus
    /// preferencias no cambien el dictado.
    pub tts: crate::tts::TtsConfig,
    /// Escritura: arreglar y traducir la selección.
    pub writer: crate::writer::WriterConfig,
}
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: Theme::Signal,
            color_mode: ColorMode::Light,
            history_enabled: true,
            audio_retention: AudioRetention::Off,
            provider: ProviderMode::ApiKey,
            language: "es".into(),
            copy_on_finish: true,
            microphone: None,
            keyterms: vec![],
            format_numbers: false,
            filler_words: false,
            endpointing_ms: 400,
            polish: false,
            polish_model: "grok-4.3".into(),
            instructions: INSTRUCTIONS_ES.into(),
            oauth_client_id: String::new(),
            toggle_shortcut: "Ctrl+Space".into(),
            shortcut_mode: ShortcutMode::Toggle,
            // Windows reserva varias combinaciones con la tecla Windows.
            paste_shortcut: if cfg!(windows) {
                "Ctrl+Alt+V"
            } else {
                "Super+Shift+V"
            }
            .into(),
            paste_keys: "CTRL+V".into(),
            overlay_x: 24.0,
            overlay_y: 24.0,
            overlay_centered: true,
            show_idle_overlay: true,
            ui_language: "auto".into(),
            subscription_acknowledged: false,
            tts: Default::default(),
            writer: Default::default(),
        }
    }
}
/// Instrucciones editoriales predeterminadas del pulido, por idioma.
pub const INSTRUCTIONS_ES: &str = "No agregues información ni cambies cifras, negaciones o nombres propios. Conserva los términos técnicos.";
pub const INSTRUCTIONS_EN: &str =
    "Do not add information or change numbers, negations or proper names. Keep technical terms.";
pub const UI_LANGUAGES: [&str; 3] = ["auto", "es", "en"];

impl AppConfig {
    /// Configuración de primer arranque según el idioma del sistema (BCP-47,
    /// por ejemplo `es-AR` o `en_US.UTF-8`). La interfaz sigue al sistema.
    pub fn for_locale(locale: &str) -> Self {
        let primary: String = locale
            .split(['-', '_', '.', '@'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut config = Self::default();
        if primary.len() == 2 && primary.bytes().all(|b| b.is_ascii_lowercase()) {
            config.language = primary.clone();
        }
        if primary != "es" {
            config.instructions = INSTRUCTIONS_EN.into();
        }
        config
    }
    /// Lee un `config.toml` existente. Las configuraciones anteriores a la
    /// interfaz bilingüe no tienen `ui_language`: su interfaz era en español y
    /// así se conserva, sin depender del idioma del sistema.
    pub fn from_toml(text: &str) -> Result<Self> {
        let mut config: AppConfig = toml::from_str(text).context("config.toml no es válido")?;
        let has_ui_language = text
            .parse::<toml::Table>()
            .map(|table| table.contains_key("ui_language"))
            .unwrap_or(true);
        if !has_ui_language {
            config.ui_language = "es".into();
        }
        Ok(config)
    }
    /// Legacy Night was a style; it now means Signal with the dark palette.
    pub fn normalize(&mut self) -> bool {
        if self.theme == Theme::Night {
            self.theme = Theme::Signal;
            self.color_mode = ColorMode::Dark;
            return true;
        }
        false
    }
    pub fn saves_audio(&self) -> bool {
        self.history_enabled && self.audio_retention.enabled()
    }
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.endpointing_ms <= 5000,
            "El silencio de corte debe estar entre 0 y 5000 ms"
        );
        anyhow::ensure!(
            self.keyterms.len() <= 100
                && self
                    .keyterms
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.chars().count() <= 50),
            "Usá hasta 100 términos de 50 caracteres"
        );
        anyhow::ensure!(
            self.overlay_x.is_finite() && self.overlay_y.is_finite(),
            "Posición de overlay inválida"
        );
        anyhow::ensure!(
            self.instructions.len() <= 16000,
            "Las instrucciones son demasiado largas"
        );
        anyhow::ensure!(
            UI_LANGUAGES.contains(&self.ui_language.as_str()),
            "Idioma de interfaz inválido; usá auto, es o en"
        );
        self.tts.validate()?;
        self.writer.validate()?;
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub runtime: PathBuf,
}
/// Project identifiers for [`directories::ProjectDirs`]. Linux and other Unix
/// keep the original XDG names (`~/.config/dicta`, `~/.local/share/dicta`) so
/// no existing user loses configuration, history or accounts.
#[cfg(not(any(target_os = "macos", windows)))]
const PROJECT: (&str, &str, &str) = ("", "", "dicta");
#[cfg(any(target_os = "macos", windows))]
const PROJECT: (&str, &str, &str) = ("io", "dicta", "Dicta");
/// Runtime folder of `--demo`, beside the normal one so both can run at once.
#[cfg(not(any(target_os = "macos", windows)))]
const DEMO_RUNTIME: &str = "dicta-demo";
#[cfg(target_os = "macos")]
const DEMO_RUNTIME: &str = "io.dicta.Dicta.demo";
#[cfg(windows)]
const DEMO_RUNTIME: &str = "run-demo";

impl AppPaths {
    /// - Linux: `$XDG_CONFIG_HOME/dicta`, `$XDG_DATA_HOME/dicta` and
    ///   `$XDG_RUNTIME_DIR/dicta`.
    /// - macOS: `~/Library/Application Support/io.dicta.Dicta` and the per-user
    ///   temporary folder (`confstr(_CS_DARWIN_USER_TEMP_DIR)`) + `io.dicta.Dicta`.
    /// - Windows: `%LOCALAPPDATA%\dicta\Dicta\{config,data}` and
    ///   `%LOCALAPPDATA%\dicta\run`.
    pub fn discover() -> Result<Self> {
        let (qualifier, organization, application) = PROJECT;
        let d = directories::ProjectDirs::from(qualifier, organization, application)
            .context("No se encontraron las carpetas del usuario")?;
        // Windows: the local, not roaming, profile, next to the data.
        #[cfg(windows)]
        let config = d.config_local_dir().to_path_buf();
        #[cfg(not(windows))]
        let config = d.config_dir().to_path_buf();
        Ok(Self {
            config,
            data: d.data_local_dir().to_path_buf(),
            runtime: runtime_dir()?,
        })
    }
    /// The private runtime folder used by `--demo` instead of [`Self::runtime`].
    pub fn demo_runtime(&self) -> PathBuf {
        self.runtime.with_file_name(DEMO_RUNTIME)
    }
    pub fn load(&self) -> Result<AppConfig> {
        let path = self.config.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                let mut c = AppConfig::from_toml(&s)?;
                c.validate()?;
                if c.normalize() {
                    self.save(&c)?;
                }
                Ok(c)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::for_locale(
                &sys_locale::get_locale().unwrap_or_default(),
            )),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, c: &AppConfig) -> Result<()> {
        let mut c = c.clone();
        c.normalize();
        c.validate()?;
        crate::fsx::create_private_dir(&self.config)?;
        // Written to config.toml.tmp, synced and renamed over config.toml.
        crate::fsx::write_private_file(
            &self.config.join("config.toml"),
            toml::to_string_pretty(&c)?.as_bytes(),
        )?;
        Ok(())
    }
}
#[cfg(not(any(target_os = "macos", windows)))]
fn runtime_dir() -> Result<PathBuf> {
    xdg_runtime_dir(std::env::var_os("XDG_RUNTIME_DIR"))
}
#[cfg(not(any(target_os = "macos", windows)))]
fn xdg_runtime_dir(value: Option<std::ffi::OsString>) -> Result<PathBuf> {
    Ok(value
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR no está definido; iniciá Dicta dentro de tu sesión gráfica")?
        .join("dicta"))
}
/// The per-user temporary folder (`/var/folders/…/T/`), private to the user.
#[cfg(target_os = "macos")]
fn runtime_dir() -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let name = libc::_CS_DARWIN_USER_TEMP_DIR;
    // SAFETY: a null buffer of length 0 only asks for the required size.
    let size = unsafe { libc::confstr(name, std::ptr::null_mut(), 0) };
    anyhow::ensure!(
        size > 0,
        "No se encontró la carpeta temporal del usuario de macOS"
    );
    let mut buffer = vec![0u8; size];
    // SAFETY: `buffer` has exactly `size` writable bytes, as confstr asked.
    let written = unsafe { libc::confstr(name, buffer.as_mut_ptr().cast(), size) };
    let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(0);
    anyhow::ensure!(
        written > 0 && written <= size && end > 0,
        "No se encontró la carpeta temporal del usuario de macOS"
    );
    buffer.truncate(end);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)).join("io.dicta.Dicta"))
}
#[cfg(windows)]
fn runtime_dir() -> Result<PathBuf> {
    let base =
        directories::BaseDirs::new().context("No se encontraron las carpetas del usuario")?;
    Ok(base.data_local_dir().join("dicta").join("run"))
}
/// Prefer [`crate::fsx::create_private_dir`].
pub fn private_dir(path: &std::path::Path) -> Result<()> {
    Ok(crate::fsx::create_private_dir(path)?)
}
/// Prefer [`crate::fsx::write_private_file`]; this one is atomic too.
pub fn private_write(path: &std::path::Path, data: &[u8]) -> Result<()> {
    Ok(crate::fsx::write_private_file(path, data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_styles_roundtrip_without_changing_provider() {
        for theme in Theme::ALL {
            let c = AppConfig {
                theme,
                ..Default::default()
            };
            let loaded: AppConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
            assert_eq!(loaded, c);
            assert_eq!(loaded.provider, ProviderMode::ApiKey);
        }
    }
    #[test]
    fn old_configs_keep_toggle_and_hold_roundtrips() {
        assert_eq!(
            toml::from_str::<AppConfig>("").unwrap().shortcut_mode,
            ShortcutMode::Toggle
        );
        let hold: AppConfig =
            toml::from_str("shortcut_mode = 'hold'\ncopy_on_finish = false").unwrap();
        let restored: AppConfig = toml::from_str(&toml::to_string(&hold).unwrap()).unwrap();
        assert_eq!(restored.shortcut_mode, ShortcutMode::Hold);
        assert!(!restored.copy_on_finish);
        assert!(toml::from_str::<AppConfig>("shortcut_mode = 'unknown'").is_err());
    }
    #[test]
    fn invalid_keyterms_rejected() {
        let mut c = AppConfig::default();
        c.keyterms = vec!["x".repeat(51)];
        assert!(c.validate().is_err());
        c.keyterms = vec!["GPUI".into()];
        c.endpointing_ms = 5001;
        assert!(c.validate().is_err());
    }
    #[test]
    fn defaults_never_choose_billed_fallback() {
        // Las versiones públicas empiezan con API key propia; la suscripción es
        // experimental y explícita. Ninguna cambia sola a la otra.
        let c: AppConfig = toml::from_str("").unwrap();
        assert!(c.copy_on_finish);
        assert!(!c.polish);
        assert_eq!(c.provider, ProviderMode::ApiKey);
        assert_eq!(c.tts.provider, ProviderMode::ApiKey);
        assert!(c.oauth_client_id.is_empty());
        assert!(c.keyterms.is_empty());
        assert_eq!(c.ui_language, "auto");
    }
    #[test]
    fn configs_from_before_i18n_keep_the_spanish_interface() {
        let legacy = AppConfig::from_toml("theme = 'orbit'\n").unwrap();
        assert_eq!(legacy.ui_language, "es");
        let chosen = AppConfig::from_toml("ui_language = 'en'\n").unwrap();
        assert_eq!(chosen.ui_language, "en");
        let auto = AppConfig::from_toml("ui_language = 'auto'\n").unwrap();
        assert_eq!(auto.ui_language, "auto");
        assert!(AppConfig::from_toml("ui_language = 3").is_err());
    }
    #[test]
    fn first_run_follows_system_locale_and_existing_configs_keep_their_provider() {
        let es = AppConfig::for_locale("es-AR");
        assert_eq!(es.language, "es");
        assert_eq!(es.instructions, INSTRUCTIONS_ES);
        let en = AppConfig::for_locale("en_US.UTF-8");
        assert_eq!(en.language, "en");
        assert_eq!(en.instructions, INSTRUCTIONS_EN);
        assert_eq!(AppConfig::for_locale("pt-BR").language, "pt");
        assert_eq!(AppConfig::for_locale("").language, "es");
        let mine: AppConfig =
            toml::from_str("provider = 'subscription'\n[tts]\nprovider = 'subscription'\n")
                .unwrap();
        assert_eq!(mine.provider, ProviderMode::Subscription);
        assert_eq!(mine.tts.provider, ProviderMode::Subscription);
        let mut bad = AppConfig::default();
        bad.ui_language = "fr".into();
        assert!(bad.validate().is_err());
    }
    #[test]
    fn older_config_gets_centering_without_losing_preferences() {
        let c: AppConfig =
            toml::from_str("theme = 'night'\ntoggle_shortcut = 'Alt+D'\noverlay_x = -40.0\n")
                .unwrap();
        assert!(c.overlay_centered);
        assert_eq!(c.toggle_shortcut, "Alt+D");
        assert_eq!(c.theme, Theme::Night);
        assert_eq!(c.overlay_x, -40.0);
        assert_eq!(
            toml::from_str::<AppConfig>(&toml::to_string(&c).unwrap()).unwrap(),
            c
        );
        assert_eq!(
            toml::from_str::<AppConfig>("").unwrap().toggle_shortcut,
            "Ctrl+Space"
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    #[test]
    fn legacy_night_migrates_on_disk_without_losing_provider_or_shortcut() {
        let root = std::env::temp_dir().join(format!(
            "dicta-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            runtime: root.join("run"),
        };
        private_dir(&paths.config).unwrap();
        std::fs::write(
            paths.config.join("config.toml"),
            "theme = 'night'\nprovider = 'api_key'\ntoggle_shortcut = 'Alt+D'\n",
        )
        .unwrap();
        let c = paths.load().unwrap();
        assert_eq!(c.theme, Theme::Signal);
        assert_eq!(c.color_mode, ColorMode::Dark);
        assert_eq!(c.provider, ProviderMode::ApiKey);
        assert_eq!(c.toggle_shortcut, "Alt+D");
        assert!(c.history_enabled);
        assert!(!c.saves_audio());
        let reread: AppConfig =
            toml::from_str(&std::fs::read_to_string(paths.config.join("config.toml")).unwrap())
                .unwrap();
        assert_eq!(reread, c);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn all_three_styles_keep_both_color_modes_and_audio_requires_history() {
        assert_eq!(Theme::ALL.len(), 3);
        for theme in Theme::ALL {
            for color_mode in [ColorMode::Light, ColorMode::Dark] {
                let mut c = AppConfig {
                    theme,
                    color_mode,
                    audio_retention: AudioRetention::Days7,
                    ..Default::default()
                };
                assert!(!c.normalize());
                assert!(c.saves_audio());
                c.history_enabled = false;
                assert!(!c.saves_audio());
                let loaded: AppConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
                assert_eq!(loaded, c);
            }
        }
    }
}

#[cfg(test)]
mod paths_tests {
    use super::*;
    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn linux_runtime_stays_in_xdg_runtime_dir_and_demo_beside_it() {
        let runtime = xdg_runtime_dir(Some("/run/user/1000".into())).unwrap();
        assert_eq!(runtime, PathBuf::from("/run/user/1000/dicta"));
        let error = xdg_runtime_dir(None).unwrap_err().to_string();
        assert!(
            error.contains("XDG_RUNTIME_DIR no está definido"),
            "{error}"
        );
        let paths = AppPaths {
            config: "/c".into(),
            data: "/d".into(),
            runtime,
        };
        assert_eq!(
            paths.demo_runtime(),
            PathBuf::from("/run/user/1000/dicta-demo")
        );
        assert_eq!(PROJECT, ("", "", "dicta"));
    }
    #[test]
    fn discovered_paths_are_absolute_distinct_runtime_and_named_for_dicta() {
        // Linux CI without a graphical session has no XDG_RUNTIME_DIR.
        let Ok(paths) = AppPaths::discover() else {
            return;
        };
        for path in [&paths.config, &paths.data, &paths.runtime] {
            assert!(path.is_absolute(), "{}", path.display());
            assert!(
                path.to_string_lossy().to_lowercase().contains("dicta"),
                "{}",
                path.display()
            );
        }
        assert_ne!(paths.runtime, paths.config);
        assert_ne!(paths.demo_runtime(), paths.runtime);
        assert_eq!(paths.demo_runtime().parent(), paths.runtime.parent());
        #[cfg(target_os = "macos")]
        assert!(paths.runtime.ends_with("io.dicta.Dicta"));
        #[cfg(windows)]
        {
            assert!(paths.runtime.ends_with("dicta\\run"));
            assert!(paths.config.ends_with("dicta\\Dicta\\config"));
            assert!(paths.data.ends_with("dicta\\Dicta\\data"));
        }
    }
}
