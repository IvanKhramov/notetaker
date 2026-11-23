//! Per-segment measurements and meeting-relative markers for chat.
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Channel, Error, Meeting, Result, Segment, Transcript, storage};

pub mod thresholds {
    pub const MIN_TEMPO_MS: u64 = 1_500;
    pub const MIN_BASELINE_SEGMENTS: usize = 5;
    pub const MIN_BASELINE_MS: u64 = 60_000;
    pub const LOUDNESS_DELTA_DB: f32 = 6.0;
    pub const PITCH_DELTA_ST: f32 = 3.0;
    pub const PITCH_SPREAD_DELTA_ST: f32 = 3.0;
    pub const TEMPO_RATIO: f32 = 1.3;
    pub const INTERRUPTION_WINDOW_MS: u64 = 500;
    pub const MIN_INTERRUPTER_MS: u64 = 1_000;

    pub const FRAME_MS: usize = 40;
    pub const MIN_SPEECH_DBFS: f32 = -50.0;
    pub const MIN_VOICED_FRAMES: usize = 3;
    pub const MIN_F0_HZ: usize = 60;
    pub const MAX_F0_HZ: usize = 500;
    pub const YIN_THRESHOLD: f32 = 0.15;
}
use thresholds::*;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SegmentProsody {
    pub loudness_dbfs: Option<f32>,
    pub f0_hz: Option<f32>,
    pub f0_iqr_st: Option<f32>,
    pub chars_per_second: Option<f32>,
    pub same_channel_overlap: bool,
}

impl SegmentProsody {
    fn values(&self) -> [Option<f32>; 4] {
        let clean = !self.same_channel_overlap;
        [
            self.loudness_dbfs.filter(|_| clean),
            self.f0_hz
                .filter(|&hz| clean && hz > 0.0)
                .map(|hz| 12.0 * hz.log2()),
            self.f0_iqr_st.filter(|_| clean),
            self.chars_per_second
                .filter(|&tempo| tempo > 0.0)
                .map(f32::ln),
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prosody {
    version: u32,
    pub segments: BTreeMap<Uuid, SegmentProsody>,
}

impl Default for Prosody {
    fn default() -> Self {
        Self {
            version: VERSION,
            segments: BTreeMap::new(),
        }
    }
}

impl Meeting {
    pub fn read_prosody(&self) -> Result<Option<Prosody>> {
        let path = self.source_path("prosody.json")?;
        let Some(bytes) = storage::read_bytes(&path)? else {
            return Ok(None);
        };
        let prosody: Prosody = serde_json::from_slice(&bytes)?;
        if prosody.version != VERSION {
            return Err(Error::UnsupportedProsodyVersion(prosody.version));
        }
        Ok(Some(prosody))
    }

    pub fn write_prosody(&self, prosody: &Prosody) -> Result<()> {
        storage::atomic_write(
            &self.path().join("prosody.json"),
            &serde_json::to_vec_pretty(prosody)?,
        )
    }
}

pub fn quantile(sorted: &[f32], fraction: f32) -> f32 {
    let index = (sorted.len() - 1) as f32 * fraction;
    let lower = index as usize;
    sorted[lower] + (sorted[index.ceil() as usize] - sorted[lower]) * index.fract()
}

#[derive(Default)]
struct Baseline {
    values: [Vec<f32>; 4],
    duration_ms: u64,
    end_ms: u64,
}

struct Marker {
    label: String,
    detail: String,
}

fn speaker_known(segment: &Segment) -> bool {
    // "them" is the unresolved system track. Rediarize may append underscores
    // to avoid a collision with a mic identity.
    segment.channel == Channel::Mic || segment.speaker.0.trim_end_matches('_') != "them"
}

impl Prosody {
    pub fn annotated_text(&self, transcript: &Transcript) -> String {
        self.lines(transcript, false)
    }

    pub fn diagnostic_text(&self, transcript: &Transcript) -> String {
        self.lines(transcript, true)
    }

    fn lines(&self, transcript: &Transcript, diagnostics: bool) -> String {
        let markers = self.markers(transcript);
        transcript
            .segments
            .iter()
            .filter_map(|segment| {
                let name = transcript.speaker_name(&segment.speaker.0);
                if let Some(markers) = markers.get(&segment.id) {
                    let time = crate::format_timestamp(segment.start_ms, '.');
                    let labels = markers
                        .iter()
                        .map(|mark| {
                            if diagnostics {
                                format!("{} ({})", mark.label, mark.detail)
                            } else {
                                mark.label.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    Some(format!(
                        "[{}] {name} [{labels}]: {}",
                        &time[..time.len() - 4],
                        segment.text
                    ))
                } else if diagnostics {
                    None
                } else {
                    Some(format!("{name}: {}", segment.text))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn markers(&self, transcript: &Transcript) -> HashMap<Uuid, Vec<Marker>> {
        let mut by_start: Vec<_> = transcript
            .segments
            .iter()
            .filter(|s| speaker_known(s))
            .collect();
        by_start.sort_by_key(|s| s.start_ms);
        let mut baselines = HashMap::<&str, Baseline>::new();
        for s in &by_start {
            let Some(raw) = self.segments.get(&s.id) else {
                continue;
            };
            let values = raw.values();
            if values.iter().all(Option::is_none) {
                continue;
            }
            let baseline = baselines.entry(&s.speaker.0).or_default();
            baseline.duration_ms += s.end_ms.saturating_sub(s.start_ms.max(baseline.end_ms));
            baseline.end_ms = baseline.end_ms.max(s.end_ms);
            for (samples, value) in baseline.values.iter_mut().zip(values) {
                samples.extend(value);
            }
        }
        baselines.retain(|_, baseline| {
            baseline.duration_ms >= MIN_BASELINE_MS
                && baseline
                    .values
                    .iter()
                    .any(|values| values.len() >= MIN_BASELINE_SEGMENTS)
        });
        for baseline in baselines.values_mut() {
            for values in &mut baseline.values {
                values.sort_by(f32::total_cmp);
            }
        }
        let mut by_end = by_start.clone();
        by_end.sort_by_key(|s| s.end_ms);
        by_start
            .into_iter()
            .filter_map(|s| {
                let raw = self.segments.get(&s.id)?;
                let baseline = baselines.get(s.speaker.0.as_str())?;
                let mut markers = metric_markers(raw, baseline);
                if let Some(previous) = interrupted(s, &by_end) {
                    let name = transcript.speaker_name(&previous.speaker.0);
                    markers.push(Marker {
                        label: format!("перебивает спикера «{name}»"),
                        detail: format!("пересечение {} мс", previous.end_ms - s.start_ms),
                    });
                }
                (!markers.is_empty()).then_some((s.id, markers))
            })
            .collect()
    }
}

fn interrupted<'a>(s: &Segment, by_end: &[&'a Segment]) -> Option<&'a Segment> {
    if s.end_ms.saturating_sub(s.start_ms) < MIN_INTERRUPTER_MS {
        return None;
    }
    let first = by_end.partition_point(|other| other.end_ms <= s.start_ms);
    by_end[first..]
        .iter()
        .copied()
        .take_while(|other| other.end_ms - s.start_ms <= INTERRUPTION_WINDOW_MS)
        .filter(|other| other.start_ms < s.start_ms && other.speaker != s.speaker)
        .max_by_key(|other| other.end_ms)
}

fn metric_markers(raw: &SegmentProsody, baseline: &Baseline) -> Vec<Marker> {
    let definitions = [
        (
            LOUDNESS_DELTA_DB,
            "заметно громче своей нормы",
            "заметно тише своей нормы",
        ),
        (PITCH_DELTA_ST, "голос выше обычного", "голос ниже обычного"),
        (
            PITCH_SPREAD_DELTA_ST,
            "шире диапазон высоты голоса",
            "уже диапазон высоты голоса",
        ),
        (TEMPO_RATIO.ln(), "быстрее обычного", "медленнее обычного"),
    ];
    raw.values()
        .into_iter()
        .zip(&baseline.values)
        .zip(definitions)
        .enumerate()
        .filter_map(|(i, ((value, samples), (threshold, high, low)))| {
            let value = value?;
            if samples.len() < MIN_BASELINE_SEGMENTS {
                return None;
            }
            let median = quantile(samples, 0.5);
            let delta = value - median;
            if delta.abs() < threshold {
                return None;
            }
            let (value, median, unit) = match i {
                0 => (value, median, "дБFS"),
                1 => (
                    2.0_f32.powf(value / 12.0),
                    2.0_f32.powf(median / 12.0),
                    "Гц",
                ),
                2 => (value, median, "пт"),
                _ => (value.exp(), median.exp(), "симв/с"),
            };
            Some(Marker {
                label: if delta > 0.0 { high } else { low }.into(),
                detail: format!("{value:.1} / {median:.1} {unit}"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Source, SpeakerId};

    fn segment(speaker: &str, channel: Channel, start_ms: u64, end_ms: u64) -> Segment {
        Segment {
            id: Uuid::now_v7(),
            speaker: SpeakerId(speaker.into()),
            channel,
            start_ms,
            end_ms,
            text: "Фраза".into(),
            source: Source::Final,
            edited: false,
            lang: None,
        }
    }

    fn fixture() -> (Transcript, Prosody) {
        let mut transcript = Transcript::default();
        transcript.rename_speaker("a", "Алёна");
        let mut prosody = Prosody::default();
        for i in 0..5 {
            let s = segment("a", Channel::Mic, i * 15_000, i * 15_000 + 12_000);
            prosody.segments.insert(
                s.id,
                SegmentProsody {
                    loudness_dbfs: Some(if i == 4 { -18.0 } else { -30.0 }),
                    f0_hz: Some(if i == 4 { 90.0 } else { 120.0 }),
                    f0_iqr_st: Some(if i == 4 { 6.0 } else { 2.0 }),
                    chars_per_second: Some(if i == 4 { 20.0 } else { 10.0 }),
                    same_channel_overlap: false,
                },
            );
            transcript.segments.push(s);
        }
        (transcript, prosody)
    }

    #[test]
    fn renders_only_outliers_against_the_current_speaker_baseline() {
        let (mut transcript, prosody) = fixture();
        let text = prosody.annotated_text(&transcript);
        assert_eq!(
            text.lines().take(4).collect::<Vec<_>>(),
            vec!["Алёна: Фраза"; 4]
        );
        let marked = text.lines().last().unwrap();
        for marker in [
            "[00:01:00] Алёна [",
            "заметно громче",
            "ниже обычного",
            "шире диапазон",
            "быстрее обычного",
        ] {
            assert!(marked.contains(marker), "{marked}");
        }
        assert!(
            prosody
                .diagnostic_text(&transcript)
                .contains("-18.0 / -30.0")
        );
        assert_eq!(
            transcript.named_text().lines().last().unwrap(),
            "Алёна: Фраза"
        );

        transcript.segments[4].speaker = SpeakerId("b".into());
        assert_eq!(prosody.annotated_text(&transcript), transcript.named_text());
    }

    #[test]
    fn too_little_speech_and_missing_measurements_have_no_markers() {
        let (mut transcript, mut prosody) = fixture();
        transcript.segments[0].end_ms -= 1;
        assert_eq!(prosody.annotated_text(&transcript), transcript.named_text());
        transcript.segments[0].end_ms += 1;
        prosody.segments.remove(&transcript.segments[4].id);
        assert_eq!(prosody.annotated_text(&transcript), transcript.named_text());
    }

    #[test]
    fn ignores_acoustics_of_mixed_audio_but_keeps_tempo() {
        let (transcript, mut prosody) = fixture();
        prosody
            .segments
            .get_mut(&transcript.segments[4].id)
            .unwrap()
            .same_channel_overlap = true;
        let text = prosody.annotated_text(&transcript);
        assert_eq!(
            text.lines().last().unwrap(),
            "[00:01:00] Алёна [быстрее обычного]: Фраза"
        );
    }

    #[test]
    fn interruption_requires_overlap_a_quick_exit_and_a_non_backchannel() {
        let (mut transcript, prosody) = fixture();
        transcript.rename_speaker("b", "Никита");
        let a = segment("b", Channel::System, 59_000, 60_200);
        transcript.segments.insert(4, a);
        assert!(
            prosody
                .annotated_text(&transcript)
                .contains("перебивает спикера «Никита»")
        );

        let b = &mut transcript.segments[5];
        b.channel = Channel::System;
        assert!(
            prosody
                .annotated_text(&transcript)
                .contains("перебивает спикера «Никита»")
        );
        let b = &mut transcript.segments[5];
        b.end_ms = b.start_ms + MIN_INTERRUPTER_MS - 1;
        // Keep enough audio from this speaker for the duration threshold.
        for segment in &mut transcript.segments[..4] {
            segment.end_ms = segment.start_ms + 15_000;
        }
        let text = prosody.annotated_text(&transcript);
        assert!(text.contains("быстрее обычного"));
        assert!(!text.contains("перебивает"));
        transcript.segments[5].end_ms = 72_000;
        transcript.segments[4].end_ms = 60_501;
        assert!(!prosody.annotated_text(&transcript).contains("перебивает"));
    }
}
