//! Run after downloading both models with NOTETAKER_MODELS_DIR set.
use notetaker_asr_proto::{AsrClient, AudioInput, Provider, Request, Response, Segment};
use std::{path::PathBuf, sync::Mutex};

static MODEL_TEST: Mutex<()> = Mutex::new(());

fn recognize(model: &str, wav: &str) -> Vec<Segment> {
    let root =
        PathBuf::from(std::env::var_os("NOTETAKER_MODELS_DIR").expect("set NOTETAKER_MODELS_DIR"));
    let path = root
        .join(model)
        .join("test_wavs")
        .join(wav)
        .canonicalize()
        .unwrap();
    let mut client = AsrClient::spawn(env!("CARGO_BIN_EXE_notetaker-asr")).unwrap();
    assert_eq!(
        client.load_model(model, Provider::Auto).unwrap(),
        Provider::Cpu
    );
    let original = client
        .transcribe("wav", AudioInput::WavPath(path.clone()), "mic", 0)
        .unwrap();
    assert!(!original.is_empty(), "expected real speech");
    for s in &original {
        assert!(s.end_ms > s.start_ms);
        assert!(!s.text.trim().is_empty());
    }
    let mut wav = hound::WavReader::open(path).unwrap();
    let sample_rate = wav.spec().sample_rate;
    let samples: Vec<f32> = wav
        .samples::<i16>()
        .map(|v| f32::from(v.unwrap()) / 32768.0)
        .collect();
    let samples = if sample_rate == 16000 {
        samples
    } else {
        sherpa_onnx::LinearResampler::create(sample_rate as i32, 16000)
            .unwrap()
            .resample(&samples, true)
    };
    let shifted = client
        .transcribe("pcm", AudioInput::pcm(&samples), "system", 12_345)
        .unwrap();
    assert_eq!(original.len(), shifted.len());
    for (a, b) in original.iter().zip(&shifted) {
        assert_eq!(a.text, b.text);
        assert_eq!(b.start_ms, a.start_ms + 12_345);
        assert_eq!(b.end_ms, a.end_ms + 12_345);
    }
    client
        .send(&Request::Transcribe {
            id: "silence".into(),
            input: AudioInput::pcm(&vec![0.0; 16000]),
            channel: "mic".into(),
            offset_ms: 0,
        })
        .unwrap();
    loop {
        match client.receive().unwrap() {
            Response::Progress { id, fraction } => {
                assert_eq!(id, "silence");
                assert!((0.0..=1.0).contains(&fraction));
            }
            Response::Segments { id, segments } => {
                assert_eq!(id, "silence");
                assert!(segments.is_empty());
                break;
            }
            response => panic!("unexpected silence response: {response:?}"),
        }
    }
    assert!(
        client
            .transcribe(
                "bad",
                AudioInput::Pcm {
                    samples_f32_16k_mono: "bad base64".into()
                },
                "mic",
                0
            )
            .is_err()
    );
    // A request error must not poison subsequent VAD state or kill the worker.
    assert!(
        client
            .transcribe("empty", AudioInput::pcm(&[]), "mic", 0)
            .unwrap()
            .is_empty()
    );
    client.shutdown().unwrap();
    original
}

#[test]
#[ignore = "requires downloaded GigaAM v3 + Silero VAD models"]
fn gigaam_russian_wav_and_pcm() {
    let _guard = MODEL_TEST.lock().unwrap();
    let result = recognize("gigaam-v3", "example.wav");
    let text = result
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    println!("GigaAM: {text}");
    assert!(text.to_lowercase().contains("лукоморья"));
}

#[test]
#[ignore = "requires downloaded Parakeet v3 + Silero VAD models"]
fn parakeet_english_wav_and_pcm() {
    let _guard = MODEL_TEST.lock().unwrap();
    let result = recognize("parakeet-v3", "en.wav");
    let text = result
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    println!("Parakeet: {text}");
    assert!(text.to_lowercase().contains("your country"));
}

#[test]
#[ignore = "requires downloaded GigaAM v3, Parakeet v3 and Silero VAD models"]
fn nemo_models_accept_hotwords_and_preserve_offsets() {
    let _guard = MODEL_TEST.lock().unwrap();
    let root =
        PathBuf::from(std::env::var_os("NOTETAKER_MODELS_DIR").expect("set NOTETAKER_MODELS_DIR"));
    for (model, wav, term) in [
        ("gigaam-v3", "example.wav", "Лукоморья"),
        ("parakeet-v3", "en.wav", "country"),
    ] {
        let mut client = AsrClient::spawn(env!("CARGO_BIN_EXE_notetaker-asr")).unwrap();
        client
            .load_model_with_hotwords(model, Provider::Cpu, &[term.into()])
            .unwrap();
        let path = root
            .join(model)
            .join("test_wavs")
            .join(wav)
            .canonicalize()
            .unwrap();
        let segments = client
            .transcribe("hotwords", AudioInput::WavPath(path), "system", 12345)
            .unwrap();
        assert!(!segments.is_empty());
        assert!(
            segments
                .iter()
                .all(|s| s.start_ms >= 12345 && s.end_ms > s.start_ms)
        );
        client.shutdown().unwrap();
    }
}

#[test]
#[ignore = "requires GigaAM, Parakeet, Whisper tiny and Silero VAD"]
fn auto_routes_russian_and_english_chunks_with_hotwords_and_offsets() {
    let _guard = MODEL_TEST.lock().unwrap();
    let root =
        PathBuf::from(std::env::var_os("NOTETAKER_MODELS_DIR").expect("set NOTETAKER_MODELS_DIR"));
    let mut samples = Vec::new();
    let mut boundary = 0;
    for (model, file) in [("gigaam-v3", "example.wav"), ("parakeet-v3", "en.wav")] {
        let mut wav =
            hound::WavReader::open(root.join(model).join("test_wavs").join(file)).unwrap();
        let rate = wav.spec().sample_rate;
        let pcm: Vec<f32> = wav
            .samples::<i16>()
            .map(|v| f32::from(v.unwrap()) / 32768.0)
            .collect();
        let pcm = if rate == 16000 {
            pcm
        } else {
            sherpa_onnx::LinearResampler::create(rate as i32, 16000)
                .unwrap()
                .resample(&pcm, true)
        };
        if !samples.is_empty() {
            boundary = samples.len() as u64 / 16;
        }
        samples.extend(pcm);
        samples.extend(vec![0.0; 16000]);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ru-en.wav");
    let mut wav = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for &sample in &samples {
        wav.write_sample(sample).unwrap();
    }
    wav.finalize().unwrap();
    let mut client = AsrClient::spawn(env!("CARGO_BIN_EXE_notetaker-asr")).unwrap();
    client
        .load_model_with_options(
            "auto",
            Provider::Cpu,
            &["Лукоморья".into(), "country".into()],
            2,
        )
        .unwrap();
    let original = client
        .transcribe("mixed", AudioInput::WavPath(path), "system", 0)
        .unwrap();
    println!("Auto: {original:?}");
    assert!(original.iter().any(|s| s.lang.as_deref() == Some("ru") && s.text.to_lowercase().contains("лукоморья")));
    assert!(
        original
            .iter()
            .any(|s| s.lang.as_deref() == Some("en")
                && s.text.to_lowercase().contains("your country"))
    );
    assert!(
        original
            .iter()
            .all(|s| s.end_ms > s.start_ms && s.end_ms <= samples.len() as u64 / 16)
    );
    assert!(
        original
            .iter()
            .filter(|s| s.start_ms < boundary)
            .all(|s| s.lang.as_deref() == Some("ru"))
    );
    assert!(
        original
            .iter()
            .filter(|s| s.start_ms >= boundary)
            .all(|s| s.lang.as_deref() == Some("en"))
    );
    let shifted = client
        .transcribe("shifted", AudioInput::pcm(&samples), "mic", 12345)
        .unwrap();
    assert_eq!(original.len(), shifted.len());
    for (a, b) in original.iter().zip(shifted) {
        assert_eq!(a.text, b.text);
        assert_eq!(a.lang, b.lang);
        assert_eq!(a.start_ms + 12345, b.start_ms);
        assert_eq!(a.end_ms + 12345, b.end_ms);
    }
    client.shutdown().unwrap();
}
