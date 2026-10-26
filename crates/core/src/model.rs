use chrono::{DateTime, Utc};
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
}

impl MeetingMeta {
    pub fn new(title: impl Into<String>, started_at: DateTime<Utc>) -> Self {
        Self {
            id: Uuid::now_v7(),
            title: title.into(),
            started_at,
            ended_at: None,
            app: None,
            mode: Mode::Local,
            template: None,
            audio_expires_at: None,
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
