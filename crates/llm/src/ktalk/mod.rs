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
