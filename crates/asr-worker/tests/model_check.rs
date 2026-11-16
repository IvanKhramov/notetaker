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
        fs::write(asr.join(file), [1]).unwrap();
    }
    fs::write(
        asr.join("FILES"),
        "encoder.int8.onnx\t1\ndecoder.onnx\t1\njoiner.onnx\t1\ntokens.txt\t1\n",
    )
    .unwrap();
    fs::write(
        asr.join("SHA256"),
        "f9620a0099019c6afcee26525ef9ed3297fa50dd5691c1902af0c948fc1a470b",
    )
    .unwrap();
    assert!(!check(root.path(), "gigaam-v3"));
    let vad = root.path().join("silero-vad");
    fs::create_dir(&vad).unwrap();
    let file = fs::File::create(vad.join("silero_vad.onnx")).unwrap();
    file.set_len(643_854).unwrap();
    // Synthetic weights prove that check-model only reads integrity metadata.
    assert!(check(root.path(), "gigaam-v3"));
    file.set_len(1).unwrap();
    assert!(!check(root.path(), "gigaam-v3"));
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
        fs::write(segmentation.join(file), [1]).unwrap();
    }
    fs::write(
        segmentation.join("FILES"),
        "model.int8.onnx\t1\nLICENSE\t1\n",
    )
    .unwrap();
    fs::write(
        segmentation.join("SHA256"),
        "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
    )
    .unwrap();
    assert!(!check(root.path(), "diarization"));
    let embedding = root.path().join("eres2net-base");
    fs::create_dir(&embedding).unwrap();
    let file = fs::File::create(
        embedding.join("3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"),
    )
    .unwrap();
    file.set_len(39_593_761).unwrap();
    assert!(check(root.path(), "diarization"));
    fs::write(segmentation.join("model.int8.onnx"), []).unwrap();
    assert!(!check(root.path(), "diarization"));
}
