//! Read-only Tolk integration. Remote text exists only in memory.
mod client;
mod sync;
pub use client::{Client, Participant, Recording, Transcript, User};
pub use sync::{Plan, Row, apply, match_recording, plan, select_recording};

use crate::CredentialStore;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Settings {
    pub base_url: String,
    pub credential_id: String,
    pub api_key_env: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, toml::Value>,
}
impl Settings {
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
