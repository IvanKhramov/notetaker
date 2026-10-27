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

#[test]
fn model_check_requires_complete_asr_and_vad_installation() {
    let root = tempfile::tempdir().unwrap();
    assert!(!check(root.path(), "gigaam-v3"));
    let asr = root.path().join("gigaam-v3");
    fs::create_dir(&asr).unwrap();
    for file in [
        "encoder.int8.onnx",
        "decoder.onnx",
        "joiner.onnx",
        "tokens.txt",
    ] {
        fs::write(asr.join(file), []).unwrap();
    }
    assert!(!check(root.path(), "gigaam-v3"));
    let vad = root.path().join("silero-vad");
    fs::create_dir(&vad).unwrap();
    fs::write(vad.join("silero_vad.onnx"), []).unwrap();
    // Empty fixtures prove that check-model does not initialize a recognizer.
    assert!(check(root.path(), "gigaam-v3"));
    assert!(!check(root.path(), "parakeet-v3"));
    assert!(!check(root.path(), "../gigaam-v3"));
}

#[test]
fn diarization_check_needs_both_models_and_no_asr_or_vad() {
    let root = tempfile::tempdir().unwrap();
    assert!(!check(root.path(), "diarization"));
    let segmentation = root.path().join("pyannote-3.0");
    fs::create_dir(&segmentation).unwrap();
    for file in ["model.int8.onnx", "LICENSE"] {
        fs::write(segmentation.join(file), []).unwrap();
    }
    assert!(!check(root.path(), "diarization"));
    let embedding = root.path().join("eres2net-base");
    fs::create_dir(&embedding).unwrap();
    fs::write(
        embedding.join("3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"),
        [],
    )
    .unwrap();
    assert!(check(root.path(), "diarization"));
}
