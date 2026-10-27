//! ICS URLs share the gateway credential abstraction; errors never include URLs.
use std::{io::Read, time::Duration};

use anyhow::{Result, ensure};
use notetaker_core::calendar::{CalendarEvent, parse_ics, timezone};
use serde::{Deserialize, Serialize};

use crate::CredentialStore;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Subscription {
    pub id: String,
    pub name: String,
}
impl Subscription {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.id.starts_with("calendar-")
                && self.id.len() <= 80
                && self
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Некорректный id календаря"
        );
        ensure!(!self.name.trim().is_empty(), "Укажите название календаря");
        Ok(())
    }
    pub fn save_url(&self, url: &str, store: &dyn CredentialStore) -> Result<()> {
        self.validate()?;
        validate_url(url)?;
        store.write(&self.id, url)
    }
}
fn validate_url(secret: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(secret).map_err(|_| anyhow::anyhow!("Некорректный адрес ICS"))?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "Для подписки нужен HTTPS URL без userinfo и fragment"
    );
    Ok(url)
}

pub fn refresh(
    subscriptions: &[Subscription],
    zone: &str,
    now: chrono::DateTime<chrono::Utc>,
    store: &dyn CredentialStore,
) -> Result<Vec<CalendarEvent>> {
    let zone = timezone(zone)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| anyhow::anyhow!("Не удалось создать клиент календаря"))?;
    let mut events = Vec::new();
    for subscription in subscriptions {
        subscription.validate()?;
        let secret = store.read(&subscription.id)?;
        let url = validate_url(&secret)?;
        // Do not format reqwest errors: they contain the private subscription URL.
        let response = client
            .get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|_| {
                anyhow::anyhow!(
                    "Не удалось загрузить календарь; проверьте URL и сеть (редиректы запрещены)"
                )
            })?;
        let mut bytes = Vec::new();
        response
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Не удалось прочитать календарь"))?;
        ensure!(bytes.len() <= 8 * 1024 * 1024, "Календарь больше 8 МБ");
        let text =
            String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("ICS должен быть UTF-8"))?;
        events.extend(parse_ics(
            &text,
            now - chrono::Duration::days(2),
            now + chrono::Duration::days(2),
            zone,
        )?);
    }
    events.sort_by_key(|event| (event.start, event.link.uid.clone()));
    events.dedup_by(|a, b| a.start == b.start && a.link.uid == b.link.uid);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Memory(RefCell<String>);
    impl CredentialStore for Memory {
        fn read(&self, _: &str) -> Result<String> {
            Ok(self.0.borrow().clone())
        }
        fn write(&self, _: &str, secret: &str) -> Result<()> {
            *self.0.borrow_mut() = secret.into();
            Ok(())
        }
    }
    #[test]
    fn secret_url_only_reaches_credential_store() {
        let sub = Subscription {
            id: "calendar-test".into(),
            name: "Работа".into(),
        };
        let store = Memory(RefCell::default());
        sub.save_url("https://example.org/private.ics?token=TEST", &store)
            .unwrap();
        assert!(store.read(&sub.id).unwrap().contains("token=TEST"));
        assert!(!toml::to_string(&sub).unwrap().contains("TEST"));
        let error = sub
            .save_url("http://secret.example.org/token=TEST", &store)
            .unwrap_err();
        assert!(!format!("{error:#}").contains("TEST"));
    }
}
