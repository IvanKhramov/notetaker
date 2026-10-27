use std::{fs, sync::Mutex};

use anyhow::Result;
use chrono::{TimeZone, Utc};
use notetaker_core::{Channel, Library, MeetingMeta, Mode, Segment, Source, SpeakerId, Transcript};
use notetaker_llm::{
    CompletionRequest, Config, Provider, ProviderConfig, ProviderKind, Template, enhance,
    enhance_meeting, estimate_tokens,
};
use uuid::Uuid;

struct Fake {
    config: ProviderConfig,
    calls: Mutex<Vec<(String, String, usize)>>,
    fail: bool,
    verbose: bool,
}

impl Fake {
    fn new(context_tokens: usize) -> Self {
        Self {
            config: ProviderConfig {
                id: "fake".into(),
                kind: ProviderKind::Openai,
                base_url: "http://localhost:11434/v1".into(),
                model: "fake".into(),
                api_key_env: None,
                context_tokens,
            },
            calls: Mutex::new(Vec::new()),
            fail: false,
            verbose: false,
        }
    }
}

impl Provider for Fake {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }
    fn complete(
        &self,
        request: &CompletionRequest,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        self.calls.lock().unwrap().push((
            request.system.clone(),
            request.user.clone(),
            request.max_output_tokens,
        ));
        assert!(
            estimate_tokens(&request.system)
                + estimate_tokens(&request.user)
                + 64
                + request.max_output_tokens
                <= self.config.context_tokens
        );
        if self.fail {
            anyhow::bail!("mock failure");
        }
        let text = if self.verbose {
            request.user.clone()
        } else if request.system.contains("Produce the final enhanced") {
            "# Итоги\n- План сохранён".into()
        } else {
            "Decision: ship. Owner: me. Deadline: Friday.".into()
        };
        on_text(&text);
        Ok(text)
    }
}

fn meta() -> MeetingMeta {
    MeetingMeta::new(
        "Release planning",
        Utc.with_ymd_and_hms(2026, 10, 26, 15, 30, 0).unwrap(),
    )
}

fn transcript(text: &str) -> Transcript {
    Transcript {
        segments: vec![Segment {
            id: Uuid::now_v7(),
            speaker: SpeakerId("them".into()),
            channel: Channel::System,
            start_ms: 62_000,
            end_ms: 65_000,
            text: text.into(),
            source: Source::Final,
            edited: false,
        }],
        ..Transcript::default()
    }
}

#[test]
fn prompt_preserves_user_outline_metadata_speakers_and_language() {
    let provider = Fake::new(4096);
    let template = Template::builtin("general").unwrap();
    let notes = "## Главное\n- План релиза\n- Мой пункт без транскрипта";
    let mut deltas = String::new();
    let result = enhance(
        &meta(),
        notes,
        &transcript("Обсудим план"),
        &template,
        &provider,
        &mut |text| deltas.push_str(text),
    )
    .unwrap();
    assert_eq!(result, deltas);
    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.contains("preserve every point"));
    assert!(calls[0].0.contains("language of the meeting"));
    assert!(calls[0].1.contains(notes));
    assert!(calls[0].1.contains("Release planning"));
    assert!(calls[0].1.contains("2026-10-26T15:30:00+00:00"));
    assert!(calls[0].1.contains("[01:02] them: Обсудим план"));
}

#[test]
fn small_context_maps_and_recursively_reduces_without_losing_notes() {
    let provider = Fake::new(1024);
    let template = Template::builtin("general").unwrap();
    let notes = "- Must keep this user point";
    // Many chunks force an extra reduce level, even with short fake summaries.
    let mut deltas = String::new();
    let result = enhance(
        &meta(),
        notes,
        &transcript(&"Текст встречи 🚀 ".repeat(2500)),
        &template,
        &provider,
        &mut |text| deltas.push_str(text),
    )
    .unwrap();
    assert_eq!(result, deltas);
    let calls = provider.calls.lock().unwrap();
    assert!(calls.len() > 3);
    assert!(
        calls
            .iter()
            .any(|call| call.0.contains("Summarize this transcript excerpt"))
    );
    assert!(
        calls
            .iter()
            .any(|call| call.0.contains("Compress these intermediate"))
    );
    assert!(
        calls
            .last()
            .unwrap()
            .0
            .contains("Produce the final enhanced")
    );
    assert!(calls.iter().all(|call| call.1.contains(notes)));
    let map_calls: Vec<_> = calls
        .iter()
        .filter(|call| call.0.contains("Summarize this transcript excerpt"))
        .collect();
    let occurrences: usize = map_calls
        .iter()
        .map(|call| call.1.matches('🚀').count())
        .sum();
    assert_eq!(occurrences, 2500);
    assert!(
        map_calls
            .iter()
            .all(|call| call.1.contains("[01:02] them:"))
    );
}

#[test]
fn local_only_guard_also_prevents_custom_provider_invocation() {
    let mut provider = Fake::new(4096);
    provider.config.base_url = "https://api.example.invalid/v1".into();
    let error = enhance(
        &meta(),
        "notes",
        &Transcript::default(),
        &Template::builtin("general").unwrap(),
        &provider,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("local-only"));
    assert!(provider.calls.lock().unwrap().is_empty());
}

#[test]
fn oversized_notes_fail_before_provider_invocation() {
    let provider = Fake::new(1024);
    let error = enhance(
        &meta(),
        &"a".repeat(4000),
        &Transcript::default(),
        &Template::builtin("general").unwrap(),
        &provider,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("too small"));
    assert!(provider.calls.lock().unwrap().is_empty());
}

#[test]
fn nonshrinking_summaries_fail_with_bounded_cost() {
    let mut provider = Fake::new(1024);
    provider.verbose = true;
    let error = enhance(
        &meta(),
        "notes",
        &transcript(&"long transcript ".repeat(300)),
        &Template::builtin("general").unwrap(),
        &provider,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("did not shrink"));
}

#[test]
fn successful_result_is_written_and_failure_preserves_existing_enhanced() {
    let directory = tempfile::tempdir().unwrap();
    let library = Library::open(directory.path()).unwrap();
    let meeting = library.create_meeting(meta()).unwrap();
    meeting.write_notes("- My agenda").unwrap();
    meeting.write_transcript(&transcript("Let's ship")).unwrap();
    let mut provider = Fake::new(4096);
    let template = Template::builtin("general").unwrap();
    let result = enhance_meeting(&meeting, "general", &template, &provider, &mut |_| {}).unwrap();
    assert_eq!(meeting.read_enhanced().unwrap(), result.text);
    provider.fail = true;
    assert!(enhance_meeting(&meeting, "general", &template, &provider, &mut |_| {}).is_err());
    assert_eq!(meeting.read_enhanced().unwrap(), result.text);
}

#[test]
fn user_template_overrides_builtin_and_supports_frontmatter() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join(".templates")).unwrap();
    fs::write(directory.path().join(".templates/general.md"), "---\r\nname: Custom review\r\ndescription: Weekly review\r\n---\r\nKeep the agenda. Write in the language of the meeting.\r\n").unwrap();
    let template = Template::load(directory.path(), "general").unwrap();
    assert_eq!(template.name, "Custom review");
    assert_eq!(template.description, "Weekly review");
    assert!(template.instruction.starts_with("Keep the agenda."));
    assert!(Template::load(directory.path(), "../escape").is_err());
    assert!(Template::load(directory.path(), "missing").is_err());
    for name in ["one-on-one", "client-call", "standup"] {
        assert!(
            Template::load(directory.path(), name)
                .unwrap()
                .instruction
                .contains("language of the meeting")
        );
    }
    fs::write(
        directory.path().join(".templates/general.md"),
        "broken frontmatter",
    )
    .unwrap();
    assert!(Template::load(directory.path(), "general").is_err());
}

#[test]
fn config_selects_defaults_override_and_validates_local_urls() {
    let text = r#"
[defaults]
local = "ollama"
hybrid = "cloud"
[[providers]]
id = "ollama"
kind = "openai"
base_url = "http://localhost:11434/v1"
model = "local-model"
context_tokens = 8192
[[providers]]
id = "cloud"
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
model = "cloud-model"
api_key_env = "ANTHROPIC_API_KEY"
context_tokens = 200000
"#;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, text).unwrap();
    let config = Config::from_path(&path).unwrap();
    assert_eq!(config.select(Mode::Local, None).unwrap().id, "ollama");
    assert_eq!(config.select(Mode::Hybrid, None).unwrap().id, "cloud");
    assert_eq!(
        config.select(Mode::Hybrid, Some("ollama")).unwrap().id,
        "ollama"
    );
    assert!(config.select(Mode::Local, Some("cloud")).is_err());
    // A remote Local default loads; only Local meetings are refused.
    let remote = Config::parse(&text.replace("local = \"ollama\"", "local = \"cloud\"")).unwrap();
    assert!(remote.select(Mode::Local, None).is_err());
    assert_eq!(remote.select(Mode::Hybrid, None).unwrap().id, "cloud");
    assert!(config.select(Mode::Hybrid, Some("missing")).is_err());
    assert!(Config::parse(&text.replace("context_tokens = 8192", "context_tokens = 0")).is_err());
    assert!(Config::parse(&text.replace("id = \"cloud\"", "id = \"ollama\"")).is_err());
    assert!(
        !Config::parse("api_key = 'must-not-echo'")
            .unwrap_err()
            .to_string()
            .contains("must-not-echo")
    );
    let mut local = config.providers[0].clone();
    for base in [
        "http://127.0.0.1:8080/v1",
        "http://[::1]:8080/v1",
        "http://localhost:11434/v1",
    ] {
        local.base_url = base.into();
        assert!(local.validate(Mode::Local).is_ok());
    }
}

#[test]
fn saved_enhancement_survives_metadata_and_index_failures() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let meeting = library.create_meeting(meta()).unwrap();
    let provider = Fake::new(4096);
    let template = Template::builtin("general").unwrap();
    // Make the index inaccessible, even when running tests as root.
    fs::create_dir_all(library.index_path()).unwrap();
    let result = enhance_meeting(&meeting, "general", &template, &provider, &mut |_| {
        // Simulate a sync tool replacing metadata while the provider is running.
        fs::write(meeting.path().join("meta.yaml"), "invalid: [").unwrap();
    })
    .unwrap();
    assert_eq!(meeting.read_enhanced().unwrap(), result.text);
    assert_eq!(meeting.load_enhanced().unwrap().revision, result.revision);
    assert_eq!(result.warnings.len(), 2);
    assert!(result.warnings.iter().any(|s| s.contains("metadata")));
    assert!(result.warnings.iter().any(|s| s.contains("index")));
}
