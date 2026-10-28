//! Blocking LLM providers, templates, and note enhancement with bounded context.
//!
//! Load [`Config`], select a [`ProviderConfig`], call [`create_provider`] for the
//! meeting mode, then call [`enhance_meeting`]. Callbacks receive final Markdown
//! deltas only; intermediate map/reduce summaries are kept internal.
pub mod calendar;
mod chat;
mod cli;
mod config;
mod credentials;
mod drafts;
pub mod live;
mod people;
pub mod search;
pub mod trackers;
pub use people::{PeopleBrief, person_brief, person_brief_in_folder, summarize_digest};
mod enhance;
pub use drafts::{Brief, FollowupKind, brief, followup, render_followup, strip_internal};
mod provider;
mod template;

pub use chat::{ChatResult, ChatTurn, Citation, ask};
pub use config::{
    Config, Defaults, KeySource, ModelChoice, ProviderConfig, ProviderKind, Scenario, Scenarios,
};
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
        Ok(Box::new(
            HttpProvider::new(config, mode)?.with_cancellation(cancel),
        ))
    }
}
