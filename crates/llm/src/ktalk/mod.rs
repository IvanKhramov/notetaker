//! Read-only Tolk integration. Remote text exists only in memory.
mod client;
mod sync;
pub use client::{
    AUDIO_LIMIT, Client, NetworkError, Participant, Recording, RecordingQuality, Transcript,
    TranscriptionStatus, User,
};
pub use sync::{
    AppliedTranscript, Plan, Row, SelectionError, VoiceEvidence, apply, match_recording, plan,
    select_recording,
};

use crate::CredentialStore;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct TranscriptNotReady;
impl std::fmt::Display for TranscriptNotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Транскрипция Толка ещё не готова")
    }
}
impl std::error::Error for TranscriptNotReady {}
#[derive(Debug)]
pub struct InvalidTranscript(pub String);
impl std::fmt::Display for InvalidTranscript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InvalidTranscript {}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Settings {
    pub base_url: String,
    pub credential_id: String,
    pub api_key_env: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, toml::Value>,
}
impl Settings {
    /// Read only the existing integration section; AI validation is independent.
    pub fn from_path(path: &std::path::Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let value: toml::Value =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("Некорректный config.toml"))?;
        value
            .get("ktalk")
            .cloned()
            .map(|value| {
                value
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Некорректные настройки Толка"))
            })
            .transpose()
    }
    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        client::base_url(&self.base_url)?;
        crate::config_write::save(
            path,
            crate::config_write::Section::Ktalk,
            toml::Value::try_from(self)?,
        )
    }
    pub fn auto_sync(&self) -> bool {
        self.extra
            .get("auto_sync")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true)
    }
    pub fn set_auto_sync(&mut self, enabled: bool) {
        self.extra
            .insert("auto_sync".into(), toml::Value::Boolean(enabled));
    }
    pub fn validate(&self) -> Result<()> {
        client::base_url(&self.base_url)?;
        Ok(())
    }
    pub fn save_key(&self, key: &str, store: &dyn CredentialStore) -> Result<()> {
        let origin = client::base_url(&self.base_url)?
            .origin()
            .ascii_serialization();
        ensure!(!self.credential_id.is_empty(), "Укажите имя ключа Толка");
        store.write(&format!("ktalk/{}", self.credential_id), key)?;
        store.write(&format!("ktalk-origin/{}", self.credential_id), &origin)
    }
    pub fn client(&self, store: &dyn CredentialStore) -> Result<Client> {
        let origin = client::base_url(&self.base_url)?
            .origin()
            .ascii_serialization();
        let key = if let Some(env) = &self.api_key_env {
            std::env::var(env)
                .map_err(|_| anyhow::anyhow!("Не найдена переменная окружения с ключом Толка"))?
        } else {
            ensure!(
                store.read(&format!("ktalk-origin/{}", self.credential_id))? == origin,
                "Адрес Толка изменился; сохраните ключ заново"
            );
            store.read(&format!("ktalk/{}", self.credential_id))?
        };
        Client::new(&self.base_url, &key)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn integration_section_saves_without_ai_and_preserves_other_tables() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path,"[defaults]\nlocal='missing'\nhybrid='missing'\n[ktalk]\nbase_url='https://neutral.ktalk.ru'\ncredential_id='neutral'\nfuture_flag={enabled=true}\n").unwrap();
        assert!(crate::Config::from_path(&path).is_err());
        let mut settings = super::Settings::from_path(&path).unwrap().unwrap();
        settings.set_auto_sync(false);
        settings.save_to(&path).unwrap();
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["defaults"]["local"].as_str(), Some("missing"));
        let loaded = super::Settings::from_path(&path).unwrap().unwrap();
        assert!(!loaded.auto_sync());
        assert_eq!(
            settings.extra.get("future_flag"),
            loaded.extra.get("future_flag")
        );
    }
    #[test]
    fn cli_config_load_and_shared_settings_serialization_preserve_unknown_keys() {
        let text = "[defaults]\nlocal='test'\nhybrid='test'\n[[providers]]\nid='test'\nkind='openai'\nbase_url='https://example.org/v1'\nmodel='test'\n[ktalk]\nbase_url='https://neutral.ktalk.ru'\ncredential_id='work'\nfuture_mode='strict'\nfuture_option={enabled=true, values=[1,2]}\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        let settings = crate::Config::from_path(&path).unwrap().ktalk.unwrap();
        let before: toml::Value = toml::from_str(text).unwrap();
        let after: toml::Value = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(before.get("ktalk"), Some(&after));
        assert_eq!(settings.extra.len(), 2);
    }
}

pub use sync::recording_voice_evidence;
mod learning;
pub use learning::{LearningPlan, LearningQueue, learning_origin, learning_plan, learning_source};
