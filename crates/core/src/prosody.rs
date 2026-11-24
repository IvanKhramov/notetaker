//! Per-segment measurements and meeting-relative markers for chat.
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Channel, Error, Meeting, Result, Segment, Transcript, storage};

pub mod thresholds {
    pub const MIN_METRIC_MS: u64 = 3_000;
    pub const MIN_BASELINE_SEGMENTS: usize = 5;
    pub const MIN_BASELINE_MS: u64 = 60_000;
    pub const LOUDNESS_DELTA_DB: f32 = 6.0;
    pub const PITCH_DELTA_ST: f32 = 3.0;
    pub const PITCH_SPREAD_DELTA_ST: f32 = 3.0;
    pub const TEMPO_RATIO: f32 = 1.3;
    pub const MAD_SCALE: f32 = 1.4826;
    pub const MAD_SIGMAS: f32 = 3.0;
    pub const MIN_INTERRUPTION_OVERLAP_MS: u64 = 300;
    pub const INTERRUPTION_WINDOW_MS: u64 = 1_500;
    pub const MIN_INTERRUPTER_MS: u64 = 1_000;
    pub const UTTERANCE_GAP_MS: u64 = 500;

    pub const FRAME_MS: usize = 40;
    pub const MIN_SPEECH_DBFS: f32 = -50.0;
    pub const MIN_VOICED_FRAMES: usize = 3;
    pub const MIN_F0_HZ: usize = 60;
    pub const MAX_F0_HZ: usize = 500;
    pub const MAX_FRAME_F0_RATIO: f32 = 1.8;
    pub const YIN_THRESHOLD: f32 = 0.15;
}
use thresholds::*;

const VERSION: u32 = 1;
// Bump when the acoustic or tempo measurement changes.
pub const MEASUREMENT_REVISION: u32 = 2;

const fn legacy_measurement_revision() -> u32 {
    1
}

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
    #[serde(default = "legacy_measurement_revision")]
    algorithm: u32,
    pub segments: BTreeMap<Uuid, SegmentProsody>,
}

impl Default for Prosody {
    fn default() -> Self {
        Self {
            version: VERSION,
            algorithm: MEASUREMENT_REVISION,
            segments: BTreeMap::new(),
        }
    }
}

impl Meeting {
    pub fn prosody_needs_recompute(&self) -> Result<bool> {
        Ok(self
            .read_prosody()?
            .is_none_or(|prosody| prosody.algorithm < MEASUREMENT_REVISION))
    }

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

#[derive(Clone, Copy)]
struct MetricBaseline {
    median: f32,
    scaled_mad: f32,
}

impl MetricBaseline {
    fn from_samples(mut samples: Vec<f32>) -> Option<Self> {
        if samples.len() < MIN_BASELINE_SEGMENTS {
            return None;
        }
        samples.sort_unstable_by(f32::total_cmp);
        let median = quantile(&samples, 0.5);
        for sample in &mut samples {
            *sample = (*sample - median).abs();
        }
        samples.sort_unstable_by(f32::total_cmp);
        Some(Self {
            median,
            scaled_mad: MAD_SCALE * quantile(&samples, 0.5),
        })
    }
}

struct Marker {
    label: String,
    detail: String,
}

fn speaker_known(transcript: &Transcript, segment: &Segment) -> bool {
    // "them" is the unresolved system track unless the user named it.
    // Rediarize may append underscores to avoid a collision with a mic identity.
    segment.channel == Channel::Mic
        || segment.speaker.0.trim_end_matches('_') != "them"
        || transcript.speaker_names.contains_key(&segment.speaker.0)
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
                    let line = format!(
                        "[{}] {name} [{labels}]: {}",
                        &time[..time.len() - 4],
                        segment.text
                    );
                    Some(if diagnostics {
                        line.replace(['\r', '\n'], " ")
                    } else {
                        line
                    })
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
            .filter(|s| speaker_known(transcript, s))
            .collect();
        by_start.sort_by_key(|s| s.start_ms);
        let mut baselines = HashMap::<&str, Baseline>::new();
        for s in &by_start {
            // Older sidecars may still contain measurements for short segments.
            if s.end_ms.saturating_sub(s.start_ms) < MIN_METRIC_MS {
                continue;
            }
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
        let baselines: HashMap<_, _> = baselines
            .into_iter()
            .filter(|(_, baseline)| baseline.duration_ms >= MIN_BASELINE_MS)
            .filter_map(|(speaker, baseline)| {
                let metrics = baseline.values.map(MetricBaseline::from_samples);
                metrics
                    .iter()
                    .any(Option::is_some)
                    .then_some((speaker, metrics))
            })
            .collect();
        let interruptions = interruptions(&utterances(&by_start));
        by_start
            .into_iter()
            .filter_map(|s| {
                let baseline = baselines.get(s.speaker.0.as_str())?;
                // Short segments have no measurements but may still interrupt.
                let mut markers = match self.segments.get(&s.id) {
                    Some(raw) if s.end_ms.saturating_sub(s.start_ms) >= MIN_METRIC_MS => {
                        metric_markers(raw, baseline)
                    }
                    _ => Vec::new(),
                };
                if let Some(&(speaker, overlap)) = interruptions.get(&s.id) {
                    let name = transcript.speaker_name(speaker);
                    markers.push(Marker {
                        label: format!("перебивает спикера «{name}»"),
                        detail: format!("пересечение {overlap} мс"),
                    });
                }
                (!markers.is_empty()).then_some((s.id, markers))
            })
            .collect()
    }
}

/// One speaker's continuous speech. ASR may split it into segments separated
/// by short gaps.
struct Utterance<'a> {
    speaker: &'a str,
    start_ms: u64,
    end_ms: u64,
    first_segment: Uuid,
}

/// Merge same-speaker segments with a gap of at most `UTTERANCE_GAP_MS`.
/// `by_start` is sorted by start time, so the result is too.
fn utterances<'a>(by_start: &[&'a Segment]) -> Vec<Utterance<'a>> {
    let mut result: Vec<Utterance<'a>> = Vec::new();
    let mut open = HashMap::<&str, usize>::new();
    for s in by_start {
        let speaker = s.speaker.0.as_str();
        if let Some(&index) = open.get(speaker) {
            let utterance = &mut result[index];
            if s.start_ms.saturating_sub(utterance.end_ms) <= UTTERANCE_GAP_MS {
                utterance.end_ms = utterance.end_ms.max(s.end_ms);
                continue;
            }
        }
        open.insert(speaker, result.len());
        result.push(Utterance {
            speaker,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            first_segment: s.id,
        });
    }
    result
}

/// For the first segment of each interrupting utterance: the interrupted
/// speaker and the overlap. B starts while A speaks, A's utterance ends
/// 300..1500 ms later and not after B's, and B is not a short backchannel.
fn interruptions<'a>(utterances: &[Utterance<'a>]) -> HashMap<Uuid, (&'a str, u64)> {
    let mut result = HashMap::new();
    for (index, b) in utterances.iter().enumerate() {
        if b.end_ms.saturating_sub(b.start_ms) < MIN_INTERRUPTER_MS {
            continue;
        }
        let interrupted = utterances[..index]
            .iter()
            .filter(|a| a.speaker != b.speaker && a.start_ms < b.start_ms && a.end_ms <= b.end_ms)
            .map(|a| (a, a.end_ms.saturating_sub(b.start_ms)))
            .filter(|&(_, overlap)| {
                (MIN_INTERRUPTION_OVERLAP_MS..=INTERRUPTION_WINDOW_MS).contains(&overlap)
            })
            .max_by_key(|&(a, _)| a.end_ms);
        if let Some((a, overlap)) = interrupted {
            result.insert(b.first_segment, (a.speaker, overlap));
        }
    }
    result
}

fn metric_markers(raw: &SegmentProsody, baseline: &[Option<MetricBaseline>; 4]) -> Vec<Marker> {
    let definitions = [
        (
            LOUDNESS_DELTA_DB,
            "заметно громче своей нормы",
            Some("заметно тише своей нормы"),
        ),
        (
            PITCH_DELTA_ST,
            "голос выше обычного",
            Some("голос ниже обычного"),
        ),
        (
            PITCH_SPREAD_DELTA_ST,
            "шире диапазон высоты голоса",
            Some("уже диапазон высоты голоса"),
        ),
        (TEMPO_RATIO.ln(), "быстрее обычного", None),
    ];
    raw.values()
        .into_iter()
        .zip(baseline)
        .zip(definitions)
        .enumerate()
        .filter_map(|(i, ((value, baseline), (threshold, high, low)))| {
            let value = value?;
            let &MetricBaseline { median, scaled_mad } = baseline.as_ref()?;
            let delta = value - median;
            if delta.abs() <= threshold.max(MAD_SIGMAS * scaled_mad) {
                return None;
            }
            let label = if delta > 0.0 { high } else { low? };
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
                label: label.into(),
                detail: format!("{value:.1} / {median:.1} {unit}"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Source, SpeakerId};

    #[test]
    fn recompute_detects_missing_legacy_and_current_measurements() {
        let root = tempfile::tempdir().unwrap();
        let library = crate::Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(crate::MeetingMeta::new("Prosody", chrono::Utc::now()))
            .unwrap();
        let path = meeting.path().join("prosody.json");
        assert!(meeting.prosody_needs_recompute().unwrap());
        std::fs::write(&path, r#"{"version":1,"segments":{}}"#).unwrap();
        assert_eq!(meeting.read_prosody().unwrap().unwrap().algorithm, 1);
        assert!(meeting.prosody_needs_recompute().unwrap());
        std::fs::write(&path, r#"{"version":1,"algorithm":1,"segments":{}}"#).unwrap();
        assert!(meeting.prosody_needs_recompute().unwrap());
        meeting.write_prosody(&Prosody::default()).unwrap();
        let saved = meeting.read_prosody().unwrap().unwrap();
        assert_eq!(saved.algorithm, MEASUREMENT_REVISION);
        assert!(!meeting.prosody_needs_recompute().unwrap());
    }

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
    fn short_segments_from_older_sidecars_do_not_count_toward_baselines() {
        let (mut transcript, prosody) = fixture();
        transcript.segments[0].end_ms = MIN_METRIC_MS - 1;
        transcript.segments[1].start_ms = 3_000;
        transcript.segments[1].end_ms = 29_000;
        assert_eq!(prosody.annotated_text(&transcript), transcript.named_text());
    }

    #[test]
    fn adapts_to_the_speakers_natural_variation() {
        let (transcript, mut prosody) = fixture();
        for (segment, db) in transcript
            .segments
            .iter()
            .zip([-42.0, -36.0, -30.0, -24.0, -18.0])
        {
            prosody.segments.insert(
                segment.id,
                SegmentProsody {
                    loudness_dbfs: Some(db),
                    ..Default::default()
                },
            );
        }
        assert_eq!(prosody.annotated_text(&transcript), transcript.named_text());
        prosody
            .segments
            .get_mut(&transcript.segments[4].id)
            .unwrap()
            .loudness_dbfs = Some(-2.0);
        assert!(
            prosody
                .diagnostic_text(&transcript)
                .contains("заметно громче")
        );
    }

    #[test]
    fn only_fast_tempo_is_marked() {
        let (transcript, mut prosody) = fixture();
        for segment in &transcript.segments {
            prosody.segments.insert(
                segment.id,
                SegmentProsody {
                    chars_per_second: Some(10.0),
                    ..Default::default()
                },
            );
        }
        prosody
            .segments
            .get_mut(&transcript.segments[4].id)
            .unwrap()
            .chars_per_second = Some(0.6);
        assert!(prosody.diagnostic_text(&transcript).is_empty());
    }

    #[test]
    fn unresolved_system_speaker_counts_only_once_named() {
        let (mut transcript, prosody) = fixture();
        transcript.speaker_names.clear();
        for s in &mut transcript.segments {
            s.speaker = SpeakerId("them".into());
            s.channel = Channel::System;
        }
        assert_eq!(prosody.annotated_text(&transcript), transcript.named_text());
        transcript.rename_speaker("them", "Анна");
        assert!(
            prosody
                .annotated_text(&transcript)
                .contains("Анна [заметно громче своей нормы")
        );
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
    fn interruptions_use_utterances_not_asr_segments() {
        let interrupted = |extra: &[(&str, u64, u64)]| {
            let (mut transcript, prosody) = fixture();
            transcript.rename_speaker("b", "Никита");
            for &(speaker, start, end) in extra {
                transcript
                    .segments
                    .push(segment(speaker, Channel::System, start, end));
            }
            prosody.annotated_text(&transcript).contains("перебивает")
        };
        // Алёна starts at 60 s. A gap of exactly 500 ms keeps Никита's utterance whole.
        assert!(interrupted(&[("b", 56_500, 59_500), ("b", 60_000, 61_200)]));
        // An 800 ms pause ends the utterance: Никита stopped, a later restart is new speech.
        assert!(interrupted(&[("b", 58_000, 60_500), ("b", 61_300, 63_000)]));
        // Алёна was already speaking before Никита: her later ASR segment is no interruption.
        assert!(!interrupted(&[
            ("a", 57_000, 60_000),
            ("b", 58_000, 61_000)
        ]));
        // A short first segment has no measurements but still carries the marker.
        assert!(interrupted(&[("b", 58_000, 60_500), ("a", 59_900, 61_000)]));
    }

    #[test]
    fn interruption_requires_overlap_a_quick_exit_and_a_non_backchannel() {
        let (mut transcript, mut prosody) = fixture();
        transcript.rename_speaker("b", "Никита");
        let a = segment("b", Channel::System, 59_000, 60_299);
        transcript.segments.insert(4, a);
        assert!(!prosody.annotated_text(&transcript).contains("перебивает"));
        transcript.segments[4].end_ms = 60_300;
        assert!(
            prosody
                .annotated_text(&transcript)
                .contains("перебивает спикера «Никита»")
        );
        // ASR split Никита's utterance: he goes on talking, so no interruption.
        transcript
            .segments
            .push(segment("b", Channel::System, 60_400, 65_000));
        assert!(!prosody.annotated_text(&transcript).contains("перебивает"));
        transcript.segments.pop();
        // A split utterance that ends inside the window is still interrupted.
        transcript
            .segments
            .push(segment("b", Channel::System, 60_300, 61_000));
        assert!(
            prosody
                .annotated_text(&transcript)
                .contains("перебивает спикера «Никита»")
        );
        transcript.segments.pop();

        let b = &mut transcript.segments[5];
        b.channel = Channel::System;
        assert!(
            prosody
                .annotated_text(&transcript)
                .contains("перебивает спикера «Никита»")
        );
        let b = &mut transcript.segments[5];
        b.end_ms = b.start_ms + MIN_INTERRUPTER_MS - 1;
        // Keep enough audio from this speaker for the duration threshold, with
        // gaps so her earlier segments stay separate utterances.
        for segment in &mut transcript.segments[..4] {
            segment.end_ms = segment.start_ms + 14_000;
        }
        let extra = segment("a", Channel::Mic, 80_000, 90_000);
        prosody.segments.insert(
            extra.id,
            SegmentProsody {
                loudness_dbfs: Some(-30.0),
                ..Default::default()
            },
        );
        transcript.segments.push(extra);
        let text = prosody.annotated_text(&transcript);
        assert!(!text.contains("быстрее обычного"));
        assert!(!text.contains("перебивает"));
        transcript.segments[5].end_ms = 72_000;
        transcript.segments[4].end_ms = 61_500;
        assert!(prosody.annotated_text(&transcript).contains("перебивает"));
        transcript.segments[4].end_ms = 61_501;
        assert!(!prosody.annotated_text(&transcript).contains("перебивает"));
    }
}
