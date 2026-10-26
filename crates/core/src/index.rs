use std::{collections::HashMap, fs, path::PathBuf, time::Duration};

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

    fn open_index(&self) -> Result<Connection> {
        let path = self.index_path();
        fs::create_dir_all(path.parent().expect("index has a parent"))?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS meetings USING fts5(
                meeting_id UNINDEXED, path UNINDEXED, stamp UNINDEXED,
                title, notes, enhanced, transcript,
                tokenize = 'unicode61 remove_diacritics 2'
            );",
        )?;
        Ok(connection)
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
            let path = meeting.path.to_str().ok_or_else(|| {
                Error::InvalidIndex(format!(
                    "meeting path is not UTF-8: {}",
                    meeting.path.display()
                ))
            })?;
            // Capture before reading: edits during indexing will be seen next time.
            let stamp = source_stamp(&meeting)?;
            if existing.remove(path).as_deref() == Some(stamp.as_str()) {
                continue;
            }
            let meta = meeting.read_meta()?;
            let notes = meeting.read_notes()?;
            let enhanced = meeting.read_enhanced()?;
            let transcript = meeting
                .read_transcript()?
                .segments
                .into_iter()
                .map(|segment| segment.text)
                .collect::<Vec<_>>()
                .join("\n");
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
