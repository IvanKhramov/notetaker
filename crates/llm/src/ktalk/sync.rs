use super::{Client, Recording, Transcript, User};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use notetaker_core::{Library, Meeting, Transcript as Local};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

type Interval = (i64, i64);
fn union(mut intervals: Vec<Interval>) -> Vec<Interval> {
    intervals.sort_unstable();
    let mut out: Vec<Interval> = Vec::new();
    for (a, b) in intervals {
        if let Some(last) = out.last_mut()
            && a <= last.1
        {
            last.1 = last.1.max(b);
        } else if b > a {
            out.push((a, b));
        }
    }
    out
}
fn overlap(a: &[Interval], b: &[Interval], offset: i64) -> i64 {
    let (mut i, mut j, mut total) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        let (start, end) = (a[i].0 + offset, a[i].1 + offset);
        total += (end.min(b[j].1) - start.max(b[j].0)).max(0);
        if end < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    total
}
fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
fn duration(meeting: &Meeting, local: &Local) -> Result<i64> {
    let meta = meeting.read_meta()?;
    let duration = meta
        .ended_at
        .map(|end| (end - meta.started_at).num_milliseconds())
        .unwrap_or_else(|| {
            local
                .segments
                .iter()
                .map(|s| s.end_ms)
                .max()
                .unwrap_or(0)
                .min(i64::MAX as u64) as i64
        });
    ensure!(duration > 0, "У встречи нет длительности");
    Ok(duration)
}
pub fn match_recording(
    recordings: &[Recording],
    start: DateTime<Utc>,
    duration_ms: i64,
) -> Result<Recording> {
    let mut candidates = Vec::new();
    for r in recordings {
        let delta = (r.created_date - start).num_milliseconds();
        let length = i64::from(r.duration) * 1000;
        let amount = (duration_ms.min(delta + length) - 0.max(delta)).max(0);
        if r.status == "complete" && amount > 0 {
            candidates.push((amount, r));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    let Some((best, r)) = candidates.first() else {
        anyhow::bail!("Подходящая запись Толка не найдена");
    };
    ensure!(
        *best * 2 >= duration_ms.min(i64::from(r.duration) * 1000),
        "Пересечение с записью Толка меньше половины короткого интервала"
    );
    if candidates
        .get(1)
        .is_some_and(|(score, _)| *score as f64 >= *best as f64 * 0.9)
    {
        let titles = candidates
            .iter()
            .filter(|(s, _)| *s as f64 >= *best as f64 * 0.9)
            .map(|(_, r)| format!("{} ({})", r.title, r.created_date))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!("Неоднозначное совпадение записей Толка: {titles}");
    }
    Ok((*r).clone())
}
pub fn select_recording(
    client: &Client,
    meeting: &Meeting,
    explicit: Option<&str>,
) -> Result<Recording> {
    let local = meeting.load_transcript()?.value;
    let meta = meeting.read_meta()?;
    let duration = duration(meeting, &local)?;
    let cutoff = meta.started_at - chrono::Duration::days(1);
    let mut recordings = Vec::new();
    let mut skip = 0;
    let mut previous = None;
    loop {
        let page = client.recordings("", skip, 100)?;
        if page.is_empty() {
            break;
        }
        // The API has no explicit ordering parameter. Refuse unexpected ordering
        // rather than silently missing a recording at the paging cutoff.
        for r in &page {
            ensure!(
                previous.is_none_or(|date| r.created_date <= date),
                "Толк вернул записи не по убыванию времени"
            );
            previous = Some(r.created_date);
        }
        skip += page.len();
        let done = page.last().is_some_and(|r| r.created_date < cutoff);
        recordings.extend(page);
        if done {
            break;
        }
    }
    if let Some(id) = explicit {
        let r = recordings.into_iter().find(|r| r.id == id).ok_or_else(|| {
            anyhow::anyhow!("Указанная запись Толка не найдена в периоде встречи")
        })?;
        ensure!(r.status == "complete", "Запись Толка ещё не готова");
        Ok(r)
    } else {
        match_recording(&recordings, meta.started_at, duration)
    }
}
#[derive(Debug)]
pub struct Row {
    pub speaker: String,
    pub participant: String,
    pub segments: usize,
    pub seconds: f64,
}
pub struct Plan {
    pub recording: Recording,
    /// Add to a local timestamp to obtain a Tolk timestamp.
    pub offset_ms: i64,
    pub validation_score: f64,
    pub rows: Vec<Row>,
    before: Local,
    after: Local,
}
pub fn plan(
    local: &Local,
    started_at: DateTime<Utc>,
    recording: Recording,
    remote: &Transcript,
    owner: &User,
) -> Result<Plan> {
    ensure!(
        remote.status == "complete",
        "Транскрипция Толка ещё не готова"
    );
    let words: Vec<_> = remote
        .tracks
        .iter()
        .flat_map(|t| t.chunks.iter().flat_map(|c| &c.words))
        .collect();
    let activity = union(
        local
            .segments
            .iter()
            .map(|s| (s.start_ms as i64, s.end_ms as i64))
            .collect(),
    );
    let speech = union(
        words
            .iter()
            .map(|w| (w.start_time_offset_in_millis, w.end_time_offset_in_millis))
            .collect(),
    );
    let implied = (started_at - recording.created_date).num_milliseconds();
    let offset = (-1800..=1800)
        .map(|step| implied + step * 100)
        .max_by_key(|offset| {
            (
                overlap(&activity, &speech, *offset),
                -(*offset - implied).abs(),
            )
        })
        .unwrap();
    let mut score = 0.0;
    let mut count = 0;
    let mut matched = 0;
    // At most 64 evenly distributed segments; symmetric token Dice tolerates ASR
    // punctuation and small lexical differences without rewarding unrelated text.
    let stride = local.segments.len().div_ceil(64).max(1);
    for segment in local.segments.iter().step_by(stride) {
        let a = tokens(&segment.text);
        if a.is_empty() {
            continue;
        }
        let text = words
            .iter()
            .filter(|w| {
                w.start_time_offset_in_millis < segment.end_ms as i64 + offset
                    && w.end_time_offset_in_millis > segment.start_ms as i64 + offset
            })
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let b = tokens(&text);
        let common = a.intersection(&b).count();
        matched += common;
        score += 2.0 * common as f64 / (a.len() + b.len()) as f64;
        count += 1;
    }
    let score = score / count.max(1) as f64;
    ensure!(
        score >= 0.5 && matched >= 3,
        "не удалось надёжно сопоставить время"
    );
    let mut after = local.clone();
    let mut rows: BTreeMap<(String, String), (usize, u64)> = BTreeMap::new();
    let mut tracks: BTreeMap<String, (String, Vec<Interval>)> = BTreeMap::new();
    for track in &remote.tracks {
        let (identity, name) = track.speaker.identity()?;
        let hash: String = Sha256::digest(identity.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let id = format!("ktalk-{hash}");
        let entry = tracks.entry(id).or_insert((name.clone(), Vec::new()));
        ensure!(entry.0 == name, "Разные имена одного участника Толка");
        entry.1.extend(
            track
                .chunks
                .iter()
                .flat_map(|c| &c.words)
                .map(|w| (w.start_time_offset_in_millis, w.end_time_offset_in_millis)),
        );
    }
    for (_, intervals) in tracks.values_mut() {
        *intervals = union(std::mem::take(intervals));
    }
    for segment in &mut after.segments {
        let original = &segment.speaker.0;
        if segment.edited || local.speaker_names.contains_key(original) {
            continue;
        }
        if original == "me" {
            ensure!(
                !owner.key.is_empty() && !owner.name().is_empty(),
                "Некорректный профиль Толка"
            );
            after.speaker_names.insert("me".into(), owner.name());
            after.speaker_suggestions.remove("me");
            after
                .ktalk_sources
                .insert("me".into(), recording.id.clone());
            let row = rows.entry((original.clone(), owner.name())).or_default();
            row.0 += 1;
            row.1 += segment.end_ms.saturating_sub(segment.start_ms);
            continue;
        }
        if local.self_speaker.as_ref() == Some(original) {
            continue;
        }
        let interval = [(segment.start_ms as i64, segment.end_ms as i64)];
        let shares: Vec<_> = tracks
            .iter()
            .map(|(id, (name, times))| (overlap(&interval, times, offset), id, name))
            .collect();
        let total: i64 = shares.iter().map(|(n, _, _)| n).sum();
        if let Some((amount, id, name)) = shares.iter().max_by_key(|(n, _, _)| n)
            && total > 0
            && *amount as f64 / total as f64 >= 0.6
        {
            let row = rows.entry((original.clone(), (*name).clone())).or_default();
            row.0 += 1;
            row.1 += segment.end_ms.saturating_sub(segment.start_ms);
            segment.speaker.0 = (*id).clone();
            // Existing confirmed names, including previous Tolk names, are immutable.
            after
                .speaker_names
                .entry((*id).clone())
                .or_insert_with(|| (*name).clone());
            after.speaker_suggestions.remove(*id);
            after
                .ktalk_sources
                .insert((*id).clone(), recording.id.clone());
        }
    }
    Ok(Plan {
        recording,
        offset_ms: offset,
        validation_score: score,
        rows: rows
            .into_iter()
            .map(|((speaker, participant), (segments, ms))| Row {
                speaker,
                participant,
                segments,
                seconds: ms as f64 / 1000.0,
            })
            .collect(),
        before: local.clone(),
        after,
    })
}
/// Reject a stale plan, including on storage's revision-conflict retry.
pub fn apply(library: &Library, meeting: &Meeting, plan: &Plan) -> Result<()> {
    let mut stale = false;
    meeting.update_transcript(|current| {
        if *current == plan.after {
            return;
        }
        if *current != plan.before {
            stale = true;
            return;
        }
        *current = plan.after.clone();
    })?;
    ensure!(
        !stale,
        "Транскрипт изменился; повторите синхронизацию Толка"
    );
    library.reindex_incremental()?;
    Ok(())
}
#[cfg(test)]
mod tests;
