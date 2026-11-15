use std::{io::Read, time::Duration};

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, de::DeserializeOwned};

const LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub key: String,
    #[serde(default)]
    pub firstname: String,
    #[serde(default)]
    pub surname: String,
}
impl User {
    pub fn name(&self) -> String {
        format!("{} {}", self.firstname.trim(), self.surname.trim())
            .trim()
            .into()
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub is_anonymous: bool,
    pub anonymous_id: Option<String>,
    pub anonymous_name: Option<String>,
    pub user_info: Option<User>,
}
impl Participant {
    pub fn identity(&self) -> Result<(String, String)> {
        let (id, name) = if self.is_anonymous {
            (
                format!(
                    "guest:{}",
                    self.anonymous_id
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| anyhow::anyhow!("У гостя Толка отсутствует ID"))?
                ),
                self.anonymous_name.clone().unwrap_or_default(),
            )
        } else {
            let user = self
                .user_info
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("У участника Толка отсутствует userInfo"))?;
            ensure!(!user.key.is_empty(), "У участника Толка отсутствует ID");
            (format!("user:{}", user.key), user.name())
        };
        ensure!(!name.trim().is_empty(), "У участника Толка отсутствует имя");
        Ok((id, name.trim().into()))
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub id: String,
    pub title: String,
    pub created_date: DateTime<Utc>,
    pub duration: u32,
    pub status: String,
}
#[derive(Debug, Deserialize)]
pub struct Recordings {
    pub recordings: Vec<Recording>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Transcript {
    pub status: String,
    pub tracks: Vec<Track>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Track {
    pub speaker: Participant,
    pub chunks: Vec<Chunk>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Chunk {
    pub words: Vec<Word>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    pub text: String,
    pub start_time_offset_in_millis: i64,
    pub end_time_offset_in_millis: i64,
}

pub struct Client {
    base: reqwest::Url,
    http: reqwest::blocking::Client,
    key: reqwest::header::HeaderValue,
}
pub(super) fn base_url(base: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(base).map_err(|_| anyhow::anyhow!("Некорректный адрес Толка"))?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "Для Толка нужен HTTPS адрес пространства без пути, query и userinfo"
    );
    Ok(url)
}
impl Client {
    pub fn new(base: &str, key: &str) -> Result<Self> {
        Self::build(base_url(base)?, key)
    }
    fn build(base: reqwest::Url, key: &str) -> Result<Self> {
        ensure!(!key.trim().is_empty(), "Пустой ключ Толка");
        let mut key = reqwest::header::HeaderValue::from_str(key)
            .map_err(|_| anyhow::anyhow!("Некорректный ключ Толка"))?;
        key.set_sensitive(true);
        Ok(Self {
            base,
            key,
            http: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| anyhow::anyhow!("Не удалось создать клиент Толка"))?,
        })
    }
    fn get<T: DeserializeOwned>(&self, path: &[&str], query: &[(&str, String)]) -> Result<T> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Некорректный адрес Толка"))?
            .clear()
            .extend(path);
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v)));
        }
        let response = self
            .http
            .get(url)
            .header("X-Auth-Token", &self.key)
            .send()
            .map_err(|_| anyhow::anyhow!("Не удалось получить данные Толка: сеть или таймаут"))?;
        let code = response.status().as_u16();
        ensure!(
            code != 429,
            "Толк: превышен лимит 50 запросов в минуту; повторите позже"
        );
        ensure!(code != 401, "Толк: ключ не принят (HTTP 401)");
        ensure!(
            response.status().is_success(),
            "Толк: HTTP {code}; редиректы запрещены"
        );
        let mut bytes = Vec::new();
        response
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Не удалось прочитать ответ Толка"))?;
        ensure!(bytes.len() as u64 <= LIMIT, "Ответ Толка больше 16 МиБ");
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Некорректная структура ответа Толка"))
    }
    pub fn profile(&self) -> Result<User> {
        let user: User = self.get(&["api", "Users", "profile"], &[])?;
        ensure!(
            !user.key.is_empty() && !user.name().is_empty(),
            "В профиле Толка отсутствует ID или имя"
        );
        Ok(user)
    }
    pub fn recordings(&self, query: &str, skip: usize, top: usize) -> Result<Vec<Recording>> {
        Ok(self
            .get::<Recordings>(
                &["api", "Recordings"],
                &[
                    ("query", query.into()),
                    ("skip", skip.to_string()),
                    ("top", top.to_string()),
                ],
            )?
            .recordings)
    }
    pub fn transcript(&self, id: &str) -> Result<Transcript> {
        let transcript: Transcript = self.get(&["api", "recordings", id, "transcript"], &[])?;
        ensure!(
            transcript.status == "complete",
            "Транскрипция Толка ещё не готова"
        );
        for track in &transcript.tracks {
            track.speaker.identity()?;
            for word in track.chunks.iter().flat_map(|c| &c.words) {
                ensure!(
                    word.start_time_offset_in_millis >= 0
                        && word.end_time_offset_in_millis > word.start_time_offset_in_millis,
                    "Некорректное время слова Толка"
                );
            }
        }
        Ok(transcript)
    }
}

#[cfg(test)]
mod tests;
