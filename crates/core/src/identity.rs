//! Shared optional identity from app.toml; unrelated settings remain opaque.
use std::io;

pub fn configured_user_name() -> io::Result<String> {
    configured_identity().map(|(name, _)| name)
}
pub(crate) fn configured_identity() -> io::Result<(String, String)> {
    let prefs = crate::app_config::AppSettings::load()?;
    Ok((prefs.user_name, prefs.user_aliases))
}

impl crate::Transcript {
    /// Explicit names win over the configured fallback, including imported self.
    pub fn default_self_name(&mut self, name: &str) -> bool {
        if name.trim().is_empty() {
            return false;
        }
        let speaker = self.self_speaker.clone().unwrap_or_else(|| "me".into());
        if let std::collections::btree_map::Entry::Vacant(entry) = self.speaker_names.entry(speaker)
        {
            entry.insert(name.trim().into());
            true
        } else {
            false
        }
    }
}
