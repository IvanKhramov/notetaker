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
    /// `None` keeps audio indefinitely; a timestamp at/before now expires it.
    pub audio_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_issues: Vec<CaptureIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SummaryStamp>,
}

impl MeetingMeta {
    /// Create a local meeting with a UUID v7 and seven-day audio retention.
    pub fn new(title: impl Into<String>, started_at: DateTime<Utc>) -> Self {
        Self {
            summary: None,
            capture_issues: Vec::new(),
            id: Uuid::now_v7(),
            title: title.into(),
            started_at,
            ended_at: None,
            app: None,
            mode: Mode::Local,
            template: None,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub version: u32,
    pub segments: Vec<Segment>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            segments: Vec::new(),
        }
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
}
impl SummaryStamp {
    pub fn new(notes: &str, transcript: &Transcript, template: &str, enhanced: &str) -> Self {
        use sha2::{Digest, Sha256};
        let input =
            serde_json::to_vec(&(notes, transcript, template)).expect("serializable summary input");
        Self {
            input: format!("{:x}", Sha256::digest(input)),
            enhanced: Self::digest(enhanced),
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
}

#[cfg(test)]
mod summary_tests {
    use super::*;
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
