use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use directories::{ProjectDirs, UserDirs};
use notetaker_core::Mode;
use serde::{Deserialize, Serialize};

pub const MODELS: [&str; 2] = ["gigaam-v3", "parakeet-v3"];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub user_name: String,
    /// Aliases separated by commas, semicolons or newlines.
    pub user_aliases: String,
    pub theme: crate::theme::Theme,
    pub library: PathBuf,
    pub model: String,
    pub mode: Mode,
    /// None means forever, zero means after the successful final pass.
    #[serde(default)]
    pub retention_days: Option<u32>,
    pub hotkey: String,
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
    pub voice_library: bool,
    /// Only credential IDs and display names, never subscription URLs.
    pub calendars: Vec<notetaker_llm::calendar::Subscription>,
    pub calendar_timezone: String,
    pub mcp_exclude_local: bool,
    pub check_updates: bool,
    pub hide_quickstart: bool,
    pub auto_download_models: bool,
    pub auto_diarization_attempted: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let home = UserDirs::new();
        let documents = home
            .as_ref()
            .map(|dirs| dirs.document_dir().unwrap_or(dirs.home_dir()).to_owned())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            user_name: String::new(),
            user_aliases: String::new(),
            theme: crate::theme::Theme::System,
            library: documents.join("notetaker"),
            model: MODELS[0].into(),
            mode: Mode::Local,
            retention_days: Some(7),
            hotkey: "Ctrl+Alt+R".into(),
            detect_calls: true,
            detect_browsers: true,
            detect_any_microphone: true,
            call_apps: String::new(),
            auto_stop_minutes: 15,
            auto_summary: true,
            vocabulary: String::new(),
            diarize: true,
            speakers: None,
            voice_library: false,
            calendars: Vec::new(),
            calendar_timezone: "Europe/Moscow".into(),
            mcp_exclude_local: true,
            check_updates: true,
            hide_quickstart: false,
            auto_download_models: true,
            auto_diarization_attempted: false,
        }
    }
}

impl Settings {
    pub fn path() -> Result<PathBuf> {
        Ok(ProjectDirs::from("", "", "notetaker")
            .context("Не найден каталог настроек")?
            .config_dir()
            .join("app.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let settings: Self =
            toml::from_str(&fs::read_to_string(path)?).context("Некорректный app.toml")?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.library.is_absolute(),
            "Путь библиотеки должен быть абсолютным"
        );
        ensure!(
            MODELS.contains(&self.model.as_str()),
            "Неизвестная модель ASR"
        );
        ensure!(
            self.retention_days.is_none_or(|days| days <= 36_500),
            "Срок хранения: 0–36500 дней или навсегда"
        );
        ensure!(
            self.speakers.is_none_or(|n| (1..=32).contains(&n)),
            "Участников: авто или 1–32"
        );
        for exe in self
            .call_apps
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            ensure!(
                exe.to_ascii_lowercase().ends_with(".exe")
                    && !exe.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
                    && exe.len() > 4,
                "Приложения для звонков: укажите имена exe без путей, по одному на строку"
            );
        }
        notetaker_core::calendar::timezone(&self.calendar_timezone)?;
        ensure!(
            self.calendars.len() <= 16,
            "Можно добавить до 16 календарей"
        );
        let mut calendar_ids = std::collections::HashSet::new();
        for calendar in &self.calendars {
            ensure!(
                calendar_ids.insert(&calendar.id),
                "Повторяющийся id календаря"
            );
            calendar.validate()?;
        }
        validate_hotkey(&self.hotkey)
    }
}

// Keep the persisted schema identical to llm::Config without adding serialization
// concerns to that crate. Config::parse is the authority for validation/privacy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmSettings {
    pub defaults: LlmDefaults,
    pub providers: Vec<ProviderSettings>,
    #[serde(default)]
    pub scenarios: notetaker_llm::Scenarios,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmDefaults {
    pub local: String,
    pub hybrid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default = "env_key_source")]
    pub key_source: String,
    #[serde(default = "cli_context")]
    pub context_tokens: usize,
}

fn cli_context() -> usize {
    150_000
}

fn env_key_source() -> String {
    "env".into()
}

impl ProviderSettings {
    pub fn config(&self) -> Result<notetaker_llm::ProviderConfig> {
        let settings = LlmSettings {
            defaults: LlmDefaults {
                local: self.id.clone(),
                hybrid: self.id.clone(),
            },
            providers: vec![self.clone()],
            scenarios: Default::default(),
        };
        Ok(notetaker_llm::Config::parse(&toml::to_string(&settings)?)?
            .providers
            .remove(0))
    }
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            id: "ollama".into(),
            kind: "openai".into(),
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen3:8b".into(),
            api_key_env: None,
            key_source: "env".into(),
            context_tokens: 32_768,
        }
    }
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            defaults: LlmDefaults {
                local: "ollama".into(),
                hybrid: "ollama".into(),
            },
            providers: vec![ProviderSettings::default()],
            scenarios: Default::default(),
        }
    }
}

impl LlmSettings {
    pub fn load() -> Result<Self> {
        let path = notetaker_llm::Config::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        // Never echo config excerpts, including accidental secrets.
        let text = fs::read_to_string(path)?;
        notetaker_llm::Config::parse(&text)?;
        toml::from_str(&text).map_err(|_| anyhow::anyhow!("Некорректный LLM config.toml"))
    }

    pub fn validate(&self) -> Result<String> {
        for provider in &self.providers {
            if let Some(name) = &provider.api_key_env {
                ensure!(
                    !name.is_empty()
                        && name.chars().enumerate().all(|(i, c)| c == '_'
                            || c.is_ascii_alphabetic()
                            || (i > 0 && c.is_ascii_digit())),
                    "api_key_env: укажите имя переменной окружения, например OPENAI_API_KEY"
                );
            }
        }
        let text = toml::to_string_pretty(self)?;
        notetaker_llm::Config::parse(&text)?;
        Ok(text)
    }
}

pub fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let parent = path.parent().context("Нет родительского каталога")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn validate_hotkey(value: &str) -> Result<()> {
    let mut parts: Vec<_> = value.split('+').collect();
    let key = parts.pop().unwrap_or_default();
    ensure!(
        !parts.is_empty(),
        "Хоткей должен содержать модификатор, например Ctrl+Alt+R"
    );
    let mut seen = std::collections::HashSet::new();
    for part in parts {
        ensure!(
            matches!(part, "Ctrl" | "Alt" | "Shift" | "Super") && seen.insert(part),
            "Модификаторы: Ctrl, Alt, Shift, Super (без повторов)"
        );
    }
    ensure!(
        (key.len() == 1
            && key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
            || key
                .strip_prefix('F')
                .and_then(|n| n.parse::<u8>().ok())
                .is_some_and(|n| (1..=24).contains(&n)),
        "Клавиша: A–Z, 0–9 или F1–F24"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quickstart_preferences_roundtrip() {
        let settings = Settings {
            hide_quickstart: true,
            auto_download_models: false,
            auto_diarization_attempted: true,
            ..Settings::default()
        };
        let restored: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert!(restored.hide_quickstart);
        assert!(!restored.auto_download_models);
        assert!(restored.auto_diarization_attempted);
    }
    #[test]
    fn existing_settings_get_detection_and_silence_defaults() {
        let settings: Settings = toml::from_str("hotkey = 'Ctrl+Alt+R'").unwrap();
        assert_eq!(settings.theme, crate::theme::Theme::System);
        assert!(settings.diarize);
        assert!(!settings.voice_library);
        assert_eq!(settings.speakers, None);
        assert!(settings.detect_calls);
        assert!(settings.check_updates);
        assert!(!settings.hide_quickstart);
        assert!(settings.auto_download_models);
        assert!(!settings.auto_diarization_attempted);
        assert!(settings.detect_browsers);
        assert!(settings.detect_any_microphone);
        assert!(settings.call_apps.is_empty());
        assert_eq!(settings.auto_stop_minutes, 15);
        let settings: Settings =
            toml::from_str("auto_stop_minutes = 0\ndetect_calls = false\ndetect_browsers = true")
                .unwrap();
        assert_eq!(settings.auto_stop_minutes, 0);
        assert!(!settings.detect_calls);
        assert!(settings.detect_browsers);
    }
    #[test]
    fn custom_call_apps_roundtrip_and_reject_paths_or_patterns() {
        let mut settings = Settings {
            library: std::env::current_dir().unwrap(),
            call_apps: "Telemost.exe\n  KonturTalk.exe  \n".into(),
            detect_any_microphone: false,
            ..Settings::default()
        };
        settings.validate().unwrap();
        let restored: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.call_apps, settings.call_apps);
        assert!(!restored.detect_any_microphone);
        for invalid in ["C:\\Call.exe", "../call.exe", "*.exe", "call", ".exe"] {
            settings.call_apps = invalid.into();
            assert!(settings.validate().is_err(), "{invalid}");
        }
    }

    #[test]
    fn validates_settings_and_roundtrip() {
        let mut settings = Settings {
            library: PathBuf::from(if cfg!(windows) { "C:\\notes" } else { "/notes" }),
            ..Settings::default()
        };
        assert!(settings.validate().is_ok());
        settings.theme = crate::theme::Theme::Dark;
        let text = toml::to_string(&settings).unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&text).unwrap().theme,
            crate::theme::Theme::Dark
        );
        assert_eq!(
            toml::from_str::<Settings>(&text).unwrap().hotkey,
            "Ctrl+Alt+R"
        );
        settings.retention_days = None;
        let forever = toml::to_string(&settings).unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&forever).unwrap().retention_days,
            None
        );
        settings.retention_days = Some(0);
        let immediate = toml::to_string(&settings).unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&immediate)
                .unwrap()
                .retention_days,
            Some(0)
        );
        settings.library = "relative".into();
        assert!(settings.validate().is_err());
        settings.library = std::env::current_dir().unwrap();
        settings.model = "unknown".into();
        assert!(settings.validate().is_err());
        for key in ["R", "Ctrl+Ctrl+R", "Alt+", "Ctrl+F25", "Ctrl+é"] {
            assert!(validate_hotkey(key).is_err(), "{key}");
        }
        for key in ["Ctrl+Alt+R", "Super+Shift+F12", "Alt+0"] {
            assert!(validate_hotkey(key).is_ok(), "{key}");
        }
    }
    #[test]
    fn validates_llm_without_storing_keys() {
        let mut config = LlmSettings::default();
        assert!(config.validate().is_ok());
        config.providers[0].base_url = "https://api.example.com/v1".into();
        assert!(config.validate().is_ok()); // remote Local default fails only Local meetings
        config.providers[0].base_url = "http://localhost:11434/v1".into();
        config.providers[0].api_key_env = Some("sk-secret-value".into());
        assert!(config.validate().is_err());
        config.providers[0].api_key_env = Some("LLM_API_KEY".into());
        assert!(config.validate().is_ok());
    }
}
