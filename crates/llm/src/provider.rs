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

/// A completion returns the full text and calls on_text with incremental deltas.
/// Callbacks can receive partial text before an error; persist only on success.
pub trait Provider: Send + Sync {
    fn config(&self) -> &ProviderConfig;
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
}

impl HttpProvider {
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
                "LLM HTTP request failed (check connectivity, TLS, timeout and API key header)"
            )
        })?;
        let status = response.status();
        // Error bodies may echo private inputs or credentials; never include them.
        ensure!(
            status.is_success(),
            "LLM provider returned HTTP {} {}; check credentials, model, endpoint and provider availability",
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

    fn complete(
        &self,
        request: &CompletionRequest,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let text = match self.config.kind {
            ProviderKind::Openai => {
                let response = self.response(json!({
                    "model": self.config.model,
                    "messages": [
                        {"role": "system", "content": request.system},
                        {"role": "user", "content": request.user}
                    ],
                    "max_tokens": request.max_output_tokens,
                    "stream": true
                }))?;
                if response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("text/event-stream"))
                {
                    read_sse(response, on_text)?
                } else {
                    // Some local OpenAI-compatible servers ignore stream=true.
                    let value: Value = response.json().context("invalid OpenAI JSON response")?;
                    ensure!(
                        value["choices"][0]["finish_reason"] != "length",
                        "LLM output reached the token limit"
                    );
                    let text = value["choices"][0]["message"]["content"]
                        .as_str()
                        .context("OpenAI response has no message text")?
                        .to_owned();
                    on_text(&text);
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
                ensure!(
                    value["stop_reason"] != "max_tokens",
                    "LLM output reached the token limit"
                );
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
                text
            }
            _ => unreachable!("HTTP provider"),
        };
        ensure!(!text.trim().is_empty(), "LLM returned an empty completion");
        Ok(text)
    }
}

fn read_sse(response: Response, on_text: &mut dyn FnMut(&str)) -> Result<String> {
    let mut text = String::new();
    let mut event = String::new();
    let mut finished = false;
    for line in BufReader::new(response)
        .lines()
        .chain(std::iter::once(Ok(String::new())))
    {
        let line = line.context("cannot read OpenAI event stream")?;
        if let Some(data) = line.strip_prefix("data:") {
            if !event.is_empty() {
                event.push('\n');
            }
            event.push_str(data.strip_prefix(' ').unwrap_or(data));
        } else if line.is_empty() && !event.is_empty() {
            if event.trim() == "[DONE]" {
                return Ok(text);
            }
            let value: Value = serde_json::from_str(&event).context("invalid OpenAI SSE event")?;
            ensure!(
                value.get("error").is_none(),
                "OpenAI stream reported a provider error"
            );
            if let Some(choices) = value["choices"].as_array() {
                for choice in choices {
                    ensure!(
                        choice["finish_reason"] != "length",
                        "LLM output reached the token limit"
                    );
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
