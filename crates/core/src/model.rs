use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingMeta {
    pub id: Uuid,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub app: Option<String>,
    pub mode: Mode,
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// `None` keeps audio indefinitely; a timestamp at/before now expires it.
    pub audio_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_issues: Vec<CaptureIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_event: Option<crate::calendar::EventLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SummaryStamp>,
}

impl MeetingMeta {
    /// Create a local meeting with a UUID v7 and seven-day audio retention.
    pub fn new(title: impl Into<String>, started_at: DateTime<Utc>) -> Self {
        Self {
            calendar_event: None,
            summary: None,
            capture_issues: Vec::new(),
            id: Uuid::now_v7(),
            title: title.into(),
            started_at,
            ended_at: None,
            app: None,
            mode: Mode::Local,
            template: None,
            folder: None,
            tags: Vec::new(),
            audio_expires_at: started_at.checked_add_signed(TimeDelta::days(7)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Local,
    Hybrid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpeakerId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Mic,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Live,
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub id: Uuid,
    pub speaker: SpeakerId,
    pub channel: Channel,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub source: Source,
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: Uuid,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    /// Speaker ID to source recording ID; no remote transcript text.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ktalk_sources: std::collections::BTreeMap<String, String>,
    pub version: u32,
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<Bookmark>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub speaker_names: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub speaker_suggestions: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_speaker: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub speaker_voice_ids: std::collections::BTreeMap<String, String>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            ktalk_sources: Default::default(),
            segments: Vec::new(),
            bookmarks: Vec::new(),
            speaker_names: Default::default(),
            speaker_suggestions: Default::default(),
            self_speaker: None,
            speaker_voice_ids: Default::default(),
        }
    }
}

impl Transcript {
    /// Prefer a segment containing the time, otherwise the nearest boundary.
    pub fn segment_at(&self, at_ms: u64) -> Option<&Segment> {
        self.segments.iter().min_by_key(|s| {
            s.start_ms
                .saturating_sub(at_ms)
                .max(at_ms.saturating_sub(s.end_ms))
        })
    }

    pub fn speaker_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.speaker_names.get(id).map(String::as_str).unwrap_or(id)
    }

    pub fn rename_speaker(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.speaker_names.get(id).is_some_and(|n| n == name) {
            return false;
        }
        self.speaker_names.insert(id.into(), name.into());
        self.speaker_suggestions.remove(id);
        true
    }

    pub fn named_text(&self) -> String {
        self.segments
            .iter()
            .map(|s| format!("{}: {}", self.speaker_name(&s.speaker.0), s.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Decode the schema version before reading segments, so future schemas fail clearly.
/// Version 1 needs no migration; future migrations will be applied here.
pub fn migrate_transcript(value: serde_json::Value) -> Result<Transcript> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let version = Version::deserialize(&value)?.version;
    if version != SCHEMA_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    Ok(serde_json::from_value(value)?)
}

/// A channel could not be captured continuously from this offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureIssue {
    pub channel: Channel,
    pub from_ms: u64,
    pub reason: String,
}

/// Content hashes detect both stale evidence and user changes without mtime races.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryStamp {
    pub input: String,
    pub enhanced: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_hash: Option<String>,
}
impl SummaryStamp {
    pub fn new(notes: &str, transcript: &Transcript, template: &str, enhanced: &str) -> Self {
        use sha2::{Digest, Sha256};
        let input =
            serde_json::to_vec(&(notes, transcript, template)).expect("serializable summary input");
        Self {
            input: format!("{:x}", Sha256::digest(input)),
            enhanced: Self::digest(enhanced),
            notes_hash: Some(Self::digest(notes)),
            transcript_hash: Some(Self::digest(
                &serde_json::to_string(transcript).expect("serializable transcript"),
            )),
            template_hash: Some(Self::digest(template)),
        }
    }
    fn digest(text: &str) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(text.as_bytes()))
    }
    pub fn edited(&self, enhanced: &str) -> bool {
        self.enhanced != Self::digest(enhanced)
    }
    pub fn stale(&self, notes: &str, transcript: &Transcript, template: &str) -> bool {
        self.input != Self::new(notes, transcript, template, "").input
    }
    /// Component hashes explain a stale decision; only `input` decides staleness.
    pub fn changed_parts(
        &self,
        notes: &str,
        transcript: &Transcript,
        template: &str,
    ) -> Option<Vec<&'static str>> {
        let (Some(old_notes), Some(old_transcript), Some(old_template)) =
            (&self.notes_hash, &self.transcript_hash, &self.template_hash)
        else {
            return None;
        };
        let current = Self::new(notes, transcript, template, "");
        Some(
            [
                (Some(old_notes), current.notes_hash.as_ref(), "заметки"),
                (
                    Some(old_transcript),
                    current.transcript_hash.as_ref(),
                    "транскрипт",
                ),
                (Some(old_template), current.template_hash.as_ref(), "шаблон"),
            ]
            .into_iter()
            .filter_map(|(old, new, label)| (old != new).then_some(label))
            .collect(),
        )
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    #[test]
    fn component_hashes_explain_changes_but_do_not_decide_staleness() {
        let transcript = Transcript::default();
        let mut stamp = SummaryStamp::new("notes", &transcript, "general", "summary");
        assert_eq!(
            stamp.changed_parts("edited", &transcript, "general"),
            Some(vec!["заметки"])
        );
        let mut edited = transcript.clone();
        edited
            .speaker_names
            .insert("speaker_1".into(), "Анна".into());
        assert_eq!(
            stamp.changed_parts("notes", &edited, "general"),
            Some(vec!["транскрипт"])
        );
        assert_eq!(
            stamp.changed_parts("notes", &transcript, "standup"),
            Some(vec!["шаблон"])
        );
        assert_eq!(
            stamp.changed_parts("edited", &edited, "standup"),
            Some(vec!["заметки", "транскрипт", "шаблон"])
        );
        stamp.notes_hash = Some("different explanatory hash".into());
        assert!(!stamp.stale("notes", &transcript, "general"));
        stamp.input = "different decision hash".into();
        assert!(stamp.stale("notes", &transcript, "general"));
    }
    #[test]
    fn legacy_stamps_read_without_rewriting_and_unknown_fields_are_accepted() {
        let transcript = Transcript::default();
        let current = SummaryStamp::new("notes", &transcript, "general", "summary");
        let legacy = serde_json::json!({"input":current.input,"enhanced":current.enhanced});
        let old: SummaryStamp = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(old.notes_hash, None);
        assert_eq!(old.changed_parts("edited", &transcript, "general"), None);
        assert_eq!(serde_json::to_value(&old).unwrap(), legacy);
        let root = tempfile::tempdir().unwrap();
        let library = crate::Library::open(root.path()).unwrap();
        let mut meta = MeetingMeta::new("Совместимость", Utc::now());
        meta.summary = Some(old);
        let meeting = library.create_meeting(meta).unwrap();
        let path = meeting.path().join("meta.yaml");
        let original = std::fs::read(&path).unwrap();
        meeting.read_meta().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), original);
        #[derive(Deserialize)]
        struct LegacyStamp {
            input: String,
            enhanced: String,
        }
        #[derive(Deserialize)]
        struct LegacyMeta {
            summary: LegacyStamp,
        }
        let mut modern = meeting.read_meta().unwrap();
        modern.summary = Some(current.clone());
        let mut value = serde_json::to_value(modern).unwrap();
        value["future_field"] = serde_json::json!(true);
        value["summary"]["future_field"] = serde_json::json!(true);
        let parsed: MeetingMeta = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed.summary, Some(current.clone()));
        let older: LegacyMeta = serde_json::from_value(value).unwrap();
        assert_eq!(older.summary.input, current.input);
        assert_eq!(older.summary.enhanced, current.enhanced);
    }
    #[test]
    fn stamp_detects_edits_and_survives_metadata_roundtrip() {
        let transcript = Transcript::default();
        let stamp = SummaryStamp::new("notes", &transcript, "general", "summary");
        assert!(!stamp.stale("notes", &transcript, "general"));
        assert!(stamp.stale("edited notes", &transcript, "general"));
        assert!(stamp.stale("notes", &transcript, "standup"));
        let mut changed = transcript.clone();
        changed.segments.push(Segment {
            id: Uuid::now_v7(),
            speaker: SpeakerId("me".into()),
            channel: Channel::Mic,
            start_ms: 0,
            end_ms: 100,
            text: "edited".into(),
            source: Source::Final,
            edited: true,
            lang: None,
        });
        assert!(stamp.stale("notes", &changed, "general"));
        assert!(!stamp.edited("summary"));
        assert!(stamp.edited("user summary"));
        let mut meta =
            MeetingMeta::new("test", DateTime::from_timestamp(1_800_000_000, 0).unwrap());
        let old = serde_yaml::to_string(&meta).unwrap();
        assert!(!old.contains("summary:"));
        assert!(
            serde_yaml::from_str::<MeetingMeta>(&old)
                .unwrap()
                .summary
                .is_none()
        );
        meta.summary = Some(stamp);
        assert_eq!(
            serde_yaml::from_str::<MeetingMeta>(&serde_yaml::to_string(&meta).unwrap()).unwrap(),
            meta
        );
    }
}
