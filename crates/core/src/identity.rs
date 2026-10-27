//! Shared optional identity from app.toml; unrelated settings remain opaque.
use std::{fs, io};

pub fn configured_user_name() -> io::Result<String> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct Preferences {
        user_name: String,
    }
    let Some(dirs) = directories::ProjectDirs::from("", "", "notetaker") else {
        return Ok(String::new());
    };
    match fs::read_to_string(dirs.config_dir().join("app.toml")) {
        Ok(text) => toml::from_str::<Preferences>(&text)
            .map(|prefs| prefs.user_name)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
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
