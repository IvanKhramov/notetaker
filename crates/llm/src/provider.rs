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
        let key = config
            .api_key_env
            .as_ref()
            .map(|name| {
                std::env::var(name)
                    .context("LLM API key environment variable is missing or not Unicode")
            })
            .transpose()?;
        Self::build(config, mode, key)
    }

    fn build(config: ProviderConfig, mode: Mode, key: Option<String>) -> Result<Self> {
        let mut endpoint = config.validate(mode)?;
        let path = format!(
            "{}/{}",
            endpoint.path().trim_end_matches('/'),
            match config.kind {
                ProviderKind::Openai => "chat/completions",
                ProviderKind::Anthropic => "messages",
            }
        );
        endpoint.set_path(&path);
        ensure!(
            key.as_ref().is_none_or(|key| !key.trim().is_empty()),
            "LLM API key environment variable is empty"
        );
        ensure!(
            config.kind != ProviderKind::Anthropic || key.is_some(),
            "Anthropic requires api_key_env"
        );
        let mut client = Client::builder()
            .timeout(Duration::from_secs(300))
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
