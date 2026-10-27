use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::{Error, Library, Meeting, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub meeting_id: Uuid,
    pub path: PathBuf,
    pub title: String,
    /// Plain text, with matching words surrounded by `[` and `]`.
    pub snippet: String,
}

impl Library {
    pub fn index_path(&self) -> PathBuf {
        self.root.join(".notetaker/index.sqlite")
    }

    /// Rebuild all index content transactionally from the meeting files.
    pub fn reindex_full(&self) -> Result<()> {
        self.reindex(true)
    }

    /// Compare each source file's mtime/length, also detecting missing/new files.
    /// A missing SQLite file is created and populated just like a full rebuild.
    pub fn reindex_incremental(&self) -> Result<()> {
        self.reindex(false)
    }

    /// Search all words with AND semantics and prefix matching. Punctuation and
    /// FTS operators are treated as separators; user text never becomes FTS syntax.
    /// Call a reindex method first to synchronize the disposable index with files.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let query = query
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(|word| format!("\"{word}\"*"))
            .collect::<Vec<_>>()
            .join(" AND ");
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.open_index()?;
        let mut statement = connection.prepare(
            "SELECT meeting_id, path, title, snippet(meetings, -1, '[', ']', ' … ', 24)
             FROM meetings WHERE meetings MATCH ?1 ORDER BY rank, path LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![query, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (id, path, title, snippet) = row?;
            Ok(Hit {
                meeting_id: Uuid::parse_str(&id)
                    .map_err(|error| Error::InvalidIndex(error.to_string()))?,
                path: PathBuf::from(path),
                title,
                snippet,
            })
        })
        .collect()
    }

    /// Natural-language retrieval with safe OR terms and weighted FTS5 BM25.
    /// Read privacy from source metadata before returning snippets or applying limit.
    pub fn retrieve(&self, question: &str, limit: usize, exclude_local: bool) -> Result<Vec<Hit>> {
        let query = retrieval_query(question);
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.open_index()?;
        let mut statement = connection.prepare(
            "SELECT meeting_id, path, title, snippet(meetings, -1, '[', ']', ' … ', 48)
             FROM meetings WHERE meetings MATCH ?1
             ORDER BY bm25(meetings, 0, 0, 0, 5, 2, 2, 1), path",
        )?;
        let mut rows = statement.query([query])?;
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            let path = PathBuf::from(row.get::<_, String>(1)?);
            // Index paths must still identify a direct child of this library.
            if path.parent() != Some(self.root()) {
                continue;
            }
            let meeting = Meeting { path: path.clone() };
            let Ok(meta) = meeting.read_meta() else {
                continue;
            };
            if exclude_local && meta.mode == crate::Mode::Local {
                continue;
            }
            hits.push(Hit {
                meeting_id: meta.id,
                path,
                title: meta.title,
                snippet: row.get(3)?,
            });
            if hits.len() >= limit.min(100) {
                break;
            }
        }
        Ok(hits)
    }

    fn open_index(&self) -> Result<Connection> {
        let path = self.index_path();
        fs::create_dir_all(path.parent().expect("index has a parent"))?;
        match connect_index(&path) {
            Ok(connection) => Ok(connection),
            Err(error) if disposable_index_error(&error) => {
                tracing::warn!(path = %path.display(), %error, "recreating invalid search index");
                fs::remove_file(&path)?;
                connect_index(&path)
            }
            Err(error) => Err(error),
        }
    }

    fn reindex(&self, full: bool) -> Result<()> {
        let mut connection = self.open_index()?;
        let transaction = connection.transaction()?;
        if full {
            transaction.execute("DELETE FROM meetings", [])?;
        }
        let mut existing: HashMap<String, String> = {
            let mut statement = transaction.prepare("SELECT path, stamp FROM meetings")?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        for meeting in self.list_meetings()? {
            let Some(path) = meeting.path.to_str() else {
                tracing::warn!(path = %meeting.path.display(), "skipping non-UTF8 meeting path");
                continue;
            };
            let previous = existing.remove(path);
            let contents = (|| -> Result<_> {
                // Capture before reading: edits during indexing are seen next time.
                let stamp = source_stamp(&meeting)?;
                if previous.as_deref() == Some(stamp.as_str()) {
                    return Ok(None);
                }
                let meta = meeting.read_meta()?;
                let notes = meeting.read_notes()?;
                let enhanced = meeting.read_enhanced()?;
                let transcript = meeting.read_transcript()?.named_text();
                Ok(Some((stamp, meta, notes, enhanced, transcript)))
            })();
            let (stamp, meta, notes, enhanced, transcript) = match contents {
                Ok(Some(contents)) => contents,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%path, %error, "skipping invalid meeting during indexing");
                    transaction.execute("DELETE FROM meetings WHERE path = ?1", [path])?;
                    continue;
                }
            };
            transaction.execute("DELETE FROM meetings WHERE path = ?1", [path])?;
            transaction.execute(
                "INSERT INTO meetings (meeting_id, path, stamp, title, notes, enhanced, transcript)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    meta.id.to_string(),
                    path,
                    stamp,
                    meta.title,
                    notes,
                    enhanced,
                    transcript
                ],
            )?;
        }
        for path in existing.keys() {
            transaction.execute("DELETE FROM meetings WHERE path = ?1", [path])?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn source_stamp(meeting: &Meeting) -> Result<String> {
    let mut stamps = Vec::new();
    for name in ["meta.yaml", "notes.md", "enhanced.md", "transcript.json"] {
        let stamp = match fs::metadata(meeting.path.join(name)) {
            Ok(metadata) => Some((metadata.modified()?, metadata.len())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        stamps.push(stamp);
    }
    Ok(serde_json::to_string(&stamps)?)
}

fn connect_index(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS meetings USING fts5(
            meeting_id UNINDEXED, path UNINDEXED, stamp UNINDEXED,
            title, notes, enhanced, transcript,
            tokenize = 'unicode61 remove_diacritics 2'
        );",
    )?;
    connection.prepare(
        "SELECT meeting_id, path, stamp, title, notes, enhanced, transcript FROM meetings LIMIT 0",
    )?;
    let schema: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE name = 'meetings'",
        [],
        |row| row.get(0),
    )?;
    if !schema.to_ascii_lowercase().contains("using fts5(") {
        return Err(Error::InvalidIndex("meetings is not an FTS5 table".into()));
    }
    let integrity: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(Error::InvalidIndex(integrity));
    }
    Ok(connection)
}

fn disposable_index_error(error: &Error) -> bool {
    match error {
        Error::InvalidIndex(_) | Error::Sqlite(rusqlite::Error::SqlInputError { .. }) => true,
        Error::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => matches!(
            code.extended_code & 0xff,
            rusqlite::ffi::SQLITE_CORRUPT
                | rusqlite::ffi::SQLITE_NOTADB
                | rusqlite::ffi::SQLITE_SCHEMA
                | rusqlite::ffi::SQLITE_ERROR
        ),
        _ => false, // Busy, permissions and I/O failures must not discard an index.
    }
}

/// Never interpolate FTS operators. A small bilingual stop list is enough for v0.6.
fn retrieval_query(question: &str) -> String {
    const STOP: &[&str] = &[
        "и",
        "а",
        "но",
        "в",
        "во",
        "на",
        "по",
        "о",
        "об",
        "от",
        "до",
        "из",
        "к",
        "ко",
        "с",
        "со",
        "у",
        "за",
        "для",
        "не",
        "это",
        "как",
        "что",
        "какие",
        "какой",
        "кто",
        "где",
        "когда",
        "мы",
        "вы",
        "я",
        "он",
        "она",
        "они",
        "ли",
        "же",
        "про",
        "были",
        "было",
        "был",
        "the",
        "a",
        "an",
        "and",
        "or",
        "to",
        "of",
        "in",
        "on",
        "for",
        "is",
        "are",
        "was",
        "were",
        "what",
        "which",
        "who",
        "when",
        "where",
        "how",
        "did",
        "do",
        "about",
        "we",
        "you",
        "i",
    ];
    let mut terms = std::collections::BTreeSet::new();
    for term in question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(128)
    {
        let term = term.to_lowercase();
        if !STOP.contains(&term.as_str()) && term.chars().count() > 1 {
            terms.insert(term);
        }
    }
    terms
        .into_iter()
        .take(32)
        .map(|term| format!("\"{term}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[cfg(test)]
mod retrieval_tests {
    use super::*;
    use crate::{MeetingMeta, Mode};
    #[test]
    fn natural_question_bm25_ranking_and_source_privacy() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let relevant = library
            .create_meeting(MeetingMeta::new("Kubernetes", now))
            .unwrap();
        relevant.write_notes("решили обновить Kubernetes").unwrap();
        let other = library
            .create_meeting(MeetingMeta::new("Другое", now))
            .unwrap();
        other
            .write_notes("Kubernetes и много слов о погоде и об инфраструктуре")
            .unwrap();
        let mut meta = other.read_meta().unwrap();
        meta.mode = Mode::Hybrid;
        other.write_meta(&meta).unwrap();
        library.reindex_full().unwrap();
        let hits = library
            .retrieve("Что мы решили про Kubernetes?", 10, false)
            .unwrap();
        assert_eq!(hits[0].path, relevant.path());
        assert_eq!(hits.len(), 2);
        assert_eq!(
            library.retrieve("Kubernetes", 1, true).unwrap()[0].path,
            other.path()
        );
        // Privacy changes are authoritative even before reindexing.
        meta.mode = Mode::Local;
        other.write_meta(&meta).unwrap();
        assert!(library.retrieve("Kubernetes", 10, true).unwrap().is_empty());
        assert!(library.retrieve("что и как", 10, false).unwrap().is_empty());
        assert!(library.retrieve("\" OR *", 10, false).unwrap().is_empty());
    }
}
