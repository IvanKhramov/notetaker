#[path = "../../model_download.rs"]
mod model_download;
use anyhow::{Context, Result, bail, ensure};
use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

/// Every model is a set of files pinned by size and SHA-256 under an immutable
/// root. GitHub release assets of sherpa-onnx were replaced twice in November
/// 2026, so models come from Hugging Face commits where one exists.
pub struct ModelManifest {
    pub id: &'static str,
    /// Each file is fetched from `{root}/{name}`.
    pub root: &'static str,
    /// Installed files: name, byte length and SHA-256.
    pub files: &'static [(&'static str, u64, &'static str)],
    /// An archive among `files`, unpacked with the system tar (macOS and
    /// Windows 10+ ship bsdtar, which also reads zip), and the file it must yield.
    pub unpack: Option<(&'static str, &'static str)>,
}

pub const GIGAAM: ModelManifest = ModelManifest {
    id: "gigaam-v3",
    root: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/a6039be7cee829a9044a69ac0ebaf1c191217c97",
    files: &[
        (
            "encoder.int8.onnx",
            224_570_820,
            "369f35a71bf288d3b8e0391fabd8dba5f2314088d440bca474056b7b4b6e66bf",
        ),
        (
            "decoder.onnx",
            4_600_132,
            "38fc7475443ea2a26f63211ca350f73ac50fff824ab7a3876ee2bd610c53bbc4",
        ),
        (
            "joiner.onnx",
            2_712_896,
            "602ff7017a93311aad34df1437c8d7f49911353c13d6eae7a6ee7b041339465c",
        ),
        (
            "tokens.txt",
            13_354,
            "39abae20e692998290c574e606f11a9edef2902a1995463fcff63d1490cf22b7",
        ),
    ],
    unpack: None,
};
// The GitHub asset was re-exported on 2026-11-07 with different weights; this
// commit holds the original export that installed copies came from.
pub const PARAKEET: ModelManifest = ModelManifest {
    id: "parakeet-v3",
    root: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78",
    files: &[
        (
            "encoder.int8.onnx",
            652_184_281,
            "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
        ),
        (
            "decoder.int8.onnx",
            11_845_275,
            "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
        ),
        (
            "joiner.int8.onnx",
            6_355_277,
            "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
        ),
        (
            "tokens.txt",
            93_939,
            "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        ),
    ],
    unpack: None,
};
// No Hugging Face copy of this version exists; the asset is unchanged since 2025-07.
pub const SILERO: ModelManifest = ModelManifest {
    id: "silero-vad",
    root: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models",
    files: &[(
        "silero_vad.onnx",
        643_854,
        "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    )],
    unpack: None,
};

/// Multilingual tiny, not tiny.en. Only the int8 weights are downloaded.
pub const WHISPER_TINY: ModelManifest = ModelManifest {
    id: "whisper-tiny",
    root: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-tiny/resolve/65176e2deb88badc814a94058666cadccc29b61c",
    files: &[
        (
            "tiny-encoder.int8.onnx",
            12_937_772,
            "d24fb083ae3b1041fc24e97971d60e280c9342201fbb67b0ab428a8b4a51a434",
        ),
        (
            "tiny-decoder.int8.onnx",
            89_855_401,
            "d2fece8dd42771f1df975c6c0445770d0c292bf7547c2cae04a6c0cc57540925",
        ),
    ],
    unpack: None,
};

pub const PYANNOTE: ModelManifest = ModelManifest {
    id: "pyannote-3.0",
    root: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/9403a6902bb58e3d5ae8c7e77c3422de279db2e0",
    files: &[
        (
            "model.int8.onnx",
            1_540_506,
            "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d",
        ),
        (
            "LICENSE",
            1_061,
            "14d7016ad68e7394d6e6b78d96cc2ae431c905287b89674cfdf021e79e62b8ba",
        ),
    ],
    unpack: None,
};
// The GitHub asset was replaced on 2026-11-19 with a file one byte longer.
pub const ERES2NET: ModelManifest = ModelManifest {
    id: "eres2net-base",
    root: "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/0743f301363dec56491a490f6d6cbc9d67f9a3bf",
    files: &[(
        "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
        39_593_761,
        "1a331345f04805badbb495c775a6ddffcdd1a732567d5ec8b3d5749e3c7a5e4b",
    )],
    unpack: None,
};

/// llama.cpp server for the built-in local AI, pinned to build b12292 (2026-11-11).
const LLAMA_ROOT: &str = "https://github.com/ggml-org/llama.cpp/releases/download/b12292";
#[cfg(target_os = "macos")]
pub const LLAMA_SERVER: ModelManifest = ModelManifest {
    id: notetaker_runtime::local_llm::SERVER_ID,
    root: LLAMA_ROOT,
    files: &[(
        "llama-b12292-bin-macos-arm64.tar.gz",
        16_870_580,
        "94b860a529a64aecfe20c4b9ccb9ca6fb4cf9126b019f3380ee02d7951ca11ef",
    )],
    unpack: Some((
        "llama-b12292-bin-macos-arm64.tar.gz",
        notetaker_runtime::local_llm::SERVER,
    )),
};
// The CPU build runs everywhere; the Vulkan one needs a GPU driver.
#[cfg(windows)]
pub const LLAMA_SERVER: ModelManifest = ModelManifest {
    id: notetaker_runtime::local_llm::SERVER_ID,
    root: LLAMA_ROOT,
    files: &[(
        "llama-b12292-bin-win-cpu-x64.zip",
        25_057_882,
        "12017843d67f560ce1bacf771dccdb189ff651a2dde684cbb49634c29df5b751",
    )],
    unpack: Some((
        "llama-b12292-bin-win-cpu-x64.zip",
        notetaker_runtime::local_llm::SERVER,
    )),
};
#[cfg(not(any(target_os = "macos", windows)))]
pub const LLAMA_SERVER: ModelManifest = ModelManifest {
    id: notetaker_runtime::local_llm::SERVER_ID,
    root: LLAMA_ROOT,
    files: &[(
        "llama-b12292-bin-ubuntu-x64.tar.gz",
        23_283_641,
        "ffaf53f17a4601493e6ffb5869c9f0c7d400bb7e124fe0096eee3b65a5862568",
    )],
    unpack: Some((
        "llama-b12292-bin-ubuntu-x64.tar.gz",
        notetaker_runtime::local_llm::SERVER,
    )),
};
/// Weights of the built-in local AI.
pub const LOCAL_LLM: ModelManifest = ModelManifest {
    id: notetaker_runtime::local_llm::MODEL_ID,
    root: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523",
    files: &[(
        notetaker_runtime::local_llm::MODEL_FILE,
        2_740_937_888,
        "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
    )],
    unpack: None,
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
    } else if id == "local-llm" {
        installed(&root, &LLAMA_SERVER)?;
        installed(&root, &LOCAL_LLM)?;
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
    if let Some((_, required)) = model.unpack {
        ensure!(
            path.join(required).is_file(),
            "Файл отсутствует: {}; повторите notetaker-asr download {}",
            path.join(required).display(),
            model.id
        );
    }
    for (file, expected, _) in model.files {
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
    // Whole-archive partials of versions up to 0.37.0 can never be resumed.
    let archive = root.join(format!(".{}.part", model.id));
    if archive.exists() {
        fs::remove_file(&archive)?;
    }
    let extracted = staging.join("extracted");
    fs::create_dir(&extracted)?;
    let mut partials = Vec::new();
    for (name, size, sha256) in model.files {
        let label = format!("{}/{name}", model.id);
        let partial = root.join(format!(".{}-{name}.part", model.id));
        fetch(
            &format!("{}/{name}", model.root),
            &partial,
            *size,
            sha256,
            &label,
        )?;
        // The installed inode must never have been open as a writable partial.
        fs::copy(&partial, extracted.join(name))?;
        eprintln!("NOTETAKER_MODEL_INSTALLED {label}");
        partials.push(partial);
    }
    if let Some((archive, _)) = model.unpack {
        let status = std::process::Command::new("tar")
            .arg("-xf")
            .arg(extracted.join(archive))
            .arg("-C")
            .arg(&extracted)
            .status()
            .context("Не удалось запустить tar")?;
        ensure!(
            status.success(),
            "Не удалось распаковать {archive}: {status}"
        );
    }
    check_files(&extracted, model)?;
    if root.join(model.id).exists() {
        fs::remove_dir_all(root.join(model.id))?;
    }
    fs::rename(&extracted, root.join(model.id))?;
    for partial in partials {
        fs::remove_file(partial)?;
    }
    fs::remove_dir_all(staging)?;
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
        "local-llm" => vec![&LLAMA_SERVER, &LOCAL_LLM],
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
        // Progress and installation are reported per file under these ids.
        for (name, size, _) in model.files {
            println!("NOTETAKER_DOWNLOAD_PLAN {}/{name} {size} {ready}", model.id);
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
            unpack: None,
            root: Box::leak(url.into_boxed_str()),
            files: Box::leak(Box::new([(
                "model.bin",
                6,
                &*Box::leak(format!("{:x}", Sha256::digest(b"abcdef")).into_boxed_str()),
            )])),
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
        let partial = root.path().join(".neutral-model.bin.part");
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
            unpack: None,
            root: base,
            files: Box::leak(Box::new([
                ("a.onnx", 3, digest(b"abc")),
                ("tokens.txt", 4, digest(b"defg")),
            ])),
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
    fn install_does_not_require_or_trust_local_markers() {
        let root = tempfile::tempdir().unwrap();
        let manifest = ModelManifest {
            id: "archive",
            files: &[("encoder.onnx", 3, "")],
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
            &LOCAL_LLM,
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(model.id);
            fs::create_dir(&path).unwrap();
            for (name, size, _) in model.files {
                fs::File::create(path.join(name))
                    .unwrap()
                    .set_len(*size)
                    .unwrap();
            }
            assert!(installed(root.path(), model).is_ok(), "{}", model.id);
            let unreachable = "http://127.0.0.1:1/must-not-download";
            let offline = ModelManifest {
                id: model.id,
                root: unreachable,
                files: model.files,
                unpack: model.unpack,
            };
            download_one(root.path(), &offline).unwrap();
            assert!(!path.join("FILES").exists());
            assert!(!path.join("SHA256").exists());
            for (name, size, _) in model.files {
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
