//! Meeting files are the source of truth; the SQLite search index is disposable.
mod attachments;
mod brief;
pub mod folders;
mod people;
pub mod sharing;
pub use people::{PeopleEdits, Person, resolve_people};
mod relationships;
pub use relationships::{CarryOver, MeetingSummary, PersonPage, Series, decisions};
mod digest;
pub use digest::{Digest, DigestFilter, DigestPeriod};
pub mod calendar;
pub mod embedding;
mod export;
mod identity;
mod index;
mod semantic;
pub use identity::configured_user_name;
pub use semantic::{
    Anchor, Chunk, Embedder, SearchMode, SemanticProgress, dot_blob, meeting_chunks,
};
mod model;
mod paths;
pub use paths::{display_path, display_text};
mod speakers;
mod storage;
mod tasks;
pub use tasks::{Task, parse_tasks, person_matches, resolve_due};
mod vocabulary;
mod voices;
pub use export::{ExportFormat, format_timestamp};
pub use speakers::{MergeUndo, SpeakerStats, SpeakerTimeline, TimelineSegment};
pub use vocabulary::Vocabulary;
pub use voices::{MeetingVoices, SELF_THRESHOLD, VOICE_MODEL, Voice, VoiceLibrary, cosine};
pub use voices::{SpeakerEdit, SpeakerEditResult, SpeakerUndo};

pub use attachments::{attachment_image_path, attachment_references, markdown_images};
pub use index::Hit;
pub use model::{
    Bookmark, CaptureIssue, Channel, MeetingMeta, Mode, SCHEMA_VERSION, Segment, Source, SpeakerId,
    SummaryStamp, Transcript, migrate_transcript,
};
pub use storage::{Library, Meeting, Revision, Versioned};

/// Errors from meeting storage, schema validation, or the rebuildable index.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid meeting YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("invalid transcript JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported transcript schema version {0}")]
    UnsupportedVersion(u32),
    #[error("SQLite index operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Conflict: {path} changed on disk; local version saved to {local}")]
    Conflict {
        path: std::path::PathBuf,
        local: std::path::PathBuf,
    },
    #[error("Поиск по смыслу: {0}")]
    Embedding(String),
    #[error("invalid index data: {0}")]
    InvalidIndex(String),
    #[error("task edit failed: {0}")]
    TaskEdit(String),
    #[error("speaker edit failed: {0}")]
    SpeakerEdit(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod relationships_tests;
