use anyhow::{Context, Result, bail, ensure};
use bzip2::read::BzDecoder;
use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct ModelManifest {
    pub id: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub directory: &'static str,
    pub files: &'static [&'static str],
}

pub const GIGAAM: ModelManifest = ModelManifest {
    id: "gigaam-v3",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16.tar.bz2",
    sha256: "f9620a0099019c6afcee26525ef9ed3297fa50dd5691c1902af0c948fc1a470b",
    size: 170_197_019,
    directory: "sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16",
    files: &[
        "encoder.int8.onnx",
        "decoder.onnx",
        "joiner.onnx",
        "tokens.txt",
    ],
};
pub const PARAKEET: ModelManifest = ModelManifest {
    id: "parakeet-v3",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
    sha256: "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf",
    size: 487_170_055,
    directory: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
    files: &[
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "joiner.int8.onnx",
        "tokens.txt",
    ],
};
pub const SILERO: ModelManifest = ModelManifest {
    id: "silero-vad",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
    sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    size: 643_854,
    directory: "",
    files: &["silero_vad.onnx"],
};

/// Multilingual tiny, not tiny.en. Only int8 weights are retained after download.
pub const WHISPER_TINY: ModelManifest = ModelManifest {
    id: "whisper-tiny",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-tiny.tar.bz2",
    sha256: "c46116994e539aa165266d96b325252728429c12535eb9d8b6a2b10f129e66b1",
    size: 116_204_861,
    directory: "sherpa-onnx-whisper-tiny",
    files: &["tiny-encoder.int8.onnx", "tiny-decoder.int8.onnx"],
};

pub const PYANNOTE: ModelManifest = ModelManifest {
    id: "pyannote-3.0",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
    sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
    size: 6_958_444,
    directory: "sherpa-onnx-pyannote-segmentation-3-0",
    files: &["model.int8.onnx", "LICENSE"],
};
pub const ERES2NET: ModelManifest = ModelManifest {
    id: "eres2net-base",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
    sha256: "1a331345f04805badbb495c775a6ddffcdd1a732567d5ec8b3d5749e3c7a5e4b",
    size: 39_593_761,
    directory: "",
    files: &["3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"],
};

pub fn check(id: &str) -> Result<()> {
    let root = models_dir()?;
    if id == "auto" {
        check(GIGAAM.id)?;
        check(PARAKEET.id)?;
        installed(&root, &WHISPER_TINY)?;
    } else if id == "diarization" {
        installed(&root, &PYANNOTE)?;
        installed(&root, &ERES2NET)?;
    } else {
        installed(&root, manifest(id)?)?;
        if id == GIGAAM.id || id == PARAKEET.id {
            installed(&root, &SILERO)?;
        }
    }
    Ok(())
}

pub fn manifest(id: &str) -> Result<&'static ModelManifest> {
    match id {
        "gigaam-v3" => Ok(&GIGAAM),
        "whisper-tiny" => Ok(&WHISPER_TINY),
        "parakeet-v3" => Ok(&PARAKEET),
        "silero-vad" => Ok(&SILERO),
        "pyannote-3.0" => Ok(&PYANNOTE),
        "eres2net-base" => Ok(&ERES2NET),
        _ => bail!(
            "unknown model {id:?}; choose gigaam-v3, parakeet-v3, whisper-tiny, silero-vad, pyannote-3.0 or eres2net-base"
        ),
    }
}

pub fn models_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NOTETAKER_MODELS_DIR") {
        ensure!(!path.is_empty(), "NOTETAKER_MODELS_DIR is empty");
        return Ok(path.into());
    }
    Ok(ProjectDirs::from("", "", "notetaker")
        .context("cannot locate data directory; set NOTETAKER_MODELS_DIR")?
        .data_local_dir()
        .join("models"))
}

pub fn installed(root: &Path, model: &ModelManifest) -> Result<PathBuf> {
    let path = root.join(model.id);
    for file in model.files {
        ensure!(
            path.join(file).is_file(),
            "missing {}; run notetaker-asr download {}",
            path.join(file).display(),
            model.id
        );
    }
    Ok(path)
}

fn verify(mut reader: impl Read, model: &ModelManifest) -> Result<()> {
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        count += n as u64;
    }
    ensure!(
        count == model.size,
        "{} size mismatch: expected {}, got {count}",
        model.id,
        model.size
    );
    ensure!(
        format!("{:x}", hash.finalize()) == model.sha256,
        "{} SHA-256 mismatch",
        model.id
    );
    Ok(())
}

fn unpack_archive(archive: impl Read, extracted: &Path, model: &ModelManifest) -> Result<()> {
    let mut tar = tar::Archive::new(BzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        ensure!(
            entry.header().entry_type().is_file() || entry.header().entry_type().is_dir(),
            "unsupported archive entry"
        );
        ensure!(
            entry.path()?.starts_with(model.directory),
            "unexpected archive root"
        );
        // The tiny archive also contains float weights. Keep just the
        // files used for language ID, without deleting installed artifacts.
        if model.id == WHISPER_TINY.id {
            let path = entry.path()?;
            if !model
                .files
                .iter()
                .any(|name| path == Path::new(model.directory).join(name))
            {
                continue;
            }
        }
        ensure!(entry.unpack_in(extracted)?, "unsafe archive path");
    }
    Ok(())
}

fn download_one(root: &Path, model: &ModelManifest) -> Result<()> {
    if installed(root, model).is_ok() {
        return Ok(());
    }
    ensure!(
        !root.join(model.id).exists(),
        "incomplete model directory {}; move it aside before retrying",
        root.join(model.id).display()
    );
    let staging = tempfile::tempdir_in(root)?;
    let archive = staging.path().join("download");
    eprintln!("Downloading {} ({} bytes)", model.id, model.size);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()?;
    let mut response = client.get(model.url).send()?.error_for_status()?;
    let mut file = File::create(&archive)?;
    // Bound the download even if a server returns an unexpectedly large body.
    std::io::copy(&mut response.by_ref().take(model.size + 1), &mut file)?;
    file.flush()?;
    drop(file);
    verify(File::open(&archive)?, model)?;
    let extracted = staging.path().join("extracted");
    fs::create_dir(&extracted)?;
    if model.directory.is_empty() {
        fs::rename(&archive, extracted.join(model.files[0]))?;
    } else {
        unpack_archive(File::open(&archive)?, &extracted, model)?;
    }
    let source = extracted.join(model.directory);
    for name in model.files {
        ensure!(source.join(name).is_file(), "archive missing {name}");
    }
    fs::write(source.join("SHA256"), model.sha256)?;
    fs::rename(source, root.join(model.id))?;
    eprintln!(
        "Installed {} in {}",
        model.id,
        root.join(model.id).display()
    );
    Ok(())
}

pub fn download(id: &str) -> Result<()> {
    let root = models_dir()?;
    fs::create_dir_all(&root)?;
    if id == "auto" {
        download_one(&root, &GIGAAM)?;
        download_one(&root, &PARAKEET)?;
        download_one(&root, &SILERO)?;
        return download_one(&root, &WHISPER_TINY);
    }
    if id == "diarization" {
        download_one(&root, &PYANNOTE)?;
        return download_one(&root, &ERES2NET);
    }
    let model = manifest(id)?;
    download_one(&root, model)?;
    if id == GIGAAM.id || id == PARAKEET.id {
        download_one(&root, &SILERO)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiny_install_keeps_only_int8_weights_and_creates_parent_directory() {
        let encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        let mut archive = tar::Builder::new(encoder);
        for name in [
            "tiny-encoder.int8.onnx",
            "tiny-decoder.int8.onnx",
            "tiny-encoder.onnx",
            "tiny-decoder.onnx",
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(3);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(
                    &mut header,
                    format!("{}/{name}", WHISPER_TINY.directory),
                    &b"abc"[..],
                )
                .unwrap();
        }
        let compressed = archive.into_inner().unwrap().finish().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        unpack_archive(compressed.as_slice(), temporary.path(), &WHISPER_TINY).unwrap();
        let installed = temporary.path().join(WHISPER_TINY.directory);
        for name in WHISPER_TINY.files {
            assert_eq!(fs::read(installed.join(name)).unwrap(), b"abc");
        }
        assert!(!installed.join("tiny-encoder.onnx").exists());
        assert!(!installed.join("tiny-decoder.onnx").exists());
    }

    #[test]
    fn checksum_rejects_corruption() {
        let model = ModelManifest {
            id: "fixture",
            url: "",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
            directory: "",
            files: &[],
        };
        assert!(verify(&b"abc"[..], &model).is_ok());
        assert!(verify(&b"abd"[..], &model).is_err());
        assert!(verify(&b"ab"[..], &model).is_err());
        assert!(manifest("../gigaam-v3").is_err());
    }
}
