//! CPU-only diarization stays in the isolated worker, including voice biometrics.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::{Diarization, SpeakerTurn, TimeRange};
use sherpa_onnx::{
    OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig, SpeakerEmbeddingExtractor,
    SpeakerEmbeddingExtractorConfig,
};

use crate::models;

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

fn extractor_config(threads: i32) -> Result<SpeakerEmbeddingExtractorConfig> {
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
    config.clustering.num_clusters = speakers.map_or(-1, |n| n as i32);
    config.clustering.threshold = 0.5;
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
        let next = format!("s{}", ids.len() + 1);
        let speaker = ids.entry(turn.speaker).or_insert(next).clone();
        turns.push(SpeakerTurn {
            start_ms: (turn.start * 1000.0) as u64,
            end_ms: ((turn.end * 1000.0) as u64).min(samples.len() as u64 / 16),
            speaker,
        });
    }
    drop(result);
    drop(diarizer);
    let mut result = Diarization {
        turns,
        ..Default::default()
    };
    if embeddings {
        let extractor =
            SpeakerEmbeddingExtractor::create(&config.embedding).context("load voice extractor")?;
        for id in ids.values() {
            let ranges: Vec<_> = result
                .turns
                .iter()
                .filter(|t| &t.speaker == id)
                .filter(|t| {
                    !result.turns.iter().any(|other| {
                        other.speaker != t.speaker
                            && other.start_ms < t.end_ms
                            && t.start_ms < other.end_ms
                    })
                })
                .map(|t| TimeRange {
                    start_ms: t.start_ms,
                    end_ms: t.end_ms,
                })
                .collect();
            if let Ok(embedding) = mean_embedding(&extractor, &samples, &ranges) {
                result.embeddings.insert(id.clone(), embedding);
            }
        }
    }
    Ok(result)
}
