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
        if self.voice_rejected.contains(&speaker)
            || self.speaker_identity(&speaker).is_some()
            || !self.segments.is_empty()
                && !self.segments.iter().any(|segment| {
                    segment.speaker.0 == speaker
                        && (segment.channel == crate::Channel::Mic
                            || self.self_speaker.as_ref() == Some(&speaker))
                })
        {
            return false;
        }
        // A configured name is not evidence for an unconfirmed acoustic link.
        self.speaker_suggestions.remove(&speaker);
        self.speaker_voice_ids.remove(&speaker);
        self.speaker_names.insert(speaker, name.trim().into());
        true
    }
}
