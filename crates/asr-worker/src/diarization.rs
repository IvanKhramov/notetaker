//! CPU-only diarization stays in the isolated worker, including voice biometrics.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::{Diarization, SpeakerTurn, TimeRange};
use sherpa_onnx::{
    OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig, SpeakerEmbeddingExtractor,
    SpeakerEmbeddingExtractorConfig,
};

use crate::models;

#[path = "diarization/clustering.rs"]
pub(crate) mod clustering;

/// Sherpa 2.0.0 uses complete-linkage cosine distance (1 - cosine).
/// Keep the upstream ERes2Net default; raising it globally can merge real voices.
const CLUSTER_DISTANCE: f32 = 0.5;

fn waveform(path: &Path) -> Result<Vec<f32>> {
    let mut wav = hound::WavReader::open(path)?;
    let spec = wav.spec();
    ensure!(
        spec.channels == 1 && spec.sample_rate == 16000,
        "expected mono 16 kHz WAV"
    );
    ensure!(
        wav.duration() <= i32::MAX as u32,
        "recording too long for sherpa"
    );
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => wav.samples::<f32>().collect::<Result<_, _>>()?,
        (hound::SampleFormat::Int, 16) => wav
            .samples::<i16>()
            .map(|v| v.map(|s| f32::from(s) / 32768.0))
            .collect::<Result<_, _>>()?,
        _ => anyhow::bail!("expected f32 or i16 PCM WAV"),
    };
    ensure!(
        samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
        "invalid PCM"
    );
    Ok(samples)
}

pub(crate) fn extractor_config(threads: i32) -> Result<SpeakerEmbeddingExtractorConfig> {
    let root = models::models_dir()?;
    let path = models::installed(&root, &models::ERES2NET)?.join(models::ERES2NET.files[0]);
    Ok(SpeakerEmbeddingExtractorConfig {
        model: Some(path.to_string_lossy().into()),
        num_threads: threads,
        ..Default::default()
    })
}

fn normalize(mut embedding: Vec<f32>) -> Option<Vec<f32>> {
    let norm = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm < 1e-6 {
        return None;
    }
    for v in &mut embedding {
        *v /= norm;
    }
    Some(embedding)
}

/// Average normalized embeddings from up to eight turns, at most 15 s each.
/// Short turns and overlap are poor enrollment evidence and are skipped.
fn mean_embedding(
    extractor: &SpeakerEmbeddingExtractor,
    samples: &[f32],
    ranges: &[TimeRange],
) -> Result<Vec<f32>> {
    let mut sum = vec![0.0; extractor.dim() as usize];
    let mut count = 0;
    for range in ranges
        .iter()
        .filter(|r| r.end_ms.saturating_sub(r.start_ms) >= 1000)
        .take(8)
    {
        let start = range.start_ms.saturating_mul(16).min(samples.len() as u64) as usize;
        let end = range.end_ms.saturating_mul(16).min(samples.len() as u64) as usize;
        if end.saturating_sub(start) < 16000 {
            continue;
        }
        let end = end.min(start + 15 * 16000);
        let stream = extractor
            .create_stream()
            .context("create embedding stream")?;
        stream.accept_waveform(16000, &samples[start..end]);
        stream.input_finished();
        if !extractor.is_ready(&stream) {
            continue;
        }
        let Some(embedding) = extractor.compute(&stream).and_then(normalize) else {
            continue;
        };
        for (s, v) in sum.iter_mut().zip(embedding) {
            *s += v;
        }
        count += 1;
    }
    ensure!(count > 0, "not enough clean speech to enroll a voice");
    normalize(sum).context("invalid mean embedding")
}

/// Keep the clean parts of a turn, rather than rejecting a whole long turn
/// because of one brief overlap. Prefer long evidence for stable centroids.
pub(crate) fn clean_ranges(turns: &[SpeakerTurn], id: &str) -> Vec<TimeRange> {
    let mut ranges = Vec::new();
    for turn in turns.iter().filter(|t| t.speaker == id) {
        let mut parts = vec![(turn.start_ms, turn.end_ms)];
        for other in turns.iter().filter(|t| t.speaker != id) {
            parts = parts
                .into_iter()
                .flat_map(|(start, end)| {
                    if other.start_ms >= end || other.end_ms <= start {
                        vec![(start, end)]
                    } else {
                        [
                            (start, other.start_ms.min(end)),
                            (other.end_ms.max(start), end),
                        ]
                        .into_iter()
                        .filter(|(a, b)| b > a)
                        .collect()
                    }
                })
                .collect();
        }
        ranges.extend(
            parts
                .into_iter()
                .map(|(start_ms, end_ms)| TimeRange { start_ms, end_ms }),
        );
    }
    ranges.sort_by_key(|r| std::cmp::Reverse(r.end_ms - r.start_ms));
    ranges
}

pub(crate) fn cluster_embedding(
    extractor: &SpeakerEmbeddingExtractor,
    samples: &[f32],
    ranges: &[TimeRange],
) -> Option<Vec<f32>> {
    if let Ok(embedding) = mean_embedding(extractor, samples, ranges) {
        return Some(embedding);
    }
    // Brief clusters may have no one-second turn. Concatenate only their clean
    // speech for comparison, never for voice enrollment. Weak matches stay put.
    let mut speech = Vec::new();
    for range in ranges.iter().filter(|r| r.end_ms - r.start_ms >= 250) {
        let start = (range.start_ms.saturating_mul(16)).min(samples.len() as u64) as usize;
        let end = (range.end_ms.saturating_mul(16)).min(samples.len() as u64) as usize;
        speech.extend_from_slice(&samples[start..end.min(start + 15 * 16000)]);
        if speech.len() >= 15 * 16000 {
            break;
        }
    }
    if speech.len() < 8000 {
        return None;
    }
    let stream = extractor.create_stream()?;
    stream.accept_waveform(16000, &speech);
    stream.input_finished();
    extractor
        .is_ready(&stream)
        .then(|| extractor.compute(&stream))
        .flatten()
        .and_then(normalize)
}

/// Fresh embeddings for individual turns allow old mixed labels to split.
/// Only non-overlapped speech trains the clustering; cap comparison audio at 10 s.
pub(crate) fn comparison_evidence(
    turns: &[SpeakerTurn],
    samples: &[f32],
    extractor: &SpeakerEmbeddingExtractor,
) -> (Vec<clustering::Evidence>, BTreeMap<String, Vec<f32>>) {
    let mut ranges = BTreeMap::new();
    for turn in turns {
        ranges
            .entry(turn.speaker.clone())
            .or_insert_with(|| clean_ranges(turns, &turn.speaker));
    }
    let evidence = turns
        .iter()
        .map(|turn| {
            let clean: Vec<_> = ranges[&turn.speaker]
                .iter()
                .filter_map(|r| {
                    let start_ms = r.start_ms.max(turn.start_ms);
                    let end_ms = r.end_ms.min(turn.end_ms).min(start_ms + 10000);
                    (end_ms > start_ms).then_some(TimeRange { start_ms, end_ms })
                })
                .collect();
            clustering::Evidence {
                clean_ms: clean.iter().map(|r| r.end_ms - r.start_ms).sum(),
                embedding: cluster_embedding(extractor, samples, &clean),
            }
        })
        .collect();
    let fallback = ranges
        .into_iter()
        .filter_map(|(id, clean)| {
            let clean: Vec<_> = clean
                .into_iter()
                .map(|r| TimeRange {
                    start_ms: r.start_ms,
                    end_ms: r.end_ms.min(r.start_ms + 10000),
                })
                .collect();
            let embedding = cluster_embedding(extractor, samples, &clean).or_else(|| {
                // Assignment only: even fully overlapped or very brief groups need
                // a comparison vector. These samples never train the clusters.
                let mut speech = Vec::new();
                for turn in turns.iter().filter(|t| t.speaker == id) {
                    let start =
                        (turn.start_ms.saturating_mul(16)).min(samples.len() as u64) as usize;
                    let end = (turn.end_ms.saturating_mul(16)).min(samples.len() as u64) as usize;
                    speech.extend_from_slice(&samples[start..end.min(start + 10 * 16000)]);
                    if speech.len() >= 10 * 16000 {
                        break;
                    }
                }
                let stream = extractor.create_stream()?;
                stream.accept_waveform(16000, &speech);
                stream.input_finished();
                extractor
                    .is_ready(&stream)
                    .then(|| extractor.compute(&stream))
                    .flatten()
                    .and_then(normalize)
            });
            embedding.map(|e| (id, e))
        })
        .collect();
    (evidence, fallback)
}

pub fn embed(path: &Path, ranges: &[TimeRange], threads: i32) -> Result<Vec<f32>> {
    let samples = waveform(path)?;
    let extractor = SpeakerEmbeddingExtractor::create(&extractor_config(threads)?)
        .context("load embedding extractor")?;
    mean_embedding(&extractor, &samples, ranges)
}

pub fn diarize(
    path: &Path,
    speakers: Option<u32>,
    embeddings: bool,
    threads: i32,
) -> Result<Diarization> {
    ensure!(
        speakers.is_none_or(|n| (1..=32).contains(&n)),
        "speakers must be 1..32 or null"
    );
    let root = models::models_dir()?;
    let segmentation = models::installed(&root, &models::PYANNOTE)?.join("model.int8.onnx");
    let mut config = OfflineSpeakerDiarizationConfig::default();
    config.segmentation.pyannote.model = Some(segmentation.to_string_lossy().into());
    config.segmentation.num_threads = threads;
    config.embedding = extractor_config(threads)?;
    // Preserve the same initial turns in auto and fixed mode. Fixed N is
    // applied to clean turn embeddings below, not sherpa complete linkage.
    config.clustering.num_clusters = -1;
    config.clustering.threshold = CLUSTER_DISTANCE;
    let samples = waveform(path)?;
    if samples.len() < 16000 {
        return Ok(Diarization::default());
    }
    let diarizer = OfflineSpeakerDiarization::create(&config).context("load diarization models")?;
    ensure!(
        diarizer.sample_rate() == 16000,
        "unexpected diarization sample rate"
    );
    let result = diarizer.process(&samples).context("diarization failed")?;
    let mut ids = BTreeMap::new();
    let mut turns = Vec::new();
    for turn in result.sort_by_start_time() {
        if !turn.start.is_finite()
            || !turn.end.is_finite()
            || turn.start < 0.0
            || turn.end <= turn.start
            || turn.speaker < 0
        {
            continue;
        }
        let start_ms = (turn.start * 1000.0) as u64;
        let end_ms = ((turn.end * 1000.0) as u64).min(samples.len() as u64 / 16);
        if end_ms <= start_ms {
            continue;
        }
        let next = format!("s{}", ids.len() + 1);
        let speaker = ids.entry(turn.speaker).or_insert(next).clone();
        turns.push(SpeakerTurn {
            start_ms,
            end_ms,
            speaker,
        });
    }
    drop(result);
    drop(diarizer);
    let mut result = Diarization {
        turns,
        ..Default::default()
    };
    refine(&mut result, &samples, speakers, embeddings, threads)?;
    Ok(result)
}

/// Shared with the ignored evaluation so segmentation results can be cached.
pub(crate) fn refine(
    result: &mut Diarization,
    samples: &[f32],
    speakers: Option<u32>,
    embeddings: bool,
    threads: i32,
) -> Result<()> {
    let ids: std::collections::BTreeSet<_> =
        result.turns.iter().map(|t| t.speaker.clone()).collect();
    if embeddings || ids.len() > 1 || (speakers.is_some() && !ids.is_empty()) {
        let extractor = SpeakerEmbeddingExtractor::create(&extractor_config(threads)?)
            .context("load voice extractor")?;
        if ids.len() > 1 || speakers.is_some() {
            let (evidence, fallback) = comparison_evidence(&result.turns, samples, &extractor);
            clustering::reassign(&mut result.turns, &evidence, &fallback, speakers);
        }
        // Number the surviving voices by first appearance. Comparison vectors
        // remain worker-local when the voice library is disabled.
        let mut names = BTreeMap::new();
        for turn in &mut result.turns {
            let next = format!("s{}", names.len() + 1);
            turn.speaker = names.entry(turn.speaker.clone()).or_insert(next).clone();
        }
        if embeddings {
            for id in names.values() {
                let ranges = clean_ranges(&result.turns, id);
                if let Ok(embedding) = mean_embedding(&extractor, samples, &ranges) {
                    result.embeddings.insert(id.clone(), embedding);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_parts_survive_brief_nested_and_adjacent_overlaps() {
        let turn = |id: &str, start_ms, end_ms| SpeakerTurn {
            speaker: id.into(),
            start_ms,
            end_ms,
        };
        let turns = [
            turn("a", 0, 10000),
            turn("b", 2000, 3000),
            turn("c", 2500, 4000),
            turn("b", 10000, 11000),
        ];
        let ranges = clean_ranges(&turns, "a");
        assert_eq!(
            ranges,
            [
                TimeRange {
                    start_ms: 4000,
                    end_ms: 10000
                },
                TimeRange {
                    start_ms: 0,
                    end_ms: 2000
                }
            ]
        );
    }
}
