use std::{fs, path::Path};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct Template {
    pub name: String,
    pub description: String,
    pub instruction: String,
}

#[derive(Deserialize)]
struct Header {
    name: String,
    description: String,
}

impl Template {
    /// User templates take precedence over built-ins; name is a filename stem.
    pub fn load(library: &Path, name: &str) -> Result<Self> {
        ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "template name must contain only letters, digits, '-' or '_'"
        );
        let path = library.join(".templates").join(format!("{name}.md"));
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::builtin(name).context("unknown template; expected general, one-on-one, client-call, standup or .templates/<name>.md")
            }
            Err(error) => Err(error).context("cannot read user template"),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let text = text.replace("\r\n", "\n");
        let text = text
            .strip_prefix("---\n")
            .context("template must start with YAML frontmatter")?;
        let Some((header, body)) = text.split_once("\n---\n") else {
            bail!("template has no closing frontmatter delimiter");
        };
        let header: Header =
            serde_yaml::from_str(header).context("invalid template frontmatter")?;
        ensure!(
            !header.name.trim().is_empty()
                && !header.description.trim().is_empty()
                && !body.trim().is_empty(),
            "template name, description and instruction must not be empty"
        );
        Ok(Self {
            name: header.name,
            description: header.description,
            instruction: body.trim().to_owned(),
        })
    }

    pub fn builtin(name: &str) -> Option<Self> {
        let (description, instruction) = match name {
            "general" => (
                "General meeting notes",
                "Organize the notes into Summary, Discussion, Decisions, and Action items. Preserve the user's headings where possible. Include owners and deadlines only when stated.",
            ),
            "one-on-one" => (
                "One-on-one conversation",
                "Organize the notes into Check-in, Topics discussed, Feedback, Growth and support, and Next steps. Preserve the user's agenda and distinguish each person's commitments.",
            ),
            "client-call" => (
                "Client conversation",
                "Organize the notes into Client goals, Requirements and constraints, Discussion, Decisions, Open questions, and Next steps. Distinguish requests from agreed commitments.",
            ),
            "standup" => (
                "Daily team standup",
                "Organize the notes by participant with Progress, Plans, and Blockers, followed by Follow-ups. Preserve the user's priorities and do not invent participant names.",
            ),
            _ => return None,
        };
        Some(Self {
            name: name.into(),
            description: description.into(),
            instruction: format!(
                "{instruction} Write the notes in the transcript language, Russian by default. Format every action item as `- [ ] Задача - Ответственный - срок` (translate labels into the transcript language). If an owner or deadline is missing, use `не указан` in that field; never invent them."
            ),
        })
    }
}
