//! Meeting files are the source of truth; the SQLite search index is disposable.
mod index;
mod model;
mod storage;

pub use index::Hit;
pub use model::{
    Channel, MeetingMeta, Mode, SCHEMA_VERSION, Segment, Source, SpeakerId, Transcript,
    migrate_transcript,
};
pub use storage::{Library, Meeting};

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
    #[error("invalid index data: {0}")]
    InvalidIndex(String),
}

pub type Result<T> = std::result::Result<T, Error>;
