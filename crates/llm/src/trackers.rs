//! Explicit task export. No credentials or network calls are used by preview.
use crate::CredentialStore;
use anyhow::{Context, Result, ensure};
use hmac::{Hmac, KeyInit, Mac};
use notetaker_core::{
    Library, Mode, Task,
    folders::{TrackerConfig, TrackerKind},
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    time::Duration,
};

pub trait Tracker {
    fn request(
        &self,
        config: &TrackerConfig,
        task: &Task,
        context: &ContextData,
    ) -> Result<(String, Value)>;
    fn issue_url(&self, config: &TrackerConfig, response: &Value) -> Result<String>;
}
pub struct Webhook;
pub struct Kaiten;
pub struct Jira;
pub struct YouTrack;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextData {
    pub meeting_title: String,
    pub meeting_date: String,
    pub meeting_link: String,
    pub summary_excerpt: String,
    pub owner_email: Option<String>,
}
impl ContextData {
    fn description(&self, task: &Task) -> String {
        format!(
            "{}\nОтветственный: {}\nСрок: {}\n\n{} ({})\n{}\n\n{}",
            task.source,
            task.owner,
            task.due_text,
            self.meeting_title,
            self.meeting_date,
            self.meeting_link,
            self.summary_excerpt
        )
    }
}
fn base(config: &TrackerConfig) -> Result<String> {
    let url = reqwest::Url::parse(&config.base_url).context("Некорректный URL трекера")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "URL трекера: нужен http(s) без пароля, query и fragment"
    );
    Ok(config.base_url.trim_end_matches('/').to_owned())
}
/// Reject embedded URL credentials before configuration enters a shared file.
pub fn validate_config(config: &TrackerConfig) -> Result<()> {
    base(config)?;
    if !config.signature_header.is_empty() {
        reqwest::header::HeaderName::from_bytes(config.signature_header.as_bytes())
            .context("Некорректное имя заголовка HMAC")?;
    }
    Ok(())
}
fn provider(kind: TrackerKind) -> Box<dyn Tracker> {
    match kind {
        TrackerKind::Webhook => Box::new(Webhook),
        TrackerKind::Kaiten => Box::new(Kaiten),
        TrackerKind::Jira => Box::new(Jira),
        TrackerKind::YouTrack => Box::new(YouTrack),
    }
}
impl Tracker for Webhook {
    fn request(&self, c: &TrackerConfig, t: &Task, data: &ContextData) -> Result<(String, Value)> {
        Ok((
            base(c)?,
            json!({"task":t.text,"owner":t.owner,"due":t.due,"due_text":t.due_text,"meeting":data,"idempotency_key":task_key(t)}),
        ))
    }
    fn issue_url(&self, _: &TrackerConfig, value: &Value) -> Result<String> {
        Ok(value["url"]
            .as_str()
            .context("Webhook должен вернуть JSON с полем url")?
            .into())
    }
}
fn numeric(value: &str, field: &str) -> Result<u64> {
    value
        .parse()
        .with_context(|| format!("Укажите числовой {field}"))
}
impl Tracker for Kaiten {
    fn request(&self, c: &TrackerConfig, t: &Task, data: &ContextData) -> Result<(String, Value)> {
        numeric(&c.space_id, "space_id")?;
        let mut body = json!({"title":t.text,"description":data.description(t),"board_id":numeric(&c.board_id,"board_id")?,"column_id":numeric(&c.column_id,"column_id")?,"lane_id":numeric(&c.lane_id,"lane_id")?});
        if let Some(owner) = data.owner_email.as_ref().and_then(|email| {
            c.owners
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(email))
                .map(|(_, id)| id)
        }) {
            body["responsible_id"] = json!(owner);
        }
        if let Some(due) = t.due {
            body["due_date"] = json!(due.format("%Y-%m-%dT23:59:59Z").to_string());
        }
        Ok((format!("{}/api/latest/cards", base(c)?), body))
    }
    fn issue_url(&self, c: &TrackerConfig, value: &Value) -> Result<String> {
        let id = value["id"]
            .as_u64()
            .context("Kaiten не вернул id карточки")?;
        Ok(format!(
            "{}/space/{}/card/{id}",
            base(c)?,
            numeric(&c.space_id, "space_id")?
        ))
    }
}
impl Tracker for Jira {
    fn request(&self, c: &TrackerConfig, t: &Task, data: &ContextData) -> Result<(String, Value)> {
        ensure!(
            !c.project.trim().is_empty() && !c.issue_type.trim().is_empty(),
            "Укажите проект и тип задачи Jira"
        );
        let description = data.description(t);
        let description = if c.jira_v3 {
            json!({"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":description}]}]})
        } else {
            json!(description)
        };
        let mut body = json!({"fields":{"project":{"key":c.project},"issuetype":{"name":c.issue_type},"summary":t.text,"description":description}});
        if let Some(due) = t.due {
            body["fields"]["duedate"] = json!(due);
        }
        Ok((
            format!(
                "{}/rest/api/{}/issue",
                base(c)?,
                if c.jira_v3 { 3 } else { 2 }
            ),
            body,
        ))
    }
    fn issue_url(&self, c: &TrackerConfig, value: &Value) -> Result<String> {
        let key = value["key"]
            .as_str()
            .context("Jira не вернула ключ задачи")?;
        ensure!(
            key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "Некорректный ключ Jira"
        );
        Ok(format!("{}/browse/{key}", base(c)?))
    }
}
impl Tracker for YouTrack {
    fn request(&self, c: &TrackerConfig, t: &Task, data: &ContextData) -> Result<(String, Value)> {
        ensure!(
            !c.project.trim().is_empty(),
            "Укажите внутренний id проекта YouTrack (например 0-1)"
        );
        Ok((
            format!("{}/api/issues?fields=idReadable", base(c)?),
            json!({"project":{"id":c.project},"summary":t.text,"description":data.description(t)}),
        ))
    }
    fn issue_url(&self, c: &TrackerConfig, value: &Value) -> Result<String> {
        let key = value["idReadable"]
            .as_str()
            .context("YouTrack не вернул idReadable")?;
        ensure!(
            key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "Некорректный ключ YouTrack"
        );
        Ok(format!("{}/issue/{key}", base(c)?))
    }
}
#[derive(Clone, Debug)]
pub struct Preview {
    pub task: Task,
    pub config: TrackerConfig,
    pub context: ContextData,
    pub endpoint: String,
    pub body: Value,
    pub local: bool,
}
pub fn preview(library: &Library, task: Task, config: TrackerConfig) -> Result<Preview> {
    ensure!(task.issue_url.is_none(), "Задача уже отправлена в трекер");
    let meeting = task.meeting(library)?;
    let meta = meeting.read_meta()?;
    let enhanced = meeting.load_enhanced()?;
    ensure!(
        enhanced.revision == task.revision,
        "Задача изменилась; обновите список"
    );
    let mut addresses = std::collections::BTreeSet::new();
    if let Some(event) = &meta.calendar_event {
        for attendee in &event.attendees {
            if !attendee.email.is_empty()
                && (notetaker_core::person_matches(&attendee.name, &task.owner)
                    || attendee.email.eq_ignore_ascii_case(&task.owner))
            {
                addresses.insert(attendee.email.to_lowercase());
            }
        }
    }
    if task.owner.contains('@') {
        addresses.insert(task.owner.to_lowercase());
    }
    if addresses.is_empty() {
        for person in library.people_in_folder(false, "", "", meta.folder.as_deref())? {
            if person.matches(&task.owner) {
                addresses.extend(person.emails);
            }
        }
    }
    let owner_email =
        (addresses.len() == 1).then(|| addresses.into_iter().next().expect("one address"));
    let context = ContextData {
        meeting_title: meta.title,
        meeting_date: meta.started_at.to_rfc3339(),
        meeting_link: format!("notetaker://meeting/{}", meta.id),
        summary_excerpt: enhanced.value.chars().take(2000).collect(),
        owner_email,
    };
    let (endpoint, body) = provider(config.kind).request(&config, &task, &context)?;
    Ok(Preview {
        task,
        config,
        context,
        endpoint,
        body,
        local: meta.mode == Mode::Local,
    })
}
pub fn signature(secret: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC supports any key size");
    mac.update(body);
    format!("sha256={}", hex(mac.finalize().into_bytes().as_slice()))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn task_key(t: &Task) -> String {
    hex(Sha256::digest(format!(
        "{}\n{}\n{}\n{}\n{}",
        t.meeting_id, t.line, t.text, t.owner, t.due_text
    ))
    .as_slice())
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    url: Option<String>,
}
/// A pending receipt blocks automatic retry after timeout/crash. The user must
/// check the tracker before removing it. Remote APIs have no common transaction.
pub fn send(
    library: &Library,
    preview: &Preview,
    confirm_local: bool,
    store: &dyn CredentialStore,
) -> Result<String> {
    let meeting = preview.task.meeting(library)?;
    let current = meeting.read_meta()?;
    ensure!(
        current.mode != Mode::Local || confirm_local,
        "Подтвердите отправку Local-задачи: данные покинут компьютер"
    );
    let latest = meeting.load_enhanced()?;
    ensure!(
        latest.revision == preview.task.revision,
        "Конспект изменился; создайте новый просмотр"
    );
    let key = task_key(&preview.task);
    let receipt_path = meeting.path().join(format!(".tracker-{key}.json"));
    if let Ok(bytes) = fs::read(&receipt_path) {
        let receipt: Receipt = serde_json::from_slice(&bytes)?;
        if let Some(url) = receipt.url {
            if preview.task.issue_url.is_none() {
                meeting.link_task_issue(&preview.task, &url)?;
            }
            return Ok(url);
        }
        anyhow::bail!("Предыдущая отправка не подтверждена. Проверьте трекер перед повтором");
    }
    ensure!(preview.task.issue_url.is_none(), "Задача уже отправлена");
    let config = &preview.config;
    let secret = if config.credential_id.is_empty() {
        None
    } else {
        Some(store.read(&format!("tracker/{}", config.credential_id))?)
    };
    ensure!(
        config.kind == TrackerKind::Webhook || secret.is_some(),
        "Укажите ключ трекера в хранилище учётных данных"
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let bytes = serde_json::to_vec(&preview.body)?;
    let mut request = client
        .post(&preview.endpoint)
        .header("Content-Type", "application/json")
        .header("Idempotency-Key", &key)
        .body(bytes.clone());
    if let Some(secret) = secret {
        if config.kind == TrackerKind::Webhook {
            let header = if config.signature_header.is_empty() {
                "X-Notetaker-Signature"
            } else {
                &config.signature_header
            };
            request = request.header(header, signature(secret.as_bytes(), &bytes));
        } else if config.kind == TrackerKind::Jira && !config.email.is_empty() {
            request = request.basic_auth(&config.email, Some(secret));
        } else {
            request = request.bearer_auth(secret);
        }
    }
    // Build and validate headers before recording an attempted network operation.
    let request = request.build().context("Некорректные параметры трекера")?;
    let mut receipt = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&receipt_path)
        .context("Отправка уже началась или нельзя сохранить квитанцию")?;
    receipt.write_all(b"{\"url\":null}")?;
    receipt.sync_all()?;
    drop(receipt);
    let response = client.execute(request).map_err(|_| {
        anyhow::anyhow!("Ошибка отправки. Проверьте трекер; автоматический повтор отключён")
    })?;
    ensure!(
        response.status().is_success(),
        "Трекер вернул HTTP {}; проверьте его перед повтором",
        response.status().as_u16()
    );
    let value: Value = response
        .json()
        .context("Трекер вернул некорректный JSON; проверьте создание задачи")?;
    let url = provider(config.kind).issue_url(config, &value)?;
    // Commit the durable remote receipt before touching a revision-checked summary.
    let mut file = tempfile::NamedTempFile::new_in(meeting.path())?;
    file.write_all(&serde_json::to_vec(&Receipt {
        url: Some(url.clone()),
    })?)?;
    file.as_file().sync_all()?;
    file.persist(&receipt_path).map_err(|e| e.error)?;
    meeting.link_task_issue(&preview.task, &url)?;
    library.reindex_incremental()?;
    Ok(url)
}

#[cfg(test)]
#[path = "trackers_tests.rs"]
mod tests;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct KaitenChoice {
    pub id: u64,
    #[serde(default)]
    pub title: String,
}
#[derive(Clone, Debug, Default)]
pub struct KaitenDestinations {
    pub spaces: Vec<KaitenChoice>,
    pub boards: Vec<KaitenChoice>,
    pub columns: Vec<KaitenChoice>,
    pub lanes: Vec<KaitenChoice>,
}
/// Explicit settings action. Only account structure is fetched, no meeting data.
pub fn kaiten_destinations(
    config: &TrackerConfig,
    store: &dyn CredentialStore,
) -> Result<KaitenDestinations> {
    let root = base(config)?;
    let token = store.read(&format!("tracker/{}", config.credential_id))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let get = |path: &str| -> Result<Value> {
        let response = client
            .get(format!("{root}/api/latest/{path}"))
            .bearer_auth(&token)
            .send()
            .map_err(|_| anyhow::anyhow!("Не удалось получить структуру Kaiten"))?;
        ensure!(
            response.status().is_success(),
            "Kaiten: HTTP {}",
            response.status().as_u16()
        );
        Ok(response.json()?)
    };
    let mut result = KaitenDestinations::default();
    // Spaces are paginated on current Kaiten versions. Bound a broken server.
    for page in 0..100 {
        let spaces: Vec<KaitenChoice> =
            serde_json::from_value(get(&format!("spaces?limit=100&offset={}", page * 100))?)?;
        let complete = spaces.len() < 100;
        result.spaces.extend(spaces);
        if complete {
            break;
        }
        ensure!(
            page < 99,
            "Слишком много пространств Kaiten; задайте ID вручную"
        );
    }
    if !config.space_id.is_empty() {
        let id = numeric(&config.space_id, "space_id")?;
        result.boards = serde_json::from_value(get(&format!("spaces/{id}/boards"))?)?;
    }
    if !config.board_id.is_empty() {
        let id = numeric(&config.board_id, "board_id")?;
        let board = get(&format!("boards/{id}"))?;
        result.columns = serde_json::from_value(board["columns"].clone())?;
        result.lanes = serde_json::from_value(board["lanes"].clone())?;
    }
    Ok(result)
}
