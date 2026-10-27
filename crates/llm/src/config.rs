use std::{
    collections::HashSet,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use directories::ProjectDirs;
use notetaker_core::Mode;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub defaults: Defaults,
    pub providers: Vec<ProviderConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub local: String,
    pub hybrid: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Openai,
    Anthropic,
    ClaudeCli,
    CodexCli,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: ProviderKind,
    /// API root, including /v1 where required (never a completions/messages URL).
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// Name of an environment variable, never an API key itself.
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub key_source: KeySource,
    #[serde(default = "default_context")]
    pub context_tokens: usize,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    #[default]
    Env,
    Credential,
}

fn default_context() -> usize {
    150_000
}

impl ProviderKind {
    pub fn is_cli(self) -> bool {
        matches!(self, Self::ClaudeCli | Self::CodexCli)
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "notetaker")
            .context("cannot locate the notetaker configuration directory")?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        Self::from_path(&Self::path()?)
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read {}; see docs/llm.md", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        // Do not echo TOML excerpts: a user may accidentally paste a secret.
        let config: Self = toml::from_str(text).map_err(|_| {
            anyhow::anyhow!("invalid LLM config.toml; see docs/llm.md for the schema")
        })?;
        let mut ids = HashSet::new();
        for provider in &config.providers {
            ensure!(
                ids.insert(&provider.id),
                "duplicate provider id in config.toml"
            );
            provider.validate(Mode::Hybrid)?;
        }
        // A remote Local default only fails Local meetings at select(); it must
        // not block Hybrid meetings from loading the same config.
        for id in [&config.defaults.local, &config.defaults.hybrid] {
            ensure!(
                config.providers.iter().any(|provider| &provider.id == id),
                "selected LLM provider is not configured"
            );
        }
        Ok(config)
    }

    pub fn select(&self, mode: Mode, id: Option<&str>) -> Result<&ProviderConfig> {
        let id = id.unwrap_or(match mode {
            Mode::Local => &self.defaults.local,
            Mode::Hybrid => &self.defaults.hybrid,
        });
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id == id)
            .context("selected LLM provider is not configured")?;
        provider.validate(mode)?;
        Ok(provider)
    }
}

impl ProviderConfig {
    /// Validate privacy before reading a key, constructing a client, or doing I/O.
    pub fn validate(&self, mode: Mode) -> Result<reqwest::Url> {
        ensure!(
            !self.id.trim().is_empty() && !self.id.chars().any(char::is_control),
            "LLM provider id must not be empty"
        );
        ensure!(
            self.context_tokens > 0,
            "LLM context_tokens must be positive"
        );
        if self.kind.is_cli() {
            ensure!(
                mode != Mode::Local,
                "Режим Local запрещает облачные CLI-провайдеры. Выберите Hybrid для этой встречи."
            );
            ensure!(
                self.key_source == KeySource::Env && self.api_key_env.is_none(),
                "CLI использует собственный вход; ключи не настраиваются"
            );
            return Ok(reqwest::Url::parse("https://cli.invalid").expect("static URL"));
        }
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| anyhow::anyhow!("invalid LLM base_url"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "LLM base_url must be an HTTP(S) URL with a host"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "LLM base_url must not contain credentials, a query, or a fragment"
        );
        if mode == Mode::Local {
            let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
            let loopback = host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
            if !loopback {
                bail!(
                    "local-only meeting requires a loopback LLM provider (localhost, 127.0.0.1 or ::1)"
                );
            }
        }
        ensure!(
            !self.model.trim().is_empty(),
            "LLM provider model must not be empty"
        );
        ensure!(
            self.api_key_env
                .as_ref()
                .is_none_or(|name| !name.is_empty()),
            "api_key_env must name an environment variable"
        );
        Ok(url)
    }
}
