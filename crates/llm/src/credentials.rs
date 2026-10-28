use anyhow::{Context, Result};

use crate::{KeySource, ProviderConfig};

pub trait CredentialStore {
    fn read(&self, id: &str) -> Result<String>;
    fn write(&self, id: &str, key: &str) -> Result<()>;
    fn delete(&self, _id: &str) -> Result<()> {
        anyhow::bail!("Удаление учётных данных недоступно")
    }
}

pub struct OsCredentialStore;

pub fn resolve_key(config: &ProviderConfig, store: &dyn CredentialStore) -> Result<Option<String>> {
    match config.key_source {
        KeySource::Credential => store.read(&config.id).map(Some),
        KeySource::Env => config
            .api_key_env
            .as_ref()
            .map(|name| std::env::var(name).context("Не найдена переменная окружения с ключом LLM"))
            .transpose(),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
impl CredentialStore for OsCredentialStore {
    fn read(&self, _id: &str) -> Result<String> {
        anyhow::bail!("Хранилище ключей доступно на Windows; используйте api_key_env в Расширенных")
    }
    fn write(&self, _id: &str, _key: &str) -> Result<()> {
        anyhow::bail!("Хранилище ключей доступно на Windows; используйте api_key_env в Расширенных")
    }
}

#[cfg(windows)]
impl CredentialStore for OsCredentialStore {
    #[allow(unsafe_code)]
    fn read(&self, id: &str) -> Result<String> {
        use windows::{Win32::Security::Credentials::*, core::PCWSTR};
        let target: Vec<u16> = format!("notetaker/llm/{id}\0").encode_utf16().collect();
        let mut credential = std::ptr::null_mut();
        // CredRead owns the allocated blob until CredFree, including on decode errors.
        unsafe {
            CredReadW(
                PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut credential,
            )
            .map_err(|_| anyhow::anyhow!("Ключ LLM не найден в Credential Manager"))?;
            let result = if (*credential).CredentialBlobSize == 0
                || (*credential).CredentialBlob.is_null()
            {
                Err(anyhow::anyhow!("Пустой ключ в Credential Manager"))
            } else {
                let bytes = std::slice::from_raw_parts(
                    (*credential).CredentialBlob,
                    (*credential).CredentialBlobSize as usize,
                );
                String::from_utf8(bytes.to_vec()).context("Некорректный ключ в Credential Manager")
            };
            CredFree(credential.cast());
            result
        }
    }
    #[allow(unsafe_code)]
    fn delete(&self, id: &str) -> Result<()> {
        use windows::{Win32::Security::Credentials::*, core::PCWSTR};
        let target: Vec<u16> = format!("notetaker/llm/{id}\0").encode_utf16().collect();
        unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) }
            .map_err(|_| anyhow::anyhow!("Не удалось удалить учётные данные"))
    }
    #[allow(unsafe_code)]
    fn write(&self, id: &str, key: &str) -> Result<()> {
        use windows::{Win32::Security::Credentials::*, core::PWSTR};
        anyhow::ensure!(
            !key.trim().is_empty() && key.len() <= 2560,
            "Пустой или слишком длинный ключ"
        );
        let mut target: Vec<u16> = format!("notetaker/llm/{id}\0").encode_utf16().collect();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: key.len() as u32,
            CredentialBlob: key.as_ptr().cast_mut(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        unsafe { CredWriteW(&credential, 0) }
            .map_err(|_| anyhow::anyhow!("Не удалось сохранить ключ в Credential Manager"))
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for OsCredentialStore {
    fn read(&self, id: &str) -> Result<String> {
        let bytes = security_framework::passwords::get_generic_password("notetaker/llm", id)
            .map_err(|_| anyhow::anyhow!("Ключ не найден или доступ к Keychain запрещён"))?;
        String::from_utf8(bytes).context("Некорректный ключ в Keychain")
    }
    fn write(&self, id: &str, key: &str) -> Result<()> {
        anyhow::ensure!(
            !key.trim().is_empty() && key.len() <= 2560,
            "Пустой или слишком длинный ключ"
        );
        security_framework::passwords::set_generic_password("notetaker/llm", id, key.as_bytes())
            .map_err(|_| anyhow::anyhow!("Не удалось сохранить ключ в Keychain"))
    }
    fn delete(&self, id: &str) -> Result<()> {
        security_framework::passwords::delete_generic_password("notetaker/llm", id)
            .map_err(|_| anyhow::anyhow!("Не удалось удалить ключ из Keychain"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    #[derive(Default)]
    struct Memory(RefCell<String>);
    impl CredentialStore for Memory {
        fn read(&self, _: &str) -> Result<String> {
            Ok(self.0.borrow().clone())
        }
        fn write(&self, _: &str, key: &str) -> Result<()> {
            *self.0.borrow_mut() = key.into();
            Ok(())
        }
    }
    #[test]
    fn credentials_are_resolved_without_environment_or_config_keys() {
        let config = crate::Config::parse("[defaults]\nlocal='test'\nhybrid='test'\n[[providers]]\nid='test'\nkind='openai'\nbase_url='https://example.com/v1'\nmodel='test'\nkey_source='credential'").unwrap();
        let store = Memory::default();
        store.write("test", "test-secret").unwrap();
        assert_eq!(
            resolve_key(&config.providers[0], &store)
                .unwrap()
                .as_deref(),
            Some("test-secret")
        );
    }
}
