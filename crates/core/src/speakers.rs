use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result, Transcript};

#[derive(Clone, Debug)]
pub struct MergeUndo {
    before: Transcript,
    after: Transcript,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpeakerStats {
    pub id: String,
    pub name: String,
    pub total_ms: u64,
    /// Share of summed speaker time, including overlaps for each speaker.
    pub share_percent: f64,
    pub segment_count: usize,
    pub turns: usize,
    pub longest_monologue_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimelineSegment {
    pub id: Uuid,
    pub speaker: String,
    pub name: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpeakerTimeline {
    pub duration_ms: u64,
    pub speakers: Vec<SpeakerStats>,
    pub segments: Vec<TimelineSegment>,
}

impl Transcript {
    pub fn speaker_ids(&self) -> BTreeSet<String> {
        self.segments.iter().map(|s| s.speaker.0.clone()).collect()
    }

    pub fn new_speaker_id(&self) -> String {
        let ids = self.speaker_ids();
        (1..)
            .map(|n| format!("s{n}"))
            .find(|id| {
                !ids.contains(id)
                    && !self.speaker_names.contains_key(id)
                    && !self.speaker_suggestions.contains_key(id)
                    && !self.speaker_voice_ids.contains_key(id)
                    && self.self_speaker.as_ref() != Some(id)
            })
            .expect("finite speaker set")
    }

    pub fn merge_speakers(&mut self, from: &str, into: &str) -> Result<MergeUndo> {
        let ids = self.speaker_ids();
        if from == into || !ids.contains(from) || !ids.contains(into) {
            return Err(Error::SpeakerEdit(
                "Укажите двух разных спикеров встречи".into(),
            ));
        }
        let before = self.clone();
        for segment in &mut self.segments {
            if segment.speaker.0 == from {
                segment.speaker.0 = into.into();
                segment.edited = true;
            }
        }
        if let Some(name) = self.speaker_names.remove(from)
            && !self.speaker_names.contains_key(into)
        {
            self.speaker_names.insert(into.into(), name);
            // A confirmed source identity takes precedence over a target guess.
            self.speaker_voice_ids.remove(into);
        }
        self.speaker_suggestions.remove(from);
        if self.speaker_names.contains_key(into) {
            self.speaker_suggestions.remove(into);
        }
        if let Some(id) = self.speaker_voice_ids.remove(from) {
            self.speaker_voice_ids.entry(into.into()).or_insert(id);
        }
        if self.self_speaker.as_deref() == Some(from) || from == "me" {
            self.self_speaker = Some(into.into());
        }
        Ok(MergeUndo {
            before,
            after: self.clone(),
        })
    }

    /// Preserve text corrections made after the merge; reject intervening speaker edits.
    pub fn undo_merge(&mut self, undo: &MergeUndo) -> Result<()> {
        if self.speaker_names != undo.after.speaker_names
            || self.speaker_suggestions != undo.after.speaker_suggestions
            || self.speaker_voice_ids != undo.after.speaker_voice_ids
            || self.self_speaker != undo.after.self_speaker
            || self.segments.len() != undo.after.segments.len()
            || self
                .segments
                .iter()
                .zip(&undo.after.segments)
                .any(|(a, b)| a.id != b.id || a.speaker != b.speaker)
        {
            return Err(Error::SpeakerEdit(
                "После объединения изменились спикеры".into(),
            ));
        }
        for (segment, original) in self.segments.iter_mut().zip(&undo.before.segments) {
            segment.speaker.clone_from(&original.speaker);
            segment.edited = original.edited || segment.text != original.text;
        }
        self.speaker_names.clone_from(&undo.before.speaker_names);
        self.speaker_suggestions
            .clone_from(&undo.before.speaker_suggestions);
        self.speaker_voice_ids
            .clone_from(&undo.before.speaker_voice_ids);
        self.self_speaker.clone_from(&undo.before.self_speaker);
        Ok(())
    }

    /// IDs, rather than positions, keep a selection stable across rendering and saves.
    pub fn reassign_segments(&mut self, ids: &[Uuid], speaker: &str) -> Result<()> {
        if speaker.trim().is_empty()
            || ids.is_empty()
            || ids
                .iter()
                .any(|id| !self.segments.iter().any(|s| s.id == *id))
        {
            return Err(Error::SpeakerEdit(
                "Некорректный выбор реплик или спикер".into(),
            ));
        }
        let selected: BTreeSet<_> = ids.iter().copied().collect();
        for segment in &mut self.segments {
            if selected.contains(&segment.id) && segment.speaker.0 != speaker {
                segment.speaker.0 = speaker.into();
                segment.edited = true;
            }
        }
        Ok(())
    }

    pub fn speaker_timeline(&self) -> SpeakerTimeline {
        let mut segments: Vec<_> = self.segments.iter().collect();
        segments.sort_by_key(|s| (s.start_ms, s.end_ms, s.id));
        let mut stats = BTreeMap::<String, SpeakerStats>::new();
        let mut runs = BTreeMap::<String, (u64, u64)>::new();
        let mut previous: Option<&str> = None;
        for segment in &segments {
            let id = &segment.speaker.0;
            let stat = stats.entry(id.clone()).or_insert_with(|| SpeakerStats {
                id: id.clone(),
                name: self.speaker_name(id).into(),
                total_ms: 0,
                share_percent: 0.0,
                segment_count: 0,
                turns: 0,
                longest_monologue_ms: 0,
            });
            stat.segment_count += 1;
            // Union within a speaker avoids double-counting duplicate/overlapping chunks.
            let run = runs
                .entry(id.clone())
                .or_insert((segment.start_ms, segment.start_ms));
            stat.total_ms = stat
                .total_ms
                .saturating_add(segment.end_ms.saturating_sub(segment.start_ms.max(run.1)));
            let contiguous =
                previous == Some(id.as_str()) && segment.start_ms <= run.1.saturating_add(1000);
            if !contiguous {
                stat.turns += 1;
                run.0 = segment.start_ms;
            }
            run.1 = run.1.max(segment.end_ms);
            stat.longest_monologue_ms = stat.longest_monologue_ms.max(run.1.saturating_sub(run.0));
            previous = Some(id);
        }
        let total: u64 = stats.values().map(|s| s.total_ms).sum();
        for stat in stats.values_mut() {
            if total > 0 {
                stat.share_percent = stat.total_ms as f64 * 100.0 / total as f64;
            }
        }
        SpeakerTimeline {
            duration_ms: segments.iter().map(|s| s.end_ms).max().unwrap_or(0),
            speakers: stats.into_values().collect(),
            segments: segments
                .into_iter()
                .map(|s| TimelineSegment {
                    id: s.id,
                    speaker: s.speaker.0.clone(),
                    name: self.speaker_name(&s.speaker.0).into(),
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    text: s.text.clone(),
                })
                .collect(),
        }
    }
}
