use super::{Client, Recording, Transcript, User};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use notetaker_core::{Library, Meeting, Transcript as Local};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use super::client::{malformed, valid_time};

type Interval = (i64, i64);
fn add(a: i64, b: i64) -> Result<i64> {
    a.checked_add(b).ok_or_else(malformed)
}
fn sub(a: i64, b: i64) -> Result<i64> {
    a.checked_sub(b).ok_or_else(malformed)
}
fn shifted(interval: Interval, offset: i64) -> Result<Interval> {
    Ok((add(interval.0, offset)?, add(interval.1, offset)?))
}
fn short_recording() -> anyhow::Error {
    anyhow::anyhow!("запись слишком короткая для надёжной сверки")
}
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
fn overlap(a: &[Interval], b: &[Interval]) -> Result<i64> {
    let (mut i, mut j, mut total) = (0, 0, 0);
    while let (Some(&(start, end)), Some(&(other_start, other_end))) = (a.get(i), b.get(j)) {
        total = add(
            total,
            sub(end.min(other_end), start.max(other_start))?.max(0),
        )?;
        if end < other_end {
            i += 1;
        } else {
            j += 1;
        }
    }
    Ok(total)
}
// Deliberately small, language-independent evidence still needs diversity and coverage.
const STOP_WORDS: &[&str] = &[
    "ага",
    "угу",
    "да",
    "нет",
    "ну",
    "вот",
    "это",
    "эта",
    "этот",
    "эти",
    "там",
    "тут",
    "так",
    "как",
    "что",
    "чтобы",
    "когда",
    "если",
    "или",
    "для",
    "при",
    "про",
    "без",
    "уже",
    "ещё",
    "еще",
    "тоже",
    "также",
    "просто",
    "значит",
    "короче",
    "ладно",
    "хорошо",
    "привет",
    "здравствуйте",
    "спасибо",
    "пожалуйста",
    "сегодня",
    "вместе",
    "номер",
    "the",
    "and",
    "but",
    "for",
    "with",
    "from",
    "this",
    "that",
    "these",
    "those",
    "you",
    "your",
    "our",
    "they",
    "their",
    "are",
    "was",
    "were",
    "have",
    "has",
    "had",
    "not",
    "yes",
    "yeah",
    "okay",
    "well",
    "just",
    "like",
    "hello",
    "thanks",
    "please",
];
const GRID_MS: i64 = 100;
const MAX_BINS: usize = 131_072;

// The search performs at most 3601 * 2048 word shifts/popcounts. Rasterization
// follows interval union and costs O(intervals + bins) per timeline.
fn estimate_offset(activity: &[Interval], speech: &[Interval], implied: i64) -> Result<i64> {
    let normalized: Vec<_> = speech
        .iter()
        .map(|(a, b)| Ok((sub(*a, implied)?, sub(*b, implied)?)))
        .collect::<Result<_>>()?;
    let bounds = activity.iter().chain(&normalized);
    let origin = sub(bounds.clone().map(|(a, _)| *a).min().unwrap_or(0), 180_000)?;
    let end = add(bounds.map(|(_, b)| *b).max().unwrap_or(0), 180_000)?;
    let bins =
        usize::try_from(add(sub(end, origin)?, GRID_MS - 1)? / GRID_MS).map_err(|_| malformed())?;
    ensure!(
        bins <= MAX_BINS,
        "Слишком длинный интервал для поиска смещения Толка (лимит 131072 отсчёта по 100 мс)"
    );
    let raster = |intervals: &[Interval]| -> Result<Vec<u64>> {
        let mut bits = vec![0u64; bins.div_ceil(64)];
        for &(a, b) in intervals {
            ensure!(a <= b, malformed());
            let first = usize::try_from(sub(a, origin)? / GRID_MS).map_err(|_| malformed())?;
            let end = usize::try_from(add(sub(b, origin)?, GRID_MS - 1)? / GRID_MS)
                .map_err(|_| malformed())?;
            ensure!(end <= bins, malformed());
            for i in first..end {
                *bits.get_mut(i / 64).ok_or_else(malformed)? |= 1 << (i % 64);
            }
        }
        Ok(bits)
    };
    let a = raster(activity)?;
    let b = raster(&normalized)?;
    let score = |shift: i64| {
        let q = shift.div_euclid(64);
        let r = shift.rem_euclid(64) as u32;
        a.iter()
            .enumerate()
            .map(|(i, bits)| {
                let j = i as i64 + q;
                let low = b.get(j as usize).copied().unwrap_or(0);
                let high = b.get((j + 1) as usize).copied().unwrap_or(0);
                ((bits << r) & low).count_ones()
                    + if r == 0 {
                        0
                    } else {
                        ((bits >> (64 - r)) & high).count_ones()
                    }
            })
            .sum::<u32>()
    };
    // Scores are bounded by MAX_BINS. Step/index arithmetic below depends only
    // on this fixed search grid, never on an unvalidated network timestamp.
    let scores: Vec<_> = (-1800i64..=1800).map(|step| (step, score(step))).collect();
    let &(step, best) = scores
        .iter()
        .max_by_key(|(step, score)| (*score, -step.abs()))
        .ok_or_else(malformed)?;
    let second = scores
        .iter()
        .filter(|(other, _)| (other - step).abs() > 20)
        .map(|(_, score)| *score)
        .max()
        .unwrap_or(0);
    ensure!(
        best > 0 && (best - second) * 100 >= best * 10,
        "неоднозначное сопоставление времени: отрыв от другого смещения меньше 10%"
    );
    add(implied, step * GRID_MS)
}

fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.chars().count() >= 3 && s.chars().all(char::is_alphabetic))
        .map(str::to_lowercase)
        .filter(|s| !STOP_WORDS.contains(&s.as_str()))
        .collect()
}
fn duration(meeting: &Meeting, local: &Local) -> Result<i64> {
    let meta = meeting.read_meta()?;
    let duration = if let Some(end) = meta.ended_at {
        (end - meta.started_at).num_milliseconds()
    } else {
        i64::try_from(local.segments.iter().map(|s| s.end_ms).max().unwrap_or(0))
            .map_err(|_| malformed())?
    };
    ensure!(duration > 0, "У встречи нет длительности");
    Ok(duration)
}
fn recording_overlap(r: &Recording, start: DateTime<Utc>, duration_ms: i64) -> Result<i64> {
    let delta = (r.created_date - start).num_milliseconds();
    let end = add(delta, i64::from(r.duration) * 1000)?;
    Ok(sub(duration_ms.min(end), 0.max(delta))?.max(0))
}
pub fn match_recording(
    recordings: &[Recording],
    start: DateTime<Utc>,
    duration_ms: i64,
) -> Result<Recording> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for r in recordings {
        if !seen.insert(&r.id) {
            continue;
        }
        let amount = recording_overlap(r, start, duration_ms)?;
        if r.status == "complete" && amount > 0 {
            candidates.push((amount, r));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    let Some((best, r)) = candidates.first() else {
        anyhow::bail!("Подходящая запись Толка не найдена");
    };
    ensure!(
        best.checked_mul(2).ok_or_else(malformed)? >= duration_ms.min(i64::from(r.duration) * 1000),
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
    let cutoff = meta
        .started_at
        .checked_sub_signed(chrono::Duration::days(1))
        .ok_or_else(malformed)?;
    let recordings = scan_recordings(|skip| client.recordings("", skip, 100), cutoff)?;
    select_from_recordings(&recordings, meta.started_at, duration, explicit)
}

const PAGE_LIMIT: usize = 30;
fn scan_recordings(
    mut fetch: impl FnMut(usize) -> Result<Vec<Recording>>,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Recording>> {
    let mut recordings = Vec::new();
    let mut seen = BTreeSet::new();
    for page_index in 0..PAGE_LIMIT {
        let page = fetch(page_index * 100)?;
        ensure!(
            page.len() <= 100,
            "Некорректная структура ответа Толка: размер страницы больше 100"
        );
        let done = page.len() < 100 || page.iter().all(|r| r.created_date < cutoff);
        let before = recordings.len();
        recordings.extend(page.into_iter().filter(|r| seen.insert(r.id.clone())));
        if done || recordings.len() == before {
            return Ok(recordings);
        }
    }
    anyhow::bail!("Достигнут лимит 30 страниц записей Толка; выбор по неполным данным запрещён")
}
fn select_from_recordings(
    recordings: &[Recording],
    start: DateTime<Utc>,
    duration: i64,
    explicit: Option<&str>,
) -> Result<Recording> {
    if let Some(id) = explicit {
        let r = recordings.iter().find(|r| r.id == id).ok_or_else(|| {
            anyhow::anyhow!("Указанная запись Толка не найдена в периоде встречи")
        })?;
        ensure!(r.status == "complete", "Запись Толка ещё не готова");
        match_recording(std::slice::from_ref(r), start, duration)
    } else {
        match_recording(recordings, start, duration)
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
    remote.validate()?;
    for segment in &local.segments {
        let start = i64::try_from(segment.start_ms).map_err(|_| malformed())?;
        let end = i64::try_from(segment.end_ms).map_err(|_| malformed())?;
        valid_time(start, end)?;
    }
    let source_tokens: BTreeSet<_> = local
        .segments
        .iter()
        .flat_map(|s| tokens(&s.text))
        .collect();
    ensure!(
        local
            .segments
            .iter()
            .filter(|s| tokens(&s.text).len() >= 4)
            .count()
            >= 6
            && source_tokens.len() >= 30,
        short_recording()
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
    let offset = estimate_offset(&activity, &speech, implied)?;
    let recording_end = i64::from(recording.duration) * 1000;
    let interval = (
        activity.first().map_or(0, |i| i.0).max(sub(0, offset)?),
        activity
            .last()
            .map_or(0, |i| i.1)
            .min(sub(recording_end, offset)?),
    );
    let eligible = |s: &&notetaker_core::Segment| {
        s.start_ms as i64 >= interval.0 && s.end_ms as i64 <= interval.1
    };
    let mut segments: Vec<_> = local.segments.iter().filter(eligible).collect();
    segments.sort_by_key(|s| s.start_ms);
    let mut score = 0.0;
    let mut count = 0;
    let mut matched = BTreeSet::new();
    let mut evidence = Vec::new();
    let mut available = BTreeSet::new();
    let samples = segments.len().min(64);
    for i in 0..samples {
        let index = i
            .checked_mul(segments.len().saturating_sub(1))
            .ok_or_else(malformed)?
            / (samples - 1).max(1);
        let segment = segments.get(index).ok_or_else(malformed)?;
        let a = tokens(&segment.text);
        if a.len() < 4 {
            continue;
        }
        available.extend(a.iter().cloned());
        let (start, end) = shifted((segment.start_ms as i64, segment.end_ms as i64), offset)?;
        let text = words
            .iter()
            .filter(|w| w.start_time_offset_in_millis < end && w.end_time_offset_in_millis > start)
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let b = tokens(&text);
        let common: BTreeSet<_> = a.intersection(&b).cloned().collect();
        let dice = 2.0 * common.len() as f64 / (a.len() + b.len()) as f64;
        if dice >= 0.8 && common.len() >= 4 {
            evidence.push((segment.start_ms as i64, segment.end_ms as i64));
            matched.extend(common);
        }
        score += dice;
        count += 1;
    }
    ensure!(count >= 6 && available.len() >= 30, short_recording());
    let score = score / count.max(1) as f64;
    let span = sub(
        evidence.iter().map(|i| i.1).max().unwrap_or(0),
        evidence.iter().map(|i| i.0).min().unwrap_or(0),
    )?;
    let length = sub(interval.1, interval.0)?;
    let required_span = if length < 180_000 { length } else { length / 3 };
    ensure!(
        score >= 0.5
            && matched.len() >= 30
            && evidence.len() >= 6
            && evidence.len() as f64 / count as f64 >= 0.6
            && add(span, 200)? >= required_span,
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
        if (segment.start_ms as i64) < interval.0
            || segment.end_ms as i64 > interval.1
            || segment.edited
            || local.speaker_names.contains_key(original)
        {
            continue;
        }
        if original == "me" {
            ensure!(
                !owner.key.is_empty() && !owner.name().is_empty(),
                "Некорректный профиль Толка"
            );
            if local.speaker_suggestions.get("me") != Some(&owner.name()) {
                after.speaker_voice_ids.remove("me");
            }
            after.speaker_names.insert("me".into(), owner.name());
            after.speaker_suggestions.remove("me");
            after
                .ktalk_sources
                .insert("me".into(), recording.id.clone());
            let row = rows.entry((original.clone(), owner.name())).or_default();
            row.0 += 1;
            row.1 = row
                .1
                .checked_add(segment.end_ms.saturating_sub(segment.start_ms))
                .ok_or_else(malformed)?;
            continue;
        }
        if local.self_speaker.as_ref() == Some(original) {
            continue;
        }
        let interval = [shifted(
            (segment.start_ms as i64, segment.end_ms as i64),
            offset,
        )?];
        let shares: Vec<_> = tracks
            .iter()
            .map(|(id, (name, times))| {
                let first = times.partition_point(|(_, end)| *end <= interval[0].0);
                let end = times.partition_point(|(start, _)| *start < interval[0].1);
                Ok((
                    overlap(&interval, times.get(first..end).ok_or_else(malformed)?)?,
                    id,
                    name,
                ))
            })
            .collect::<Result<_>>()?;
        let total = shares
            .iter()
            .try_fold(0i64, |total, (n, _, _)| add(total, *n))?;
        if let Some((amount, id, name)) = shares.iter().max_by_key(|(n, _, _)| n)
            && total > 0
            && *amount as f64 / total as f64 >= 0.6
        {
            let row = rows.entry((original.clone(), (*name).clone())).or_default();
            row.0 += 1;
            row.1 = row
                .1
                .checked_add(segment.end_ms.saturating_sub(segment.start_ms))
                .ok_or_else(malformed)?;
            segment.speaker.0 = (*id).clone();
            // Existing confirmed names, including previous Tolk names, are immutable.
            after
                .speaker_names
                .entry((*id).clone())
                .or_insert_with(|| (*name).clone());
            after.speaker_suggestions.remove(*id);
            if !local.speaker_names.contains_key(*id) {
                after.speaker_voice_ids.remove(*id);
            }
            after
                .ktalk_sources
                .insert((*id).clone(), recording.id.clone());
        }
    }
    let active = after.speaker_ids();
    after.speaker_names.retain(|id, _| active.contains(id));
    after
        .speaker_suggestions
        .retain(|id, _| active.contains(id));
    after.speaker_voice_ids.retain(|id, _| active.contains(id));
    after.ktalk_sources.retain(|id, _| active.contains(id));
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
