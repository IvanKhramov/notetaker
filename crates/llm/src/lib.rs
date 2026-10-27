//! Blocking LLM providers, templates, and note enhancement with bounded context.
//!
//! Load [`Config`], select a [`ProviderConfig`], call [`create_provider`] for the
//! meeting mode, then call [`enhance_meeting`]. Callbacks receive final Markdown
//! deltas only; intermediate map/reduce summaries are kept internal.
mod cli;
mod config;
mod credentials;
mod enhance;
mod provider;
mod template;

pub use config::{Config, Defaults, KeySource, ProviderConfig, ProviderKind};
pub use enhance::{
    EnhanceResult, enhance, enhance_meeting, enhance_meeting_if_unedited, estimate_tokens,
};
pub use provider::{CompletionRequest, HttpProvider, Provider};
pub use template::Template;

pub use cli::{Cancellation, CliProvider, find_cli, open_login};
pub use credentials::{CredentialStore, OsCredentialStore, resolve_key};

pub fn create_provider(
    config: ProviderConfig,
    mode: notetaker_core::Mode,
    cancel: Cancellation,
) -> anyhow::Result<Box<dyn Provider>> {
    if config.kind.is_cli() {
        Ok(Box::new(CliProvider::new(config, mode, cancel)?))
    } else {
        Ok(Box::new(HttpProvider::new(config, mode)?))
    }
}
