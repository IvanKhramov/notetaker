//! cargo build -p notetaker-asr-worker -p notetaker-cli -j 2
//! cargo test -p notetaker-pipeline --test real_models -- --ignored --nocapture
use std::{path::PathBuf, process::Command};

use directories::ProjectDirs;
use notetaker_core::{Library, Source};
use notetaker_pipeline::AsrConfig;

#[test]
#[ignore = "requires downloaded GigaAM v3, Parakeet v3, Silero VAD and built CLI/worker"]
fn record_from_files_with_real_models() {
    let root = std::env::var_os("NOTETAKER_MODELS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            ProjectDirs::from("", "", "notetaker")
                .unwrap()
                .data_local_dir()
                .join("models")
        });
    let worker = AsrConfig::new("gigaam-v3").unwrap().binary;
    let cli = std::env::var_os("NOTETAKER_CLI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(format!("notetaker{}", std::env::consts::EXE_SUFFIX))
        });
    for (model, wav) in [("gigaam-v3", "example.wav"), ("parakeet-v3", "en.wav")] {
        let wav = root
            .join(model)
            .join("test_wavs")
            .join(wav)
            .canonicalize()
            .expect("download models using notetaker-asr download, or set NOTETAKER_MODELS_DIR");
        let dir = tempfile::tempdir().unwrap();
        let output = Command::new(&cli)
            .args(["record", "--library"])
            .arg(dir.path())
            .args(["--title", "Real pipeline", "--model", model, "--from-files"])
            .arg(&wav)
            .arg(&wav)
            .env("NOTETAKER_ASR_BIN", &worker)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let library = Library::open(dir.path()).unwrap();
        let meetings = library.list_meetings().unwrap();
        assert_eq!(meetings.len(), 1);
        let meeting = &meetings[0];
        let transcript = meeting.read_transcript().unwrap();
        assert!(!transcript.segments.is_empty());
        assert!(
            transcript
                .segments
                .iter()
                .all(|s| s.source == Source::Final)
        );
        assert!(
            transcript
                .segments
                .windows(2)
                .all(|s| s[0].start_ms <= s[1].start_ms)
        );
        // Both tracks carry the same file, so every mic segment is an echo of
        // the system track and must be suppressed. System speakers may be
        // diarized into s1, s2, ... when the speaker models are installed.
        assert!(transcript.segments.iter().all(|s| s.speaker.0 != "me"));
        assert!(meeting.read_meta().unwrap().ended_at.is_some());
        assert!(!meeting.path().join("audio/.work").exists());
        for segment in transcript.segments.iter().take(3) {
            println!(
                "{model} [{}-{}] {}: {}",
                segment.start_ms, segment.end_ms, segment.speaker.0, segment.text
            );
        }
    }
}

#[test]
#[ignore = "requires both ASR models, Whisper tiny, Silero VAD and built worker"]
fn auto_import_persists_languages_and_vocabulary_corrections() {
    use notetaker_pipeline::{DiarizationConfig, RecordingConfig, RecordingSession};
    let root =
        PathBuf::from(std::env::var_os("NOTETAKER_MODELS_DIR").expect("set NOTETAKER_MODELS_DIR"));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ru-en.wav");
    // Both fixture rates divide 48 kHz exactly. Repetition avoids linking the
    // native ASR runtime into this frontend crate; import resamples to 16 kHz.
    let mut output = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for (model, wav) in [("gigaam-v3", "example.wav"), ("parakeet-v3", "en.wav")] {
        let mut input =
            hound::WavReader::open(root.join(model).join("test_wavs").join(wav)).unwrap();
        assert_eq!(48000 % input.spec().sample_rate, 0);
        let repeat = 48000 / input.spec().sample_rate;
        for sample in input.samples::<i16>() {
            let sample = sample.unwrap();
            for _ in 0..repeat {
                output.write_sample(sample).unwrap();
            }
        }
        for _ in 0..48000 {
            output.write_sample(0i16).unwrap();
        }
    }
    output.finalize().unwrap();
    let library = Library::open(directory.path().join("library")).unwrap();
    let mut config = AsrConfig::new("auto").unwrap();
    config.num_threads = 2;
    config.vocabulary =
        notetaker_core::Vocabulary::parse("Lukomorye = лукоморья\nCountry = country");
    let session = RecordingSession::import(
        library,
        &path,
        notetaker_core::Mode::Local,
        config,
        RecordingConfig {
            diarization: DiarizationConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let meeting = session.wait().unwrap();
    let transcript = meeting.read_transcript().unwrap();
    assert!(
        transcript
            .segments
            .iter()
            .any(|s| s.lang.as_deref() == Some("ru") && s.text.contains("Lukomorye"))
    );
    assert!(
        transcript
            .segments
            .iter()
            .any(|s| s.lang.as_deref() == Some("en") && s.text.contains("Country"))
    );
    assert!(
        transcript
            .segments
            .iter()
            .all(|s| s.source == Source::Final && s.end_ms > s.start_ms)
    );
    assert!(
        transcript
            .segments
            .windows(2)
            .all(|s| s[0].start_ms <= s[1].start_ms)
    );
    // Editing and saving in a frontend must retain optional ASR metadata.
    meeting
        .update_transcript(|t| t.segments[0].edited = true)
        .unwrap();
    assert_eq!(
        meeting.read_transcript().unwrap().segments[0].lang,
        transcript.segments[0].lang
    );
    println!("Auto import: {:?}", transcript.segments);
}
