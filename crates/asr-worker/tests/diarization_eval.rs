//! Run scripts/diarization-fixtures.py first. No network or audio is needed by CI.
use std::{collections::BTreeMap, path::PathBuf};

use notetaker_asr_proto::{Diarization, SpeakerTurn};
use sherpa_onnx::{OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig};

#[allow(dead_code)]
#[path = "../src/diarization.rs"]
mod diarization;
#[path = "../src/diarization/cleanup.rs"]
mod legacy_cleanup;
#[allow(dead_code)]
#[path = "../src/models.rs"]
mod models;

fn counts(result: &Diarization) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for turn in &result.turns {
        *counts.entry(turn.speaker.clone()).or_default() += turn.end_ms - turn.start_ms;
    }
    counts
}

// Many-to-one purity detects false merges; counts separately detect splitting.
fn purity(result: &Diarization, truth: &[serde_json::Value]) -> f64 {
    let mut overlap = BTreeMap::<&str, BTreeMap<u64, u64>>::new();
    for turn in &result.turns {
        for reference in truth {
            let ms = turn
                .end_ms
                .min(reference[1].as_u64().unwrap())
                .saturating_sub(turn.start_ms.max(reference[0].as_u64().unwrap()));
            *overlap
                .entry(&turn.speaker)
                .or_default()
                .entry(reference[2].as_u64().unwrap())
                .or_default() += ms;
        }
    }
    let total: u64 = overlap.values().flat_map(|v| v.values()).sum();
    let correct: u64 = overlap.values().filter_map(|v| v.values().max()).sum();
    correct as f64 / total.max(1) as f64
}

// Best one-to-one mapping of cluster IDs to known voices. Unlike purity this
// penalizes fragmentation as well as merging, including quiet participants.
fn mapped_accuracy(result: &Diarization, truth: &[serde_json::Value], speakers: usize) -> f64 {
    let mut overlap = BTreeMap::<&str, Vec<u64>>::new();
    for turn in &result.turns {
        for reference in truth {
            let ms = turn
                .end_ms
                .min(reference[1].as_u64().unwrap())
                .saturating_sub(turn.start_ms.max(reference[0].as_u64().unwrap()));
            overlap
                .entry(&turn.speaker)
                .or_insert_with(|| vec![0; speakers])[reference[2].as_u64().unwrap() as usize] +=
                ms;
        }
    }
    let total: u64 = overlap.values().flatten().sum();
    let mut best = vec![0; 1 << speakers];
    for scores in overlap.values() {
        let previous = best.clone();
        for (mask, score) in previous.iter().enumerate() {
            for (speaker, ms) in scores.iter().enumerate() {
                if mask & (1 << speaker) == 0 {
                    let next = mask | (1 << speaker);
                    best[next] = best[next].max(score + ms);
                }
            }
        }
    }
    *best.iter().max().unwrap() as f64 / total.max(1) as f64
}

#[test]
#[ignore = "requires diarization models and scripts/diarization-fixtures.py audio"]
fn call_audio_speaker_counts() {
    let root =
        PathBuf::from(std::env::var_os("NOTETAKER_MODELS_DIR").expect("set NOTETAKER_MODELS_DIR"));
    let fixtures = PathBuf::from(
        std::env::var_os("NOTETAKER_DIARIZATION_EVAL_DIR")
            .expect("set NOTETAKER_DIARIZATION_EVAL_DIR"),
    );
    let cases: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixtures.join("manifest.json")).unwrap()).unwrap();
    let threshold: f32 = std::env::var("NOTETAKER_EVAL_THRESHOLD")
        .unwrap_or("0.5".into())
        .parse()
        .unwrap();
    let mut config = OfflineSpeakerDiarizationConfig::default();
    config.segmentation.pyannote.model = Some(
        root.join("pyannote-3.0/model.int8.onnx")
            .to_string_lossy()
            .into(),
    );
    config.segmentation.num_threads = 2;
    config.embedding.model = Some(
        root.join("eres2net-base/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx")
            .to_string_lossy()
            .into(),
    );
    config.embedding.num_threads = 2;
    config.clustering.threshold = threshold;
    let fixed = std::env::var_os("NOTETAKER_EVAL_FIXED").is_some();
    let legacy = std::env::var_os("NOTETAKER_EVAL_LEGACY").is_some();
    let mut report = Vec::new();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if std::env::var("NOTETAKER_EVAL_FILTER").is_ok_and(|filter| !name.contains(&filter)) {
            continue;
        }
        let path = fixtures.join(case["file"].as_str().unwrap());
        let mut wav = hound::WavReader::open(&path).unwrap();
        let samples: Vec<f32> = wav
            .samples::<i16>()
            .map(|s| f32::from(s.unwrap()) / 32768.0)
            .collect();
        let target = fixed.then(|| case["speakers"].as_u64().unwrap() as u32);
        config.clustering.num_clusters = target.map_or(-1, |n| n as i32);
        let mode = target.map_or_else(
            || format!("threshold-{threshold}"),
            |n| format!("fixed-{n}"),
        );
        let cache = fixtures.join(format!("{name}-{mode}.json"));
        let baseline: Diarization = if cache.is_file() {
            serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap()
        } else {
            let diarizer = OfflineSpeakerDiarization::create(&config).unwrap();
            let result = diarizer.process(&samples).unwrap();
            let result = Diarization {
                turns: result
                    .sort_by_start_time()
                    .iter()
                    .map(|t| SpeakerTurn {
                        start_ms: (t.start * 1000.0) as u64,
                        end_ms: (t.end * 1000.0) as u64,
                        speaker: format!("s{}", t.speaker),
                    })
                    .collect(),
                ..Default::default()
            };
            std::fs::write(cache, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
            result
        };
        let embeddings = std::env::var_os("NOTETAKER_EVAL_EMBEDDINGS").is_some();
        let mut result = baseline.clone();
        // Cached turn labels let threshold sweeps and cleanup tuning avoid a
        // second expensive segmentation pass over the identical recording.
        result.embeddings.clear();
        if legacy && !fixed {
            let extractor = sherpa_onnx::SpeakerEmbeddingExtractor::create(
                &diarization::extractor_config(2).unwrap(),
            )
            .unwrap();
            let centroids = counts(&baseline)
                .keys()
                .filter_map(|id| {
                    let ranges = diarization::clean_ranges(&baseline.turns, id);
                    diarization::cluster_embedding(&extractor, &samples, &ranges)
                        .map(|e| (id.clone(), e))
                })
                .collect();
            legacy_cleanup::reassign(&mut result.turns, &centroids);
        } else if std::env::var_os("NOTETAKER_EVAL_DUMP").is_some() && !fixed {
            assert!(
                !embeddings,
                "dump mode evaluates worker-local comparison vectors"
            );
            let extractor = sherpa_onnx::SpeakerEmbeddingExtractor::create(
                &diarization::extractor_config(2).unwrap(),
            )
            .unwrap();
            let (evidence, fallback) =
                diarization::comparison_evidence(&baseline.turns, &samples, &extractor);
            let turns: Vec<_> = baseline.turns.iter().zip(&evidence).map(|(turn, e)| {
                serde_json::json!({"turn": turn, "clean_ms": e.clean_ms, "embedding": e.embedding})
            }).collect();
            std::fs::write(
                fixtures.join(format!("{name}-comparison.json")),
                serde_json::to_vec(&serde_json::json!({"turns": turns, "fallback": fallback}))
                    .unwrap(),
            )
            .unwrap();
            diarization::clustering::reassign(&mut result.turns, &evidence, &fallback);
        } else if std::env::var_os("NOTETAKER_EVAL_BASELINE_ONLY").is_none() {
            diarization::refine(&mut result, &samples, target, embeddings, 2).unwrap();
        }
        std::fs::write(
            fixtures.join(format!("{name}-{mode}-current.json")),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        let before = counts(&baseline);
        let after = counts(&result);
        let truth = case["truth"].as_array().unwrap();
        let speakers = case["speakers"].as_u64().unwrap() as usize;
        let before_accuracy = mapped_accuracy(&baseline, truth, speakers);
        let after_accuracy = mapped_accuracy(&result, truth, speakers);
        println!(
            "{name}: expected={}, baseline({threshold})={} {before:?}, current={} {after:?}, purity={:.3}/{:.3}",
            case["speakers"],
            before.len(),
            after.len(),
            purity(&baseline, truth),
            purity(&result, truth)
        );
        println!("one-to-one attribution: {before_accuracy:.3}/{after_accuracy:.3}");
        report.push(serde_json::json!({"name": name, "expected": case["speakers"], "baseline": before, "current": after, "baseline_purity": purity(&baseline, truth), "current_purity": purity(&result, truth), "baseline_accuracy": before_accuracy, "current_accuracy": after_accuracy}));
        assert!(!after.is_empty());
        if fixed {
            // Sherpa may return fewer active labels after segmentation, even
            // when num_clusters is exact. Measure that limitation, do not invent turns.
            assert!(after.len() <= speakers, "fixed count for {name}");
        } else if !legacy && std::env::var_os("NOTETAKER_EVAL_BASELINE_ONLY").is_none() {
            assert!(
                after.len().abs_diff(speakers) <= 1,
                "speaker count for {name}: {}",
                after.len()
            );
            assert!(
                after_accuracy >= before_accuracy - 0.02,
                "attribution for {name}"
            );
            assert!(
                purity(&result, truth) + 1e-6 >= purity(&baseline, truth),
                "false merges for {name}"
            );
        }
        if !embeddings {
            assert!(result.embeddings.is_empty());
        }
    }
    std::fs::write(
        fixtures.join(format!(
            "report-round2-{}.json",
            if fixed {
                "fixed".to_string()
            } else if legacy {
                "legacy".to_string()
            } else {
                threshold.to_string()
            }
        )),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
