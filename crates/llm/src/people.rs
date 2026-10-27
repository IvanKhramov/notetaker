//! Summary-only composition. Local evidence never enters a remote request.
use crate::{
    Cancellation, Config, Scenario, create_provider, drafts::compose, enhance::notes_without_images,
};
use anyhow::{Context, Result};
use notetaker_core::{Digest, Library, Mode, Person};

#[derive(Clone, Debug)]
pub struct PeopleBrief {
    pub text: String,
    pub warning: Option<String>,
}

pub fn person_brief(
    library: &Library,
    person: Person,
    recent: usize,
    config: Option<&Config>,
    cancel: Cancellation,
) -> Result<PeopleBrief> {
    let page = library.person_page(person, false)?;
    let meetings: Vec<_> = page.meetings.iter().take(recent.clamp(1, 100)).collect();
    let mode = if meetings.iter().any(|m| m.meta.mode == Mode::Local) {
        Mode::Local
    } else {
        Mode::Hybrid
    };
    let evidence = meetings
        .iter()
        .map(|m| {
            format!(
                "### {} ({})\n{}",
                m.meta.title,
                m.meta.started_at.format("%d.%m.%Y"),
                notes_without_images(&m.summary)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let instruction = format!(
        "Сводка по человеку «{}» на русском: темы, решения, обязательства и вопросы к следующей встрече. Сохрани источники и даты. Не делай выводов о личности, не выдумывай факты.",
        page.person.name
    );
    let generated = (|| -> Result<String> {
        anyhow::ensure!(!meetings.is_empty(), "Нет встреч");
        let selected = config.context("Модель не настроена")?.select_scenario(
            mode,
            Scenario::Summary,
            None,
        )?;
        let provider = create_provider(selected, mode, cancel.clone())?;
        compose(&instruction, &evidence, provider.as_ref(), &cancel)
    })();
    Ok(match generated {
        Ok(text) => PeopleBrief {
            text,
            warning: None,
        },
        Err(error) => PeopleBrief {
            text: evidence,
            warning: Some(format!("Показаны конспекты без LLM: {error:#}")),
        },
    })
}

pub fn summarize_digest(
    mut digest: Digest,
    config: Option<&Config>,
    cancel: Cancellation,
) -> Digest {
    // Deterministic sections contain local data only on disk/UI. The provider sees
    // only Hybrid summaries, never the rendered digest, task index or transcripts.
    let evidence = digest_evidence(&digest);
    let generated = (|| -> Result<String> {
        anyhow::ensure!(
            !evidence.trim().is_empty(),
            "Нет Hybrid-конспектов для обработки"
        );
        let selected = config.context("Модель не настроена")?.select_scenario(
            Mode::Hybrid,
            Scenario::Summary,
            None,
        )?;
        let provider = create_provider(selected, Mode::Hybrid, cancel.clone())?;
        compose(
            "Подготовь недельную сводку на русском: ключевые решения, задачи и сроки, итоги по каждому человеку. Сохраняй даты и названия источников. Не выдумывай факты.",
            &evidence,
            provider.as_ref(),
            &cancel,
        )
    })();
    match generated {
        Ok(text) => digest
            .markdown
            .push_str(&format!("\n## Сводка LLM\n\n{text}\n")),
        Err(error) => digest
            .markdown
            .push_str(&format!("\nБез сводки LLM: {error:#}\n")),
    }
    if digest.meetings.iter().any(|m| m.meta.mode == Mode::Local) {
        digest
            .markdown
            .push_str("\nLocal-встречи показаны списками и не передавались LLM.\n");
    }
    digest
}

fn digest_evidence(digest: &Digest) -> String {
    digest
        .meetings
        .iter()
        .filter(|m| m.meta.mode == Mode::Hybrid)
        .map(|m| {
            format!(
                "### {} ({})\n{}",
                m.meta.title,
                m.meta.started_at.format("%d.%m.%Y"),
                notes_without_images(&m.summary)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{DigestFilter, DigestPeriod, Library, MeetingMeta};
    #[test]
    fn local_person_requires_local_provider_and_digest_lists_local_only() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Secret", chrono::Utc::now()))
            .unwrap();
        let mut transcript = meeting.read_transcript().unwrap();
        transcript.rename_speaker("s1", "Анна");
        meeting.write_transcript(&transcript).unwrap();
        meeting.write_enhanced("SECRET решено").unwrap();
        let person = library.people(false, "", "").unwrap().pop().unwrap();

        let config = Config::parse("[defaults]\nlocal='cloud'\nhybrid='cloud'\n[[providers]]\nid='cloud'\nkind='openai'\nbase_url='https://example.invalid/v1'\nmodel='test'").unwrap();
        let brief =
            person_brief(&library, person, 10, Some(&config), Cancellation::default()).unwrap();
        assert!(brief.warning.unwrap().contains("loopback"));
        assert!(brief.text.contains("SECRET"));
        let digest = library
            .digest(
                DigestPeriod::week(chrono::Local::now(), false),
                &DigestFilter::default(),
                &chrono::Local,
                false,
            )
            .unwrap();
        let digest = summarize_digest(digest, Some(&config), Cancellation::default());
        assert!(digest.markdown.contains("Local-встречи"));
        assert!(!digest.markdown.contains("Сводка LLM"));
        let mut meta = MeetingMeta::new("PUBLIC", chrono::Utc::now());
        meta.mode = Mode::Hybrid;
        library
            .create_meeting(meta)
            .unwrap()
            .write_enhanced("PUBLIC SUMMARY")
            .unwrap();
        let digest = library
            .digest(
                DigestPeriod::week(chrono::Local::now(), false),
                &DigestFilter::default(),
                &chrono::Local,
                false,
            )
            .unwrap();
        let evidence = digest_evidence(&digest);
        assert!(evidence.contains("PUBLIC SUMMARY"));
        assert!(!evidence.contains("SECRET"));
        assert!(!evidence.contains("Secret"));
    }
}
