//! Shared app.toml schema and defaults.
pub use notetaker_asr_proto::Provider;
pub fn default_speech_model(language: &str) -> &'static str {
    if language.to_ascii_lowercase().starts_with("ru") {
        "gigaam-v3"
    } else {
        "parakeet-v3"
    }
}

pub fn system_language() -> &'static str {
    static LANGUAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LANGUAGE.get_or_init(read_system_language)
}

fn read_system_language() -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key)
            && !value.is_empty()
            && value != "C"
            && value != "C.UTF-8"
        {
            return value;
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
    {
        return String::from_utf8_lossy(&output.stdout).trim().into();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Ok(output) = std::process::Command::new("powershell.exe")
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .args([
                "-NoProfile",
                "-Command",
                "[System.Globalization.CultureInfo]::CurrentUICulture.Name",
            ])
            .output()
        {
            return String::from_utf8_lossy(&output.stdout).trim().into();
        }
    }
    // The app's UI is Russian, unless the OS explicitly reports another language.
    "ru".into()
}

/// Keep the persisted schema and defaults in one place while allowing the GUI
/// to use its existing Theme, DocumentAction and calendar types without a core
/// dependency on egui or the LLM layer. Non-GUI readers keep those values opaque.
#[macro_export]
macro_rules! app_settings_schema {
    ($name:ident, $theme:ty, $theme_default:expr, $calendar:ty, $action:ty, $pinned:expr) => {
        #[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            pub user_name: String,
            pub digest_reminder: bool,
            /// Aliases separated by commas, semicolons or newlines.
            pub user_aliases: String,
            pub theme: $theme,
            pub ui_zoom: f32,
            /// None identifies a legacy config, whose user has already set up the app.
            pub onboarding_complete: Option<bool>,
            pub models_folder: Option<std::path::PathBuf>,
            pub library: std::path::PathBuf,
            pub model: String,
            pub asr_acceleration: $crate::app_config::Provider,
            /// Zero means min(physical cores, 8).
            pub asr_threads: u32,
            pub mode: $crate::Mode,
            /// None means forever, zero means after the successful final pass.
            #[serde(default)]
            pub retention_days: Option<u32>,
            pub hotkey: String,
            pub recap_hotkey: String,
            pub bookmark_hotkey: String,
            pub live_summary: bool,
            pub live_summary_minutes: u32,
            pub recap_minutes: u32,
            pub detect_calls: bool,
            pub detect_browsers: bool,
            pub detect_any_microphone: bool,
            /// Executable names, one per line.
            pub call_apps: String,
            /// Zero disables the silence timeout.
            pub auto_stop_minutes: u32,
            pub auto_summary: bool,
            pub vocabulary: String,
            pub diarize: bool,
            pub speakers: Option<u32>,
            pub folder: Option<String>,
            pub folder_mode_override: Option<$crate::Mode>,
            pub voice_library: bool,
            /// Only credential IDs and display names, never subscription URLs.
            pub calendars: Vec<$calendar>,
            pub calendar_timezone: String,
            pub mcp_exclude_local: bool,
            pub check_updates: bool,
            /// Meeting actions shown as buttons in the header, in menu order.
            pub pinned_actions: Vec<$action>,
            // Retired preferences remain readable but are no longer written or used.
            #[serde(skip_serializing, rename = "hide_quickstart")]
            pub _hide_quickstart: bool,
            #[serde(skip_serializing, rename = "auto_download_models")]
            pub _auto_download_models: bool,
            #[serde(skip_serializing, rename = "auto_diarization_attempted")]
            pub _auto_diarization_attempted: bool,
        }

        impl Default for $name {
            fn default() -> Self {
                let home = directories::UserDirs::new();
                let documents = home
                    .as_ref()
                    .map(|dirs| dirs.document_dir().unwrap_or(dirs.home_dir()).to_owned())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                Self {
                    user_name: String::new(),
                    digest_reminder: false,
                    user_aliases: String::new(),
                    theme: $theme_default,
                    ui_zoom: 1.0,
                    onboarding_complete: None,
                    models_folder: None,
                    library: documents.join("notetaker"),
                    model: $crate::app_config::default_speech_model(
                        $crate::app_config::system_language(),
                    )
                    .into(),
                    asr_acceleration: $crate::app_config::Provider::Auto,
                    asr_threads: 0,
                    mode: $crate::Mode::Local,
                    retention_days: Some(7),
                    hotkey: if cfg!(target_os = "macos") {
                        "Super+Alt+Shift+R"
                    } else {
                        "Ctrl+Alt+R"
                    }
                    .into(),
                    recap_hotkey: if cfg!(target_os = "macos") {
                        "Super+Alt+Shift+M"
                    } else {
                        "Ctrl+Alt+M"
                    }
                    .into(),
                    bookmark_hotkey: if cfg!(target_os = "macos") {
                        "Super+Alt+Shift+B"
                    } else {
                        "Ctrl+Alt+B"
                    }
                    .into(),
                    live_summary: true,
                    live_summary_minutes: 3,
                    recap_minutes: 5,
                    detect_calls: true,
                    detect_browsers: true,
                    detect_any_microphone: true,
                    call_apps: String::new(),
                    auto_stop_minutes: 15,
                    auto_summary: true,
                    vocabulary: String::new(),
                    diarize: true,
                    speakers: None,
                    folder: None,
                    folder_mode_override: None,
                    voice_library: false,
                    calendars: Vec::new(),
                    calendar_timezone: "Europe/Moscow".into(),
                    mcp_exclude_local: true,
                    check_updates: true,
                    pinned_actions: vec![$pinned],
                    _hide_quickstart: false,
                    _auto_download_models: true,
                    _auto_diarization_attempted: false,
                }
            }
        }

        impl $name {
            pub fn models_path(&self) -> std::path::PathBuf {
                self.models_folder.clone().unwrap_or_else(|| {
                    std::env::var_os("NOTETAKER_MODELS_DIR")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| {
                            directories::ProjectDirs::from("", "", "notetaker")
                                .expect("app data directory")
                                .data_local_dir()
                                .join("models")
                        })
                })
            }
        }
    };
}
app_settings_schema!(
    AppSettings,
    toml::Value,
    toml::Value::String("light".into()),
    toml::Value,
    toml::Value,
    toml::Value::String("copy_all".into())
);
impl AppSettings {
    pub fn load() -> std::io::Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "notetaker")
            .ok_or_else(|| std::io::Error::other("Нет каталога настроек"))?;
        match std::fs::read_to_string(dirs.config_dir().join("app.toml")) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_0231_config_keeps_paths_and_preferences() {
        let text = include_str!("../tests/fixtures/app-0.23.1.toml");
        let settings: AppSettings = toml::from_str(text).unwrap();
        assert_eq!(
            settings.models_path(),
            std::path::Path::new("/custom/models")
        );
        assert_eq!(settings.model, "parakeet-v3");
        assert_eq!(settings.asr_threads, 4);
        assert_eq!(settings.calendars.len(), 1);
        let saved = toml::to_string(&settings).unwrap();
        let roundtrip: AppSettings = toml::from_str(&saved).unwrap();
        assert_eq!(roundtrip.models_path(), settings.models_path());
        assert!(!saved.contains("auto_download_models"));
    }
    #[test]
    fn legacy_omissions_keep_defaults_and_forever_retention() {
        let settings: AppSettings = toml::from_str("model = 'gigaam-v3'").unwrap();
        assert!(settings.diarize);
        assert!(settings.auto_summary);
        assert_eq!(settings.retention_days, None);
        assert_eq!(AppSettings::default().retention_days, Some(7));
    }
}
