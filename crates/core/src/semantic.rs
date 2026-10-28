//! Rebuildable semantic index. Source files and current privacy always win.
use crate::index::source_stamp;
use crate::{Error, Hit, Library, Meeting, Mode, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Keyword,
    Semantic,
    #[default]
    Hybrid,
}
impl std::str::FromStr for SearchMode {
    type Err = String;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "keyword" => Ok(Self::Keyword),
            "semantic" => Ok(Self::Semantic),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err("Ожидается keyword, semantic или hybrid".into()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum Anchor {
    Notes { line: usize },
    Summary { line: usize },
    Transcript { segment: Uuid, start_ms: u64 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub text: String,
    pub anchor: Anchor,
}
/// Implementations must identify the exact model/tokenizer/pooling revision.
/// A remote embedder is never called for a Local source, including loopback HTTP.
pub trait Embedder {
    fn model_id(&self) -> &str;
    fn is_local(&self) -> bool;
    fn embed(&mut self, text: &str, query: bool) -> Result<Vec<f32>>;
}
#[derive(Clone, Debug, Default)]
pub struct SemanticProgress {
    pub completed: usize,
    pub total: usize,
    pub excluded_local: usize,
    pub chunks: usize,
}

/// About 400 E5 tokens (1800 Unicode characters), 60-token overlap. The worker
/// applies the model's tokenizer limit as well. Long lines/segments are split.
pub fn meeting_chunks(meeting: &Meeting) -> Result<Vec<Chunk>> {
    let mut result = Vec::new();
    for (summary, text) in [
        (false, meeting.read_notes()?),
        (true, meeting.read_enhanced()?),
    ] {
        split_chunks(
            &text,
            |offset| {
                let line = text[..offset].bytes().filter(|&c| c == b'\n').count() + 1;
                if summary {
                    Anchor::Summary { line }
                } else {
                    Anchor::Notes { line }
                }
            },
            &mut result,
        );
    }
    let transcript = meeting.read_transcript()?;
    let mut text = String::new();
    let mut positions = Vec::new();
    for segment in &transcript.segments {
        positions.push((text.len(), segment));
        text.push_str(&segment.text);
        text.push('\n');
    }
    if !positions.is_empty() {
        split_chunks(
            &text,
            |offset| {
                let index = positions
                    .partition_point(|(pos, _)| *pos <= offset)
                    .saturating_sub(1);
                let segment = positions[index].1;
                Anchor::Transcript {
                    segment: segment.id,
                    start_ms: segment.start_ms,
                }
            },
            &mut result,
        );
    }
    Ok(result)
}
fn split_chunks(text: &str, anchor: impl Fn(usize) -> Anchor, result: &mut Vec<Chunk>) {
    let positions: Vec<_> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let mut start = 0;
    let len = positions.len() - 1;
    while start < len {
        let mut end = (start + 1800).min(len);
        if end < len {
            while end > start + 1500 && !text[positions[end]..].starts_with(char::is_whitespace) {
                end -= 1;
            }
        }
        let raw = &text[positions[start]..positions[end]];
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            result.push(Chunk {
                text: trimmed.into(),
                anchor: anchor(positions[start] + raw.len() - raw.trim_start().len()),
            });
        }
        if end == len {
            break;
        }
        start = end.saturating_sub(270).max(start + 1);
    }
}
fn normalize(mut vector: Vec<f32>) -> Result<Vec<f32>> {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if vector.is_empty() || vector.len() > 8192 || !norm.is_finite() || norm <= 0. {
        return Err(Error::Embedding("Некорректный вектор".into()));
    }
    vector.iter_mut().for_each(|v| *v /= norm);
    Ok(vector)
}
impl Library {
    /// Commit one meeting at a time; inference never holds a SQLite write lock.
    /// mtime/length includes meta, so privacy and deletions invalidate old vectors.
    pub fn reindex_semantic(
        &self,
        embedder: &mut dyn Embedder,
        full: bool,
        progress: &mut dyn FnMut(SemanticProgress),
    ) -> Result<SemanticProgress> {
        self.reindex_incremental()?;
        let mut connection = self.open_index()?;
        if full {
            connection.execute(
                "DELETE FROM semantic_sources WHERE model=?1",
                [embedder.model_id()],
            )?;
            connection.execute(
                "DELETE FROM semantic_chunks WHERE model=?1",
                [embedder.model_id()],
            )?;
        }
        let meetings = self.list_meetings()?;
        let mut state = SemanticProgress {
            total: meetings.len(),
            ..Default::default()
        };
        connection.execute(
            "DELETE FROM semantic_chunks WHERE path NOT IN (SELECT path FROM meetings)",
            [],
        )?;
        connection.execute(
            "DELETE FROM semantic_sources WHERE path NOT IN (SELECT path FROM meetings)",
            [],
        )?;
        progress(state.clone());
        for meeting in meetings {
            meeting.validate_source_paths()?;
            let stamp = source_stamp(&meeting)?;
            let meta = meeting.read_meta()?;
            let path = meeting.path().to_string_lossy();
            // Remove obsolete remote material even when currently using a local model.
            if meta.mode == Mode::Local {
                connection.execute(
                    "DELETE FROM semantic_chunks WHERE path=?1 AND local=0",
                    [path.as_ref()],
                )?;
                connection.execute(
                    "DELETE FROM semantic_sources WHERE path=?1 AND local=0",
                    [path.as_ref()],
                )?;
            }
            if meta.mode == Mode::Local && !embedder.is_local() {
                state.excluded_local += 1;
            } else {
                let previous: Option<String> = connection
                    .query_row(
                        "SELECT stamp FROM semantic_sources WHERE path=?1 AND model=?2",
                        params![path, embedder.model_id()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if previous.as_deref() != Some(&stamp) {
                    let chunks = meeting_chunks(&meeting)?;
                    let mut vectors = Vec::with_capacity(chunks.len());
                    for chunk in &chunks {
                        // Recheck before every remote call if privacy changes mid-pass.
                        if !embedder.is_local() && meeting.read_meta()?.mode == Mode::Local {
                            return Err(Error::Embedding(
                                "Режим встречи изменён: повторите индексацию".into(),
                            ));
                        }
                        let vector = normalize(embedder.embed(&chunk.text, false)?)?;
                        if vectors
                            .first()
                            .is_some_and(|v: &Vec<f32>| v.len() != vector.len())
                        {
                            return Err(Error::Embedding("Размерность модели изменилась".into()));
                        }
                        vectors.push(vector);
                    }
                    if source_stamp(&meeting)? == stamp {
                        let tx = connection.transaction()?;
                        tx.execute(
                            "DELETE FROM semantic_chunks WHERE path=?1 AND model=?2",
                            params![path, embedder.model_id()],
                        )?;
                        for (chunk, vector) in chunks.iter().zip(vectors) {
                            let bytes: Vec<u8> =
                                vector.iter().flat_map(|v| v.to_le_bytes()).collect();
                            tx.execute("INSERT INTO semantic_chunks(path,model,stamp,local,chunk,vector) VALUES(?1,?2,?3,?4,?5,?6)",params![path,embedder.model_id(),stamp,embedder.is_local(),serde_json::to_string(chunk)?,bytes])?;
                        }
                        tx.execute("INSERT OR REPLACE INTO semantic_sources(path,model,stamp,local) VALUES(?1,?2,?3,?4)",params![path,embedder.model_id(),stamp,embedder.is_local()])?;
                        tx.commit()?;
                        state.chunks += chunks.len();
                    }
                }
            }
            state.completed += 1;
            progress(state.clone());
        }
        Ok(state)
    }
    pub fn semantic_ready(&self, model: &str) -> Result<bool> {
        Ok(self.open_index()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM semantic_chunks WHERE model=?1)",
            [model],
            |r| r.get(0),
        )?)
    }
    /// Semantic mode keeps Local FTS results when only a remote model is present.
    /// No network call occurs for keyword mode, empty queries, or an empty index.
    pub fn search_with(
        &self,
        query: &str,
        limit: usize,
        mode: SearchMode,
        exclude_local: bool,
        mut embedder: Option<&mut dyn Embedder>,
    ) -> Result<Vec<Hit>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let lexical = self.retrieve(query, 100, exclude_local)?;
        if mode == SearchMode::Keyword {
            return Ok(lexical.into_iter().take(limit).collect());
        }
        let Some(embedder) = embedder.as_mut() else {
            return Ok(lexical.into_iter().take(limit).collect());
        };
        if !self.semantic_ready(embedder.model_id())? {
            return Ok(lexical.into_iter().take(limit).collect());
        }
        let vector = normalize(embedder.embed(query, true)?)?;
        let mut semantic = self.semantic_hits(&vector, embedder.model_id(), exclude_local)?;
        if mode == SearchMode::Semantic {
            if !embedder.is_local() {
                let local_keyword = lexical
                    .into_iter()
                    .filter(|hit| {
                        Meeting {
                            path: hit.path.clone(),
                        }
                        .read_meta()
                        .is_ok_and(|m| m.mode == Mode::Local)
                    })
                    .collect();
                return Ok(fuse(local_keyword, semantic, limit.min(100)));
            }
            semantic.truncate(limit.min(100));
            return Ok(semantic);
        }
        Ok(fuse(lexical, semantic, limit.min(100)))
    }
    fn semantic_hits(&self, query: &[f32], model: &str, exclude_local: bool) -> Result<Vec<Hit>> {
        let connection = self.open_index()?;
        let mut stmt = connection.prepare(
            "SELECT path,stamp,local,chunk,vector FROM semantic_chunks WHERE model=?1 ORDER BY id",
        )?;
        let mut rows = stmt.query([model])?;
        let mut sources = HashMap::new();
        // Keep only three candidates per meeting, not 100k copies of source text.
        let mut candidates: HashMap<PathBuf, Vec<(f32, String)>> = HashMap::new();
        while let Some(row) = rows.next()? {
            let path = PathBuf::from(row.get::<_, String>(0)?);
            let stamp: String = row.get(1)?;
            let local: bool = row.get(2)?;
            let meta = sources.entry(path.clone()).or_insert_with(|| {
                let meeting = Meeting { path: path.clone() };
                if path.parent() != Some(self.root()) || meeting.validate_source_paths().is_err() {
                    return None;
                }
                let meta = meeting.read_meta().ok()?;
                if exclude_local && meta.mode == Mode::Local {
                    return None;
                }
                Some((meta, source_stamp(&meeting).ok()?))
            });
            let Some((meta, current)) = meta else {
                continue;
            };
            if current != &stamp || (!local && meta.mode == Mode::Local) {
                continue;
            }
            let bytes = row
                .get_ref(4)?
                .as_blob()
                .map_err(|e| Error::InvalidIndex(e.to_string()))?;
            if bytes.len() != query.len() * 4 {
                continue;
            }
            let score = dot_blob(query, bytes);
            if !score.is_finite() {
                continue;
            }
            let best = candidates.entry(path).or_default();
            if best.len() < 3 || score > best.last().expect("three candidates").0 {
                best.push((score, row.get(3)?));
                best.sort_by(|a, b| b.0.total_cmp(&a.0));
                best.truncate(3);
            }
        }
        let mut ranked: Vec<_> = candidates.into_iter().collect();
        ranked.sort_by(|a, b| b.1[0].0.total_cmp(&a.1[0].0).then_with(|| a.0.cmp(&b.0)));
        ranked.retain(|(path, _)| {
            let meeting = Meeting { path: path.clone() };
            meeting.validate_source_paths().is_ok()
                && source_stamp(&meeting)
                    .is_ok_and(|stamp| stamp == sources[path].as_ref().expect("validated source").1)
        });
        ranked
            .into_iter()
            .take(100)
            .map(|(path, best)| {
                let meta = &sources[&path].as_ref().expect("validated source").0;
                let chunks: Vec<Chunk> = best
                    .into_iter()
                    .map(|(_, json)| serde_json::from_str(&json))
                    .collect::<std::result::Result<_, _>>()?;
                Ok(Hit {
                    meeting_id: meta.id,
                    path,
                    title: meta.title.clone(),
                    snippet: chunks[0].text.clone(),
                    anchor: Some(chunks[0].anchor.clone()),
                    chunks,
                })
            })
            .collect()
    }
}
/// Normalized f32, little endian, no allocation per vector.
pub fn dot_blob(query: &[f32], bytes: &[u8]) -> f32 {
    query
        .iter()
        .zip(bytes.as_chunks::<4>().0)
        .map(|(q, b)| q * f32::from_le_bytes(*b))
        .sum()
}
fn fuse(keyword: Vec<Hit>, semantic: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut scores: HashMap<Uuid, (f64, Hit)> = HashMap::new();
    for (semantic_list, list) in [(false, keyword), (true, semantic)] {
        for (rank, hit) in list.into_iter().enumerate() {
            let score = 1. / (60. + rank as f64 + 1.);
            scores
                .entry(hit.meeting_id)
                .and_modify(|(total, best)| {
                    *total += score;
                    if semantic_list {
                        *best = hit.clone();
                    }
                })
                .or_insert((score, hit));
        }
    }
    let mut hits: Vec<_> = scores.into_values().collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    hits.into_iter().take(limit).map(|(_, h)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, MeetingMeta, Segment, Source, SpeakerId, Transcript};
    struct Fake {
        local: bool,
        calls: usize,
    }
    impl Embedder for Fake {
        fn model_id(&self) -> &str {
            if self.local {
                "fake-local"
            } else {
                "fake-remote"
            }
        }
        fn is_local(&self) -> bool {
            self.local
        }
        fn embed(&mut self, text: &str, _query: bool) -> Result<Vec<f32>> {
            self.calls += 1;
            if !self.local {
                assert!(!text.contains("SECRET"));
            }
            Ok(if text.contains("деньги") || text.contains("бюджет") {
                vec![1., 0.]
            } else {
                vec![0., 1.]
            })
        }
    }
    fn meeting(lib: &Library, title: &str, text: &str, mode: Mode) -> Meeting {
        let mut meta = MeetingMeta::new(title, chrono::Utc::now());
        meta.mode = mode;
        let meeting = lib.create_meeting(meta).unwrap();
        meeting.write_notes(text).unwrap();
        meeting
    }
    #[test]
    fn concurrent_first_open_keeps_one_complete_schema() {
        let dir = tempfile::tempdir().unwrap();
        let library = std::sync::Arc::new(Library::open(dir.path()).unwrap());
        meeting(&library, "План", "деньги", Mode::Local);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let library = library.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    library.reindex_incremental().unwrap();
                    assert_eq!(library.retrieve("деньги", 10, false).unwrap().len(), 1);
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }
    #[test]
    fn chunking_covers_long_unicode_lines_and_transcript_anchors() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let m = meeting(
            &lib,
            "Чанки",
            &format!("Вступление\n{}\nКонец", "юникод ".repeat(800)),
            Mode::Local,
        );
        m.write_enhanced("Первая строка\nВторая строка").unwrap();
        let id = Uuid::now_v7();
        m.write_transcript(&Transcript {
            segments: vec![Segment {
                id,
                speaker: SpeakerId("me".into()),
                channel: Channel::Mic,
                start_ms: 42000,
                end_ms: 45000,
                text: "Решили выделить деньги".into(),
                source: Source::Final,
                edited: false,
                lang: None,
            }],
            ..Transcript::default()
        })
        .unwrap();
        let chunks = meeting_chunks(&m).unwrap();
        assert!(chunks.iter().all(|c| c.text.chars().count() <= 1800));
        assert!(chunks.iter().any(|c| c.text.contains("Конец")));
        assert!(chunks.iter().any(|c| c.anchor == Anchor::Notes { line: 2 }));
        assert!(chunks.iter().any(|c| c.anchor
            == Anchor::Transcript {
                segment: id,
                start_ms: 42000
            }));
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c.anchor, Anchor::Summary { line: 1 }))
        );
        let mut raw = Vec::new();
        let text = "слово ".repeat(500);
        split_chunks(&text, |_| Anchor::Notes { line: 1 }, &mut raw);
        assert!(
            raw[0]
                .text
                .contains(&raw[1].text.chars().take(100).collect::<String>())
        );
    }
    #[test]
    fn incremental_privacy_stale_vectors_deletion_and_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let private = meeting(&lib, "Личное", "SECRET деньги", Mode::Local);
        let public = meeting(&lib, "План", "говорили про деньги на Q1", Mode::Hybrid);
        let mut remote = Fake {
            local: false,
            calls: 0,
        };
        let p = lib
            .reindex_semantic(&mut remote, false, &mut |_| {})
            .unwrap();
        assert_eq!(p.excluded_local, 1);
        assert_eq!(remote.calls, 1);
        lib.reindex_semantic(&mut remote, false, &mut |_| {})
            .unwrap();
        assert_eq!(remote.calls, 1);
        let hits = lib
            .search_with("бюджет", 10, SearchMode::Hybrid, false, Some(&mut remote))
            .unwrap();
        assert_eq!(hits[0].meeting_id, public.read_meta().unwrap().id);
        assert!(
            hits.iter()
                .all(|h| h.meeting_id != private.read_meta().unwrap().id)
        );
        let mut meta = public.read_meta().unwrap();
        meta.mode = Mode::Local;
        public.write_meta(&meta).unwrap();
        assert!(
            lib.search_with("бюджет", 10, SearchMode::Semantic, false, Some(&mut remote))
                .unwrap()
                .is_empty()
        );
        lib.reindex_semantic(&mut remote, false, &mut |_| {})
            .unwrap();
        let mut local = Fake {
            local: true,
            calls: 0,
        };
        lib.reindex_semantic(&mut local, false, &mut |_| {})
            .unwrap();
        assert_eq!(local.calls, 2);
        let hits = lib
            .search_with("бюджет", 10, SearchMode::Semantic, true, Some(&mut local))
            .unwrap();
        assert!(hits.is_empty());
        private.write_notes("Обновили деньги и сроки").unwrap();
        lib.reindex_semantic(&mut local, false, &mut |_| {})
            .unwrap();
        assert_eq!(local.calls, 4); // 2 sources + query + changed source.
        lib.reindex_semantic(&mut local, true, &mut |_| {}).unwrap();
        assert_eq!(local.calls, 6);
        // Removed meetings cannot return old vectors even without reindexing.
        let removed = tempfile::tempdir().unwrap();
        std::fs::rename(private.path(), removed.path().join("removed")).unwrap();
        lib.reindex_semantic(&mut local, false, &mut |_| {})
            .unwrap();
        assert_eq!(local.calls, 6);
        let count: i64 = lib
            .open_index()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM semantic_chunks WHERE model='fake-local'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    #[test]
    fn remote_semantic_keeps_local_keyword_matches_and_rrf_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let local = meeting(&lib, "Секрет", "бюджет SECRET", Mode::Local);
        let shared = meeting(&lib, "Финансы", "деньги на Q1", Mode::Hybrid);
        let both = meeting(&lib, "Бюджет", "бюджет и деньги", Mode::Hybrid);
        let mut fake = Fake {
            local: false,
            calls: 0,
        };
        lib.reindex_semantic(&mut fake, false, &mut |_| {}).unwrap();
        let hits = lib
            .search_with("бюджет", 10, SearchMode::Hybrid, false, Some(&mut fake))
            .unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].meeting_id, both.read_meta().unwrap().id);
        assert!(
            hits.iter()
                .any(|h| h.meeting_id == shared.read_meta().unwrap().id && h.anchor.is_some())
        );
        let hits = lib
            .search_with("бюджет", 10, SearchMode::Semantic, false, Some(&mut fake))
            .unwrap();
        assert!(
            hits.iter()
                .any(|h| h.meeting_id == local.read_meta().unwrap().id)
        );
        let calls = fake.calls;
        lib.search_with("бюджет", 10, SearchMode::Keyword, false, Some(&mut fake))
            .unwrap();
        assert_eq!(fake.calls, calls);
        lib.search_with(" ", 10, SearchMode::Hybrid, false, Some(&mut fake))
            .unwrap();
        assert_eq!(fake.calls, calls);
        assert!(normalize(vec![f32::NAN]).is_err());
        assert!(normalize(vec![0.]).is_err());
    }
    #[test]
    #[ignore = "SQLite 100k measurement, no timing assertion"]
    fn sqlite_100k_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let meeting = meeting(&lib, "Измерение", "деньги", Mode::Local);
        lib.reindex_incremental().unwrap();
        let query = vec![1f32 / 384f32.sqrt(); 384];
        let blob: Vec<u8> = query.iter().flat_map(|v| v.to_le_bytes()).collect();
        let stamp = source_stamp(&meeting).unwrap();
        let chunk = serde_json::to_string(&Chunk {
            text: "Решили обсудить расходы команды".into(),
            anchor: Anchor::Notes { line: 1 },
        })
        .unwrap();
        let mut connection = lib.open_index().unwrap();
        let tx = connection.transaction().unwrap();
        {
            let mut insert=tx.prepare("INSERT INTO semantic_chunks(path,model,stamp,local,chunk,vector) VALUES(?1,'bench',?2,1,?3,?4)").unwrap();
            for _ in 0..100_000 {
                insert
                    .execute(params![
                        meeting.path().to_str().unwrap(),
                        stamp,
                        chunk,
                        blob
                    ])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(connection);
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let hits = lib.semantic_hits(&query, "bench", false).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].chunks.len(), 3);
            eprintln!(
                "SQLITE chunks=100000 dim=384 ms={:.2} db_MiB={:.2}",
                start.elapsed().as_secs_f64() * 1000.,
                std::fs::metadata(lib.index_path()).unwrap().len() as f64 / 1048576.
            );
        }
    }
    #[test]
    #[ignore = "CPU measurement, no timing assertion"]
    fn cosine_100k_benchmark() {
        let query = vec![1f32 / (384f32.sqrt()); 384];
        let bytes: Vec<u8> = query
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .cycle()
            .take(100_000 * 384 * 4)
            .collect();
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let sum: f32 = bytes
                .as_chunks::<1536>()
                .0
                .iter()
                .map(|b| dot_blob(std::hint::black_box(&query), b))
                .sum();
            eprintln!(
                "COSINE chunks=100000 dim=384 ms={:.2} vector_MiB={:.2} checksum={sum}",
                start.elapsed().as_secs_f64() * 1000.,
                bytes.len() as f64 / 1048576.
            );
        }
    }
}
