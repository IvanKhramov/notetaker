//! First-run readiness must not load weights or access the network.
use std::{fs, path::Path, process::Command};

fn check(root: &Path, model: &str) -> bool {
    Command::new(env!("CARGO_BIN_EXE_notetaker-asr"))
        .args(["check-model", model])
        .env("NOTETAKER_MODELS_DIR", root)
        .output()
        .unwrap()
        .status
        .success()
}
fn legacy_install(root: &Path, model: &str, files: &[(&str, u64)]) {
    let path = root.join(model);
    fs::create_dir(&path).unwrap();
    for (name, size) in files {
        fs::File::create(path.join(name))
            .unwrap()
            .set_len(*size)
            .unwrap();
    }
    assert!(!path.join("FILES").exists());
    assert!(!path.join("SHA256").exists());
}

#[test]
fn model_check_requires_complete_asr_and_vad_installation() {
    let root = tempfile::tempdir().unwrap();
    assert!(!check(root.path(), "gigaam-v3"));
    legacy_install(
        root.path(),
        "gigaam-v3",
        &[
            ("encoder.int8.onnx", 224_570_820),
            ("decoder.onnx", 4_600_132),
            ("joiner.onnx", 2_712_896),
            ("tokens.txt", 13_354),
        ],
    );
    assert!(!check(root.path(), "gigaam-v3"));
    legacy_install(root.path(), "silero-vad", &[("silero_vad.onnx", 643_854)]);
    // Sparse synthetic weights prove check-model only reads sizes, not ONNX data.
    assert!(check(root.path(), "gigaam-v3"));
    fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("silero-vad/silero_vad.onnx"))
        .unwrap()
        .set_len(1)
        .unwrap();
    assert!(!check(root.path(), "gigaam-v3"));
    assert!(!check(root.path(), "parakeet-v3"));
    assert!(!check(root.path(), "../gigaam-v3"));
}

#[test]
fn diarization_check_needs_both_models_and_no_asr_or_vad() {
    let root = tempfile::tempdir().unwrap();
    assert!(!check(root.path(), "diarization"));
    legacy_install(
        root.path(),
        "pyannote-3.0",
        &[("model.int8.onnx", 1_540_506), ("LICENSE", 1_061)],
    );
    assert!(!check(root.path(), "diarization"));
    legacy_install(
        root.path(),
        "eres2net-base",
        &[(
            "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
            39_593_761,
        )],
    );
    assert!(check(root.path(), "diarization"));
    fs::write(root.path().join("pyannote-3.0/model.int8.onnx"), []).unwrap();
    assert!(!check(root.path(), "diarization"));
}
