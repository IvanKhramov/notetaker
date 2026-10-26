//! Blocking LLM providers, templates, and note enhancement with bounded context.
//!
//! Load [`Config`], select a [`ProviderConfig`], construct [`HttpProvider`] for the
//! meeting mode, then call [`enhance_meeting`]. Callbacks receive final Markdown
//! deltas only; intermediate map/reduce summaries are kept internal.
mod config;
mod enhance;
mod provider;
mod template;

pub use config::{Config, Defaults, ProviderConfig, ProviderKind};
pub use enhance::{enhance, enhance_meeting, estimate_tokens};
pub use provider::{CompletionRequest, HttpProvider, Provider};
pub use template::Template;
