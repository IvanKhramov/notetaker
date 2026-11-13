use std::{
    io::{BufRead, BufReader},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use notetaker_core::Mode;
use reqwest::{
    blocking::{Client, Response},
    redirect::Policy,
};
use serde_json::{Value, json};

use crate::{ProviderConfig, ProviderKind};

pub struct CompletionRequest {
    pub system: String,
    pub user: String,
    pub max_output_tokens: usize,
}

/// A token cutoff is recoverable, including when reasoning used the whole budget.
/// Display never includes the private completion text.
#[derive(Debug)]
pub struct OutputLimit {
    pub text: String,
}

impl std::fmt::Display for OutputLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Модель достигла лимита ответа. Увеличьте контекст модели или выберите другую модель и повторите запрос.")
    }
}

impl std::error::Error for OutputLimit {}

/// A completion returns the full text and calls on_text with incremental deltas.
/// Callbacks can receive partial text before an error; persist only on success.
/// Token cutoffs return OutputLimit with all available text for recovery.
pub trait Provider: Send + Sync {
    fn config(&self) -> &ProviderConfig;
    fn is_cancelled(&self) -> bool {
        false
    }
    fn complete(
        &self,
        request: &CompletionRequest,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String>;
}

pub struct HttpProvider {
    config: ProviderConfig,
    client: Client,
    endpoint: reqwest::Url,
    key: Option<String>,
    cancel: crate::Cancellation,
}

impl HttpProvider {
    pub fn with_cancellation(mut self, cancel: crate::Cancellation) -> Self {
        self.cancel = cancel;
        self
    }
    pub fn new(config: ProviderConfig, mode: Mode) -> Result<Self> {
        config.validate(mode)?;
        let key = crate::resolve_key(&config, &crate::OsCredentialStore)?;
        Self::build(config, mode, key)
    }

    pub fn with_key(config: ProviderConfig, mode: Mode, key: Option<String>) -> Result<Self> {
        Self::build_timeout(config, mode, key, Duration::from_secs(30))
    }

    fn build(config: ProviderConfig, mode: Mode, key: Option<String>) -> Result<Self> {
        Self::build_timeout(config, mode, key, Duration::from_secs(600))
    }

    fn build_timeout(
        config: ProviderConfig,
        mode: Mode,
        key: Option<String>,
        timeout: Duration,
    ) -> Result<Self> {
        ensure!(!config.kind.is_cli(), "Используйте CliProvider для CLI");
        let mut endpoint = config.validate(mode)?;
        let path = format!(
            "{}/{}",
            endpoint.path().trim_end_matches('/'),
            match config.kind {
                ProviderKind::Openai => "chat/completions",
                ProviderKind::Anthropic => "messages",
                _ => unreachable!("HTTP provider"),
            }
        );
        endpoint.set_path(&path);
        ensure!(
            key.as_ref().is_none_or(|key| !key.trim().is_empty()),
            "LLM API key environment variable is empty"
        );
        ensure!(
            config.kind != ProviderKind::Anthropic || key.is_some(),
            "Anthropic требует API-ключ"
        );
        let mut client = Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            // A loopback redirect must never leak a local-only meeting to a remote host.
            .redirect(Policy::none());
        if config.validate(Mode::Local).is_ok() {
            client = client.no_proxy().resolve_to_addrs(
                "localhost",
                &[
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
                ],
            );
        }
        Ok(Self {
            config,
            client: client.build().context("cannot create LLM HTTP client")?,
            endpoint,
            key,
            cancel: crate::Cancellation::default(),
        })
    }

    pub fn models(&self) -> Result<Vec<String>> {
        let mut url = self.config.validate(Mode::Hybrid)?;
        url.set_path(&format!("{}/models", url.path().trim_end_matches('/')));
        let mut request = self.client.get(url).timeout(Duration::from_secs(30));
        if let Some(key) = &self.key {
            request = if self.config.kind == ProviderKind::Anthropic {
                request
                    .header("anthropic-version", "2023-06-01")
                    .header("x-api-key", key)
            } else {
                request.bearer_auth(key)
            };
        }
        let response = request.send().map_err(|_| {
            anyhow::anyhow!("Не удалось загрузить модели; проверьте VPN, адрес и ключ")
        })?;
        ensure!(
            response.status().is_success(),
            "Список моделей: HTTP {}",
            response.status().as_u16()
        );
        parse_models(
            response
                .json()
                .context("Некорректный JSON списка моделей")?,
        )
    }

    fn response(&self, body: Value) -> Result<Response> {
        ensure!(!self.cancel.is_cancelled(), "Запрос отменён");
        let mut request = self.client.post(self.endpoint.clone()).json(&body);
        match self.config.kind {
            ProviderKind::Openai => {
                if let Some(key) = &self.key {
                    request = request.bearer_auth(key);
                }
            }
            ProviderKind::Anthropic => {
                request = request
                    .header("anthropic-version", "2023-06-01")
                    .header("x-api-key", self.key.as_deref().unwrap_or_default());
            }
            _ => unreachable!("HTTP provider"),
        }
        let response = request.send().map_err(|_| {
            anyhow::anyhow!(
                "Не удалось связаться с AI. Проверьте интернет, VPN, адрес и ключ в «Настройки → AI»."
            )
        })?;
        ensure!(!self.cancel.is_cancelled(), "Запрос отменён");
        let status = response.status();
        // Error bodies may echo private inputs or credentials; never include them.
        ensure!(
            status.is_success(),
            "AI ответил ошибкой {} {}. Проверьте ключ, модель и адрес в «Настройки → AI».",
            status.as_u16(),
            status.canonical_reason().unwrap_or("error")
        );
        Ok(response)
    }
}

impl Provider for HttpProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn complete(
        &self,
        request: &CompletionRequest,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let text = match self.config.kind {
            ProviderKind::Openai => {
                let mut body = json!({
                    "model": self.config.model,
                    "messages": [
                        {"role": "system", "content": request.system},
                        {"role": "user", "content": request.user}
                    ],
                    "stream": true
                });
                body[output_token_parameter(&self.config)] = json!(request.max_output_tokens);
                let response = self.response(body)?;
                if response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("text/event-stream"))
                {
                    read_sse(response, on_text, &self.cancel)?
                } else {
                    // Some local OpenAI-compatible servers ignore stream=true.
                    let value: Value = response.json().context("invalid OpenAI JSON response")?;
                    let limited = value["choices"][0]["finish_reason"] == "length";
                    let text = value["choices"][0]["message"]["content"]
                        .as_str()
                        .or_else(|| limited.then_some(""))
                        .context("OpenAI response has no message text")?
                        .to_owned();
                    on_text(&text);
                    ensure!(!self.cancel.is_cancelled(), "Запрос отменён");
                    if limited {
                        return Err(OutputLimit { text }.into());
                    }
                    text
                }
            }
            ProviderKind::Anthropic => {
                let response = self.response(json!({
                    "model": self.config.model,
                    "system": request.system,
                    "messages": [{"role": "user", "content": request.user}],
                    "max_tokens": request.max_output_tokens
                }))?;
                let value: Value = response.json().context("invalid Anthropic JSON response")?;
                let blocks = value["content"]
                    .as_array()
                    .context("Anthropic response has no content blocks")?;
                let mut text = String::new();
                for block in blocks {
                    if block["type"] == "text" {
                        text.push_str(
                            block["text"]
                                .as_str()
                                .context("invalid Anthropic text block")?,
                        );
                    }
                }
                on_text(&text);
                ensure!(!self.cancel.is_cancelled(), "Запрос отменён");
                if value["stop_reason"] == "max_tokens" {
                    return Err(OutputLimit { text }.into());
                }
                text
            }
            _ => unreachable!("HTTP provider"),
        };
        ensure!(!self.cancel.is_cancelled(), "Запрос отменён");
        ensure!(
            !text.trim().is_empty(),
            "Модель вернула пустой ответ. Выберите другую модель или увеличьте контекст и повторите запрос."
        );
        Ok(text)
    }
}

fn output_token_parameter(config: &ProviderConfig) -> &'static str {
    // OpenAI reasoning models reject max_tokens. Ollama and older compatible
    // servers document max_tokens, so do not switch arbitrary models globally.
    // OpenRouter accepts either field and routes it to the upstream provider.
    let model = config.model.rsplit('/').next().unwrap_or(&config.model);
    if model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("gpt-5")
        || reqwest::Url::parse(&config.base_url)
            .is_ok_and(|url| url.host_str() == Some("api.openai.com"))
    {
        "max_completion_tokens"
    } else {
        "max_tokens"
    }
}

fn read_sse(
    response: Response,
    on_text: &mut dyn FnMut(&str),
    cancel: &crate::Cancellation,
) -> Result<String> {
    let mut text = String::new();
    let mut event = String::new();
    let mut finished = false;
    let mut limited = false;
    for line in BufReader::new(response)
        .lines()
        .chain(std::iter::once(Ok(String::new())))
    {
        ensure!(!cancel.is_cancelled(), "Запрос отменён");
        let line = line.context("cannot read OpenAI event stream")?;
        if let Some(data) = line.strip_prefix("data:") {
            if !event.is_empty() {
                event.push('\n');
            }
            event.push_str(data.strip_prefix(' ').unwrap_or(data));
        } else if line.is_empty() && !event.is_empty() {
            if event.trim() == "[DONE]" {
                finished = true;
                break;
            }
            let value: Value = serde_json::from_str(&event).context("invalid OpenAI SSE event")?;
            ensure!(
                value.get("error").is_none(),
                "OpenAI stream reported a provider error"
            );
            if let Some(choices) = value["choices"].as_array() {
                for choice in choices {
                    limited |= choice["finish_reason"] == "length";
                    if !choice["finish_reason"].is_null() {
                        finished = true;
                    }
                    if let Some(delta) = choice["delta"]["content"].as_str() {
                        text.push_str(delta);
                        on_text(delta);
                    }
                }
            }
            event.clear();
        }
    }
    ensure!(!cancel.is_cancelled(), "Запрос отменён");
    if limited {
        return Err(OutputLimit { text }.into());
    }
    if !finished {
        bail!("OpenAI event stream ended before completion");
    }
    Ok(text)
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;

fn parse_models(value: Value) -> Result<Vec<String>> {
    let data = value["data"]
        .as_array()
        .context("В списке моделей нет массива data")?;
    let mut models: Vec<String> = data
        .iter()
        .filter_map(|model| model["id"].as_str())
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .collect();
    let mut seen = std::collections::HashSet::new();
    models.retain(|id| seen.insert(id.clone()));
    ensure!(!models.is_empty(), "Шлюз не вернул доступных моделей");
    Ok(models)
}
