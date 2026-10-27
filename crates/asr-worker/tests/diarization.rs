//! Real CPU smoke test, using two different speakers from sherpa ASR fixtures.
use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use notetaker_asr_proto::{AsrClient, TimeRange};

#[test]
#[ignore = "requires downloaded diarization, GigaAM and Parakeet test WAVs"]
fn two_speakers_with_automatic_clustering_and_embeddings() {
    let root =
        PathBuf::from(std::env::var_os("NOTETAKER_MODELS_DIR").expect("set NOTETAKER_MODELS_DIR"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two-speakers.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for (model, file) in [("gigaam-v3", "example.wav"), ("parakeet-v3", "en.wav")] {
        let mut wav =
            hound::WavReader::open(root.join(model).join("test_wavs").join(file)).unwrap();
        let rate = wav.spec().sample_rate;
        let samples: Vec<f32> = wav
            .samples::<i16>()
            .map(|s| f32::from(s.unwrap()) / 32768.0)
            .collect();
        let samples = if rate == 16000 {
            samples
        } else {
            sherpa_onnx::LinearResampler::create(rate as i32, 16000)
                .unwrap()
                .resample(&samples, true)
        };
        for sample in samples {
            writer.write_sample(sample).unwrap();
        }
        for _ in 0..16000 {
            writer.write_sample(0.0f32).unwrap();
        }
    }
    writer.finalize().unwrap();
    let mut client = AsrClient::spawn(env!("CARGO_BIN_EXE_notetaker-asr")).unwrap();
    client.set_timeout(Duration::from_secs(300));
    let result = client.diarize(&path, "system", None, true).unwrap();
    let ids: BTreeSet<_> = result.turns.iter().map(|t| t.speaker.as_str()).collect();
    println!(
        "automatic clusters: {ids:?}; turns: {:?}; embedding dimensions: {:?}",
        result.turns,
        result.embeddings.values().map(Vec::len).collect::<Vec<_>>()
    );
    assert_eq!(
        ids.len(),
        2,
        "two fixture speakers should form two automatic clusters"
    );
    assert_eq!(result.embeddings.len(), 2);
    assert!(result.turns.iter().all(|t| t.end_ms > t.start_ms));
    let embedded = client
        .embed(
            &path,
            vec![TimeRange {
                start_ms: 0,
                end_ms: 8000,
            }],
        )
        .unwrap();
    assert_eq!(
        embedded.len(),
        result.embeddings.values().next().unwrap().len()
    );
    assert!(embedded.iter().all(|v| v.is_finite()));
    // Bad requests leave the worker alive for a following valid request.
    assert!(client.diarize(&path, "system", Some(0), false).is_err());
    let hinted = client.diarize(&path, "system", Some(2), false).unwrap();
    assert_eq!(
        hinted
            .turns
            .iter()
            .map(|t| &t.speaker)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert!(hinted.embeddings.is_empty());
    client.shutdown().unwrap();
}
