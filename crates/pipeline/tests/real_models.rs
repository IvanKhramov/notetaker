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
