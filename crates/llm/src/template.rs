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
            "followup-team" => (
                "Сообщение в команду",
                "Напиши короткое сообщение в команду на русском. Только решения и следующие действия: задача, ответственный, срок. Без вступления, благодарностей и воды. Не выдумывай отсутствующие сроки и имена.",
            ),
            "followup-client" => (
                "Сообщение клиенту",
                "Напиши короткое сообщение клиенту на русском. Только согласованные решения, действия, ответственные и сроки. Исключи внутренние замечания, оценки, внутренние обсуждения и строки с пометкой internal или внутреннее. Без воды и выдуманных обязательств.",
            ),
            "general" => (
                "General meeting notes",
                "Для русского транскрипта используй заголовки: Кратко, Обсуждение, Решения, Задачи, Открытые вопросы. Только для английского транскрипта: Summary, Discussion, Decisions, Action items, Open questions. Сохраняй пользовательский план. Ответственных и сроки включай только из данных встречи.",
            ),
            "one-on-one" => (
                "One-on-one conversation",
                "Для русского транскрипта используй заголовки: Как дела, Обсуждение, Обратная связь, Развитие и поддержка, Задачи. Только для английского транскрипта: Check-in, Discussion, Feedback, Growth and support, Next steps. Сохраняй повестку пользователя и различай обязательства участников.",
            ),
            "client-call" => (
                "Client conversation",
                "Для русского транскрипта используй заголовки: Цели клиента, Требования и ограничения, Обсуждение, Решения, Открытые вопросы, Задачи. Только для английского транскрипта: Client goals, Requirements and constraints, Discussion, Decisions, Open questions, Next steps. Различай запросы и согласованные обязательства.",
            ),
            "standup" => (
                "Daily team standup",
                "Группируй по участникам. Для русского транскрипта используй заголовки: Сделано, Планы, Блокеры, Задачи. Только для английского транскрипта: Progress, Plans, Blockers, Follow-ups. Сохраняй приоритеты пользователя, не выдумывай имена.",
            ),
            _ => return None,
        };
        Some(Self {
            name: name.into(),
            description: description.into(),
            instruction: if name.starts_with("followup-") {
                instruction.into()
            } else {
                format!(
                    "{instruction} Если есть пользовательские закладки, добавь раздел «Важные моменты» с метками времени и контекстом. Write the notes in the transcript language, Russian by default. Format every action item as `- [ ] Задача - Ответственный - срок` (translate labels into the transcript language). If an owner or deadline is missing, use `не указан` in that field; never invent them."
                )
            },
        })
    }
}

#[cfg(test)]
mod heading_tests {
    #[test]
    fn summary_headings_follow_transcript_language() {
        for id in ["general", "one-on-one", "client-call", "standup"] {
            let instruction = super::Template::builtin(id).unwrap().instruction;
            assert!(instruction.contains("Для русского транскрипта"));
            assert!(instruction.contains("Только для английского транскрипта"));
            assert!(instruction.contains("Задачи"));
        }
    }
}
