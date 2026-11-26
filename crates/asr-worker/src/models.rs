#[path = "../../model_download.rs"]
mod model_download;
use anyhow::{Context, Result, bail, ensure};
use bzip2::read::BzDecoder;
use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct ModelManifest {
    pub id: &'static str,
    pub source: Source,
    pub directory: &'static str,
    /// Required installed files and their pinned, unpacked byte lengths.
    pub files: &'static [(&'static str, u64)],
}
pub enum Source {
    /// One file or a `.tar.bz2` archive, verified as a whole.
    Archive {
        url: &'static str,
        sha256: &'static str,
        size: u64,
    },
    /// Each of `files` from `{root}/{name}`, verified by the SHA-256 at the same index.
    Files {
        root: &'static str,
        sha256: &'static [&'static str],
    },
}

pub const GIGAAM: ModelManifest = ModelManifest {
    id: "gigaam-v3",
    source: Source::Archive {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16.tar.bz2",
        sha256: "f9620a0099019c6afcee26525ef9ed3297fa50dd5691c1902af0c948fc1a470b",
        size: 170_197_019,
    },
    directory: "sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16",
    files: &[
        ("encoder.int8.onnx", 224_570_820),
        ("decoder.onnx", 4_600_132),
        ("joiner.onnx", 2_712_896),
        ("tokens.txt", 13_354),
    ],
};
// The GitHub release asset was re-exported on 2026-11-07 with different weights.
// Pin the files of the original export by Hugging Face commit: installed copies stay valid.
pub const PARAKEET: ModelManifest = ModelManifest {
    id: "parakeet-v3",
    source: Source::Files {
        root: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78",
        sha256: &[
            "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
            "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
            "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
            "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        ],
    },
    directory: "",
    files: &[
        ("encoder.int8.onnx", 652_184_281),
        ("decoder.int8.onnx", 11_845_275),
        ("joiner.int8.onnx", 6_355_277),
        ("tokens.txt", 93_939),
    ],
};
pub const SILERO: ModelManifest = ModelManifest {
    id: "silero-vad",
    source: Source::Archive {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
        sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
        size: 643_854,
    },
    directory: "",
    files: &[("silero_vad.onnx", 643_854)],
};

/// Multilingual tiny, not tiny.en. Only int8 weights are retained after download.
pub const WHISPER_TINY: ModelManifest = ModelManifest {
    id: "whisper-tiny",
    source: Source::Archive {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-tiny.tar.bz2",
        sha256: "c46116994e539aa165266d96b325252728429c12535eb9d8b6a2b10f129e66b1",
        size: 116_204_861,
    },
    directory: "sherpa-onnx-whisper-tiny",
    files: &[
        ("tiny-encoder.int8.onnx", 12_937_772),
        ("tiny-decoder.int8.onnx", 89_855_401),
    ],
};

pub const PYANNOTE: ModelManifest = ModelManifest {
    id: "pyannote-3.0",
    source: Source::Archive {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
        sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
        size: 6_958_444,
    },
    directory: "sherpa-onnx-pyannote-segmentation-3-0",
    files: &[("model.int8.onnx", 1_540_506), ("LICENSE", 1_061)],
};
pub const ERES2NET: ModelManifest = ModelManifest {
    id: "eres2net-base",
    source: Source::Archive {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
        sha256: "1a331345f04805badbb495c775a6ddffcdd1a732567d5ec8b3d5749e3c7a5e4b",
        size: 39_593_761,
    },
    directory: "",
    files: &[(
        "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
        39_593_761,
    )],
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
    check_files(&path, model)?;
    Ok(path)
}
fn check_files(path: &Path, model: &ModelManifest) -> Result<()> {
    for (file, expected) in model.files {
        let metadata = fs::metadata(path.join(file))?;
        ensure!(
            metadata.is_file() && *expected > 0 && metadata.len() == *expected,
            "Файл отсутствует или обрезан: {}; повторите notetaker-asr download {}",
            path.join(file).display(),
            model.id
        );
    }
    Ok(())
}

fn verify(mut reader: impl Read, size: u64, sha256: &str, label: &str) -> Result<()> {
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
        count == size,
        "{label} size mismatch: expected {size}, got {count}"
    );
    ensure!(
        format!("{:x}", hash.finalize()) == sha256,
        "{label} SHA-256 mismatch"
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
                .any(|(name, _)| path == Path::new(model.directory).join(name))
            {
                continue;
            }
        }
        ensure!(entry.unpack_in(extracted)?, "unsafe archive path");
    }
    Ok(())
}

/// Download `url` to a resumable partial and keep it only if it matches the pin.
fn fetch(url: &str, partial: &Path, size: u64, sha256: &str, label: &str) -> Result<()> {
    model_download::download(url, partial, size, label, Duration::from_secs(60))?;
    if let Err(error) = verify(File::open(partial)?, size, sha256, label) {
        model_download::discard_corrupt(partial)?;
        return Err(error);
    }
    Ok(())
}

fn download_one(root: &Path, model: &ModelManifest) -> Result<()> {
    let _lock = model_download::lock(root, model.id)?;
    let staging = model_download::staging(root, model.id)?;
    if installed(root, model).is_ok() {
        fs::remove_dir_all(staging)?;
        return Ok(());
    }
    let extracted = staging.join("extracted");
    fs::create_dir(&extracted)?;
    let archive = root.join(format!(".{}.part", model.id));
    let mut partials = vec![archive.clone()];
    match model.source {
        Source::Archive { url, sha256, size } => {
            fetch(url, &archive, size, sha256, model.id)?;
            if model.directory.is_empty() {
                // The installed inode must never have been open as a writable partial.
                fs::copy(&archive, extracted.join(model.files[0].0))?;
            } else {
                unpack_archive(File::open(&archive)?, &extracted, model)?;
            }
        }
        Source::Files { root: base, sha256 } => {
            ensure!(
                sha256.len() == model.files.len(),
                "{}: pin mismatch",
                model.id
            );
            // An archive partial of an earlier source can never be resumed.
            if archive.exists() {
                fs::remove_file(&archive)?;
            }
            for ((name, size), sha256) in model.files.iter().zip(sha256) {
                let label = format!("{}/{name}", model.id);
                let partial = root.join(format!(".{}-{name}.part", model.id));
                fetch(&format!("{base}/{name}"), &partial, *size, sha256, &label)?;
                fs::copy(&partial, extracted.join(name))?;
                eprintln!("NOTETAKER_MODEL_INSTALLED {label}");
                partials.push(partial);
            }
        }
    }
    let source = extracted.join(model.directory);
    check_files(&source, model)?;
    if let Source::Archive { sha256, .. } = model.source {
        fs::write(source.join("SHA256"), sha256)?;
    }
    if root.join(model.id).exists() {
        fs::remove_dir_all(root.join(model.id))?;
    }
    fs::rename(source, root.join(model.id))?;
    for partial in partials {
        if partial.exists() {
            fs::remove_file(partial)?;
        }
    }
    fs::remove_dir_all(staging)?;
    if let Source::Archive { .. } = model.source {
        eprintln!("NOTETAKER_MODEL_INSTALLED {}", model.id);
    }
    eprintln!(
        "Installed {} in {}",
        model.id,
        root.join(model.id).display()
    );
    Ok(())
}

fn downloads(id: &str) -> Result<Vec<&'static ModelManifest>> {
    Ok(match id {
        "auto" => vec![&GIGAAM, &PARAKEET, &SILERO, &WHISPER_TINY],
        "diarization" => vec![&PYANNOTE, &ERES2NET],
        _ => {
            let model = manifest(id)?;
            if id == GIGAAM.id || id == PARAKEET.id {
                vec![model, &SILERO]
            } else {
                vec![model]
            }
        }
    })
}
pub fn download_plan(id: &str) -> Result<()> {
    let root = models_dir()?;
    for model in downloads(id)? {
        let ready = u8::from(installed(&root, model).is_ok());
        match model.source {
            Source::Archive { size, .. } => {
                println!("NOTETAKER_DOWNLOAD_PLAN {} {size} {ready}", model.id);
            }
            // Progress and installation are reported per file under these ids.
            Source::Files { .. } => {
                for (name, size) in model.files {
                    println!("NOTETAKER_DOWNLOAD_PLAN {}/{name} {size} {ready}", model.id);
                }
            }
        }
    }
    Ok(())
}
pub fn download(id: &str) -> Result<()> {
    let root = models_dir()?;
    fs::create_dir_all(&root)?;
    for model in downloads(id)? {
        download_one(&root, model)?;
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
        for (name, _) in WHISPER_TINY.files {
            assert_eq!(fs::read(installed.join(name)).unwrap(), b"abc");
        }
        assert!(!installed.join("tiny-encoder.onnx").exists());
        assert!(!installed.join("tiny-decoder.onnx").exists());
    }

    #[test]
    fn checksum_rejects_corruption() {
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify(&b"abc"[..], 3, sha, "fixture").is_ok());
        assert!(verify(&b"abd"[..], 3, sha, "fixture").is_err());
        assert!(verify(&b"ab"[..], 3, sha, "fixture").is_err());
        assert!(manifest("../gigaam-v3").is_err());
    }
}

#[cfg(test)]
mod installation_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        process::{Child, Command, Stdio},
        thread,
        time::Instant,
    };
    fn model(url: String) -> ModelManifest {
        ModelManifest {
            id: "neutral",
            source: Source::Archive {
                url: Box::leak(url.into_boxed_str()),
                sha256: Box::leak(format!("{:x}", Sha256::digest(b"abcdef")).into_boxed_str()),
                size: 6,
            },
            directory: "",
            files: &[("model.bin", 6)],
        }
    }
    fn headers(stream: &mut std::net::TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
    }
    struct Worker(Child);
    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn wait(child: &mut Child) -> std::process::ExitStatus {
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "downloader did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn concurrent_downloaders_lock_before_opening_the_partial() {
        const CHILD_ROOT: &str = "NOTETAKER_MODEL_LOCK_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            if let Some(ready) = std::env::var_os("NOTETAKER_MODEL_LOCK_TEST_READY") {
                fs::write(ready, b"ready").unwrap();
            }
            download_one(
                &root,
                &model(std::env::var("NOTETAKER_MODEL_LOCK_TEST_URL").unwrap()),
            )
            .unwrap();
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let server_listener = listener.try_clone().unwrap();
        let (started, received) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = server_listener.accept().unwrap();
            headers(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc")
                .unwrap();
            started.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(40)).unwrap();
            stream.write_all(b"def").unwrap();
        });
        let spawn = |ready: Option<&Path>| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "models::installation_tests::concurrent_downloaders_lock_before_opening_the_partial", "--nocapture"])
                .env(CHILD_ROOT,root.path()).env("NOTETAKER_MODEL_LOCK_TEST_URL",&url).stdout(Stdio::null()).stderr(Stdio::null());
            if let Some(ready) = ready {
                command.env("NOTETAKER_MODEL_LOCK_TEST_READY", ready);
            }
            Worker(command.spawn().unwrap())
        };
        let mut first = spawn(None);
        received.recv_timeout(Duration::from_secs(15)).unwrap();
        let ready = root.path().join("second-ready");
        let mut second = spawn(Some(&ready));
        let until = Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(100));
        assert!(
            second.0.try_wait().unwrap().is_none(),
            "second installer bypassed the lock"
        );
        release.send(()).unwrap();
        assert!(wait(&mut first.0).success());
        assert!(wait(&mut second.0).success());
        server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "second installer sent a late HTTP request"
        );
        assert_eq!(
            fs::read(root.path().join("neutral/model.bin")).unwrap(),
            b"abcdef"
        );
        assert!(!root.path().join(".neutral.staging").exists());
    }
    #[test]
    fn stale_writable_descriptor_cannot_truncate_the_installed_inode() {
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join(".neutral.part");
        fs::write(&partial, b"abcdef").unwrap();
        let stale = fs::OpenOptions::new().write(true).open(&partial).unwrap();
        download_one(root.path(), &model("http://127.0.0.1:1/unused".into())).unwrap();
        stale.set_len(0).unwrap();
        assert_eq!(
            fs::read(root.path().join("neutral/model.bin")).unwrap(),
            b"abcdef"
        );
    }
    #[test]
    fn truncated_install_is_downloaded_again_and_stale_staging_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("neutral")).unwrap();
        fs::write(root.path().join("neutral/model.bin"), b"ab").unwrap();
        fs::create_dir(root.path().join(".neutral.staging")).unwrap();
        fs::write(root.path().join(".neutral.staging/old-copy"), b"stale").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let manifest = model(format!("http://{}/model", listener.local_addr().unwrap()));
        assert!(installed(root.path(), &manifest).is_err());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            headers(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef")
                .unwrap();
        });
        download_one(root.path(), &manifest).unwrap();
        server.join().unwrap();
        assert!(installed(root.path(), &manifest).is_ok());
        assert_eq!(
            fs::read(root.path().join("neutral/model.bin")).unwrap(),
            b"abcdef"
        );
        assert!(!root.path().join(".neutral.staging").exists());
    }
    #[test]
    fn pinned_files_install_each_verified_file_and_drop_a_stale_archive_partial() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".pinned.part"), b"old archive").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base: &'static str =
            Box::leak(format!("http://{}/rev", listener.local_addr().unwrap()).into_boxed_str());
        let digest = |body: &[u8]| -> &'static str {
            Box::leak(format!("{:x}", Sha256::digest(body)).into_boxed_str())
        };
        let manifest = ModelManifest {
            id: "pinned",
            source: Source::Files {
                root: base,
                sha256: Box::leak(vec![digest(b"abc"), digest(b"defg")].into_boxed_slice()),
            },
            directory: "",
            files: &[("a.onnx", 3), ("tokens.txt", 4)],
        };
        let server = thread::spawn(move || {
            let mut paths = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let line = String::from_utf8(request).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_string();
                let body: &[u8] = if path.ends_with("/a.onnx") {
                    b"abc"
                } else {
                    b"defg"
                };
                stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                            .as_bytes(),
                    )
                    .unwrap();
                stream.write_all(body).unwrap();
                paths.push(path);
            }
            paths
        });
        download_one(root.path(), &manifest).unwrap();
        assert_eq!(server.join().unwrap(), ["/rev/a.onnx", "/rev/tokens.txt"]);
        assert_eq!(fs::read(root.path().join("pinned/a.onnx")).unwrap(), b"abc");
        assert_eq!(
            fs::read(root.path().join("pinned/tokens.txt")).unwrap(),
            b"defg"
        );
        assert!(installed(root.path(), &manifest).is_ok());
        let left: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with('.') && !name.ends_with(".lock"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
    #[test]
    fn archive_install_does_not_require_or_trust_local_markers() {
        let root = tempfile::tempdir().unwrap();
        let manifest = ModelManifest {
            id: "archive",
            directory: "archive-root",
            files: &[("encoder.onnx", 3)],
            ..model(String::new())
        };
        let path = root.path().join(manifest.id);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("encoder.onnx"), b"abc").unwrap();
        assert!(installed(root.path(), &manifest).is_ok());
        fs::write(path.join("SHA256"), "old marker").unwrap();
        fs::write(path.join("FILES"), "encoder.onnx\t1\n").unwrap();
        assert!(installed(root.path(), &manifest).is_ok());
        fs::write(path.join("encoder.onnx"), b"a").unwrap();
        assert!(installed(root.path(), &manifest).is_err());
    }
    #[test]
    fn every_released_model_is_ready_without_markers_or_a_download() {
        for model in [
            &GIGAAM,
            &PARAKEET,
            &SILERO,
            &WHISPER_TINY,
            &PYANNOTE,
            &ERES2NET,
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(model.id);
            fs::create_dir(&path).unwrap();
            for (name, size) in model.files {
                fs::File::create(path.join(name))
                    .unwrap()
                    .set_len(*size)
                    .unwrap();
            }
            assert!(installed(root.path(), model).is_ok(), "{}", model.id);
            let unreachable = "http://127.0.0.1:1/must-not-download";
            let offline = ModelManifest {
                id: model.id,
                source: match model.source {
                    Source::Archive { sha256, size, .. } => Source::Archive {
                        url: unreachable,
                        sha256,
                        size,
                    },
                    Source::Files { sha256, .. } => Source::Files {
                        root: unreachable,
                        sha256,
                    },
                },
                directory: model.directory,
                files: model.files,
            };
            download_one(root.path(), &offline).unwrap();
            assert!(!path.join("FILES").exists());
            assert!(!path.join("SHA256").exists());
            for (name, size) in model.files {
                fs::OpenOptions::new()
                    .write(true)
                    .open(path.join(name))
                    .unwrap()
                    .set_len(size - 1)
                    .unwrap();
                assert!(
                    installed(root.path(), model).is_err(),
                    "{} / {name}: truncated",
                    model.id
                );
                fs::remove_file(path.join(name)).unwrap();
                assert!(
                    installed(root.path(), model).is_err(),
                    "{} / {name}: missing",
                    model.id
                );
                fs::File::create(path.join(name))
                    .unwrap()
                    .set_len(*size)
                    .unwrap();
                assert!(installed(root.path(), model).is_ok());
            }
        }
    }
}
