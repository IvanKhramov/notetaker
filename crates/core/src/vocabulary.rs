//! Shared vocabulary for ASR correction and summary prompts.
use std::{fs, io, path::Path};

#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    entries: Vec<(String, Vec<String>)>,
}

impl Vocabulary {
    pub fn parse(text: &str) -> Self {
        let mut vocabulary = Self::default();
        vocabulary.merge(text);
        vocabulary
    }

    /// Library entries override global entries with the same canonical spelling.
    pub fn with_library(global: &str, library: &Path) -> io::Result<Self> {
        let mut vocabulary = Self::parse(global);
        match fs::read_to_string(library.join(".notetaker/vocabulary.txt")) {
            Ok(text) => vocabulary.merge(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(vocabulary)
    }

    /// Read only the optional vocabulary field from the app's shared config.
    pub fn load(library: &Path) -> io::Result<Self> {
        #[derive(Default, serde::Deserialize)]
        struct Config {
            #[serde(default)]
            vocabulary: String,
        }
        let config = directories::ProjectDirs::from("", "", "notetaker")
            .map(|dirs| dirs.config_dir().join("app.toml"));
        let global = match config.map(fs::read_to_string).transpose() {
            Ok(Some(text)) => {
                toml::from_str::<Config>(&text)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                    .vocabulary
            }
            Ok(None) => String::new(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        Self::with_library(&global, library)
    }

    fn merge(&mut self, text: &str) {
        for line in text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
        {
            let (term, aliases) = line.split_once('=').unwrap_or((line, ""));
            let term = term.trim();
            if term.is_empty()
                || !term
                    .chars()
                    .all(|c| c.is_alphanumeric() || c.is_whitespace() || c == '-')
            {
                continue;
            }
            self.entries
                .retain(|(name, _)| name.to_lowercase() != term.to_lowercase());
            self.entries.push((
                term.into(),
                aliases
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_lowercase)
                    .collect(),
            ));
        }
    }

    pub fn terms(&self) -> Vec<String> {
        self.entries.iter().map(|(term, _)| term.clone()).collect()
    }

    pub fn hotwords(&self) -> Vec<String> {
        self.entries
            .iter()
            .flat_map(|(term, aliases)| {
                std::iter::once(term.clone()).chain(aliases.iter().cloned())
            })
            .filter(|term| {
                term.chars()
                    .all(|c| c.is_alphanumeric() || c == ' ' || c == '-')
            })
            .collect()
    }

    pub fn prompt_context(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        format!(
            "\n\nПравильное написание терминов: {}",
            self.entries
                .iter()
                .map(|(term, aliases)| {
                    if aliases.is_empty() {
                        term.clone()
                    } else {
                        format!("{term} ({})", aliases.join(", "))
                    }
                })
                .collect::<Vec<_>>()
                .join("; ")
        )
    }

    pub fn correct(&self, text: &str) -> String {
        // Preserve punctuation and spacing verbatim. Aliases are explicit whole
        // tokens; no automatic transliteration or cross-script fuzzy matching.
        text.split_inclusive(|c: char| !c.is_alphanumeric())
            .map(|part| {
                let end = part
                    .find(|c: char| !c.is_alphanumeric())
                    .unwrap_or(part.len());
                let (word, suffix) = part.split_at(end);
                let lower = word.to_lowercase();
                let exact = self.entries.iter().rev().find(|(term, aliases)| {
                    term.to_lowercase() == lower || aliases.contains(&lower)
                });
                let replacement = if let Some((term, _)) = exact {
                    Some(term)
                } else if lower.chars().count() >= 5 {
                    let mut candidates = self.entries.iter().filter(|(term, _)| {
                        let target = term.to_lowercase();
                        target.chars().count() >= 5
                            && target.chars().all(char::is_alphabetic)
                            && same_script(&lower, &target)
                            && edit_one(&lower, &target)
                    });
                    let first = candidates.next();
                    if candidates.next().is_none() {
                        first.map(|(term, _)| term)
                    } else {
                        None
                    }
                } else {
                    None
                };
                format!("{}{suffix}", replacement.map_or(word, String::as_str))
            })
            .collect()
    }
}

fn same_script(a: &str, b: &str) -> bool {
    let latin = |s: &str| s.chars().all(|c| c.is_ascii_alphabetic());
    let cyrillic = |s: &str| s.chars().all(|c| ('а'..='я').contains(&c) || c == 'ё');
    (latin(a) && latin(b)) || (cyrillic(a) && cyrillic(b))
}

fn edit_one(a: &str, b: &str) -> bool {
    let a: Vec<_> = a.chars().collect();
    let b: Vec<_> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        if a.len() >= b.len() {
            i += 1;
        }
        if b.len() >= a.len() {
            j += 1;
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conservative_unicode_correction_and_aliases() {
        let vocabulary =
            Vocabulary::parse("Deckhouse\nKubernetes\nFlant\nwerf = верф, вёрф\nfesco\nИкрамов");
        assert_eq!(
            vocabulary.correct("deckhous, Kubernetees! ВЁРФ и верф; Икрамовв."),
            "Deckhouse, Kubernetes! werf и werf; Икрамов."
        );
        assert_eq!(
            vocabulary.correct("декхаус флант верфы Flan fast fesco123"),
            "декхаус флант верфы Flan fast fesco123"
        );
        assert_eq!(Vocabulary::parse("house\nmouse").correct("louse"), "louse");
        assert_eq!(Vocabulary::parse("werf").correct("вёрф"), "вёрф");
    }
    #[test]
    fn library_overrides_global_and_keeps_other_terms() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".notetaker")).unwrap();
        fs::write(
            dir.path().join(".notetaker/vocabulary.txt"),
            "WERF = вёрф\nKubernetes",
        )
        .unwrap();
        let vocabulary = Vocabulary::with_library("werf = верф\nFlant", dir.path()).unwrap();
        assert_eq!(vocabulary.correct("верф вёрф flant"), "верф WERF Flant");
        assert_eq!(vocabulary.terms(), ["Flant", "WERF", "Kubernetes"]);
        assert!(
            vocabulary
                .prompt_context()
                .contains("Правильное написание терминов:")
        );
    }
}
