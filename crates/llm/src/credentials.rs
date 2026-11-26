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

/// Raw generic-password items, addressed by service and account.
#[cfg(any(target_os = "macos", test))]
trait RawItems {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, service: &str, account: &str, data: &[u8]) -> Result<()>;
    /// Succeeds when the item is already absent.
    fn remove(&self, service: &str, account: &str) -> Result<()>;
}

/// All secrets live in one Keychain item. The ad-hoc signature changes with
/// every build, and macOS asks for the login password once per item it
/// protects, so one item means one prompt after an update instead of one per
/// secret. The map is cached per process, so a plain "Allow" covers the run.
#[cfg(any(target_os = "macos", test))]
struct Bundled<R> {
    raw: R,
    cache: std::sync::Mutex<Option<std::collections::BTreeMap<String, String>>>,
}

#[cfg(any(target_os = "macos", test))]
impl<R: RawItems> Bundled<R> {
    const SERVICE: &str = "notetaker";
    const ACCOUNT: &str = "secrets";
    /// One item per secret, written by versions up to 0.37.0.
    const LEGACY: &str = "notetaker/llm";

    fn load(&self) -> Result<std::collections::BTreeMap<String, String>> {
        match self.raw.get(Self::SERVICE, Self::ACCOUNT)? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).context("Хранилище ключей в Keychain повреждено")
            }
            None => Ok(Default::default()),
        }
    }
    fn store(&self, map: &std::collections::BTreeMap<String, String>) -> Result<()> {
        self.raw
            .set(Self::SERVICE, Self::ACCOUNT, &serde_json::to_vec(map)?)
    }
    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<std::collections::BTreeMap<String, String>>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn read(&self, id: &str) -> Result<String> {
        let mut cache = self.lock();
        if cache.is_none() {
            *cache = Some(self.load()?);
        }
        let map = cache.as_mut().expect("loaded above");
        if let Some(secret) = map.get(id) {
            return Ok(secret.clone());
        }
        // Move a secret of an older version into the shared item on first use.
        let bytes = self
            .raw
            .get(Self::LEGACY, id)?
            .context("Ключ не найден в Keychain")?;
        let secret = String::from_utf8(bytes).context("Некорректный ключ в Keychain")?;
        let mut fresh = self.load()?;
        fresh.insert(id.into(), secret.clone());
        self.store(&fresh)?;
        *map = fresh;
        Ok(secret)
    }
    fn write(&self, id: &str, key: &str) -> Result<()> {
        let mut cache = self.lock();
        // Re-read: another notetaker process may have changed the item.
        let mut map = self.load()?;
        map.insert(id.into(), key.into());
        self.store(&map)?;
        *cache = Some(map);
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<()> {
        let mut cache = self.lock();
        let mut map = self.load()?;
        map.remove(id);
        self.store(&map)?;
        *cache = Some(map);
        // A legacy copy would otherwise come back on the next read.
        self.raw.remove(Self::LEGACY, id)
    }
}

#[cfg(target_os = "macos")]
struct Keychain;

#[cfg(target_os = "macos")]
impl RawItems for Keychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(service, account) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.code() == security_framework_sys::base::errSecItemNotFound => {
                Ok(None)
            }
            Err(_) => anyhow::bail!("Доступ к Keychain запрещён"),
        }
    }
    fn set(&self, service: &str, account: &str, data: &[u8]) -> Result<()> {
        security_framework::passwords::set_generic_password(service, account, data)
            .map_err(|_| anyhow::anyhow!("Не удалось сохранить ключ в Keychain"))
    }
    fn remove(&self, service: &str, account: &str) -> Result<()> {
        match security_framework::passwords::delete_generic_password(service, account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == security_framework_sys::base::errSecItemNotFound => {
                Ok(())
            }
            Err(_) => anyhow::bail!("Не удалось удалить ключ из Keychain"),
        }
    }
}

#[cfg(target_os = "macos")]
static KEYCHAIN: Bundled<Keychain> = Bundled {
    raw: Keychain,
    cache: std::sync::Mutex::new(None),
};

#[cfg(target_os = "macos")]
impl CredentialStore for OsCredentialStore {
    fn read(&self, id: &str) -> Result<String> {
        KEYCHAIN.read(id)
    }
    fn write(&self, id: &str, key: &str) -> Result<()> {
        anyhow::ensure!(
            !key.trim().is_empty() && key.len() <= 2560,
            "Пустой или слишком длинный ключ"
        );
        KEYCHAIN.write(id, key)
    }
    fn delete(&self, id: &str) -> Result<()> {
        KEYCHAIN.delete(id)
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

    #[derive(Default)]
    struct Items {
        data: RefCell<std::collections::BTreeMap<(String, String), Vec<u8>>>,
        gets: std::cell::Cell<usize>,
    }
    impl RawItems for Items {
        fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>> {
            self.gets.set(self.gets.get() + 1);
            Ok(self
                .data
                .borrow()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn set(&self, service: &str, account: &str, data: &[u8]) -> Result<()> {
            self.data
                .borrow_mut()
                .insert((service.into(), account.into()), data.to_vec());
            Ok(())
        }
        fn remove(&self, service: &str, account: &str) -> Result<()> {
            self.data
                .borrow_mut()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    fn bundled() -> Bundled<Items> {
        Bundled {
            raw: Items::default(),
            cache: Default::default(),
        }
    }
    #[test]
    fn secrets_share_one_item_and_reads_are_cached() {
        let store = bundled();
        store.write("llm", "one").unwrap();
        store.write("calendar-1", "two").unwrap();
        assert_eq!(store.raw.data.borrow().len(), 1);
        let gets = store.raw.gets.get();
        assert_eq!(store.read("llm").unwrap(), "one");
        assert_eq!(store.read("calendar-1").unwrap(), "two");
        assert_eq!(
            store.raw.gets.get(),
            gets,
            "cached reads touch the Keychain"
        );
        assert!(store.read("missing").is_err());
    }
    #[test]
    fn legacy_items_move_into_the_shared_item_and_deleted_ones_stay_deleted() {
        let store = bundled();
        store.raw.set("notetaker/llm", "llm", b"old").unwrap();
        store.raw.set("notetaker/llm", "ktalk/1", b"token").unwrap();
        assert_eq!(store.read("llm").unwrap(), "old");
        let shared: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&store.raw.get("notetaker", "secrets").unwrap().unwrap())
                .unwrap();
        assert_eq!(shared.get("llm").map(String::as_str), Some("old"));
        // A fresh process reads from the shared item.
        let later = Bundled {
            raw: store.raw,
            cache: Default::default(),
        };
        assert_eq!(later.read("llm").unwrap(), "old");
        later.delete("ktalk/1").unwrap();
        assert!(later.read("ktalk/1").is_err());
        later.write("llm", "new").unwrap();
        assert_eq!(later.read("llm").unwrap(), "new");
    }
}
