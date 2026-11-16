//! Small stdio client: the main app never links an inference runtime/tokenizer.
use crate::{Embedder, Error, Result};
use directories::ProjectDirs;
use notetaker_runtime::process::{Child, Kind};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

pub const MODEL_ID: &str = "multilingual-e5-small-int8-761b726-mean-v1";
pub const MODEL_DIRECTORY: &str = "multilingual-e5-small";
pub const MODEL_ROOT: &str = "https://huggingface.co/Xenova/multilingual-e5-small/resolve/761b726dd34fb83930e26aab4e9ac3899aa1fa78";
pub const MODEL_FILES: &[(&str, &str, u64, &str)] = &[
    (
        "onnx/model_quantized.onnx",
        "model.onnx",
        118_308_185,
        "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193",
    ),
    (
        "tokenizer.json",
        "tokenizer.json",
        17_082_730,
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
];
#[derive(Serialize, Deserialize)]
pub struct EmbedRequest {
    pub text: String,
    pub query: bool,
}
#[derive(Serialize, Deserialize)]
pub struct EmbedResponse {
    #[serde(default)]
    pub vector: Vec<f32>,
    pub error: Option<String>,
}
// Desktop preference, separate from the process environment (which is unsafe to
// mutate once workers exist). Child processes receive the resolved path explicitly.
static MODELS_DIRECTORY: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

pub fn set_models_dir(path: PathBuf) {
    *MODELS_DIRECTORY.write().expect("models directory lock") = Some(path);
}

pub fn models_dir() -> Result<PathBuf> {
    if let Some(path) = MODELS_DIRECTORY
        .read()
        .expect("models directory lock")
        .clone()
    {
        return Ok(path);
    }
    if let Some(path) = std::env::var_os("NOTETAKER_MODELS_DIR") {
        return Ok(PathBuf::from(path));
    }
    ProjectDirs::from("", "", "notetaker")
        .map(|d| d.data_local_dir().join("models"))
        .ok_or_else(|| Error::Embedding("Не найден каталог моделей".into()))
}
pub fn model_dir() -> Result<PathBuf> {
    Ok(models_dir()?.join(MODEL_DIRECTORY))
}
pub fn worker_install_path() -> Result<PathBuf> {
    Ok(models_dir()?
        .join(".workers")
        .join(env!("CARGO_PKG_VERSION"))
        .join(if cfg!(windows) {
            "notetaker-embed.exe"
        } else {
            "notetaker-embed"
        }))
}
pub fn worker_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NOTETAKER_EMBED_BIN") {
        return PathBuf::from(path);
    }
    let name = if cfg!(windows) {
        "notetaker-embed.exe"
    } else {
        "notetaker-embed"
    };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.with_file_name(name))
        .filter(|p| p.is_file())
        .or_else(|| worker_install_path().ok().filter(|p| p.is_file()))
        .unwrap_or_else(|| name.into())
}
pub fn installed() -> bool {
    model_dir().is_ok_and(|dir| installed_at(&dir))
}
/// The GUI and the worker use the same size check without loading ONNX weights.
pub fn installed_at(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|(_, name, size, _)| {
        std::fs::metadata(dir.join(name))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == *size)
    })
}
pub struct LocalEmbedder {
    child: Child,
    input: ChildStdin,
    output: Receiver<std::io::Result<String>>,
}
/// Terminal application shutdown stops every registered search worker.
pub fn shutdown_workers() {
    notetaker_runtime::process::terminate_kind(Kind::Embedding);
}
impl LocalEmbedder {
    pub fn start() -> Result<Self> {
        if !installed() {
            return Err(Error::Embedding("Модель поиска ещё загружается".into()));
        }
        let mut command = Command::new(worker_path());
        command
            .arg("serve")
            .arg(model_dir()?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000 | 0x00004000); // Hidden, below normal.
        }
        let mut child =
            notetaker_runtime::process::spawn(&mut command, Kind::Embedding).map_err(|error| {
                Error::Embedding(format!("Не удалось запустить notetaker-embed: {error}"))
            })?;
        let input = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, output) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let result = reader.by_ref().take(1_048_577).read_line(&mut line);
                let value = match result {
                    Ok(0) => break,
                    Ok(_) if line.len() > 1_048_576 => {
                        Err(std::io::Error::other("Слишком большой ответ воркера"))
                    }
                    Ok(_) => Ok(line),
                    Err(e) => Err(e),
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output,
        })
    }
}
impl Embedder for LocalEmbedder {
    fn model_id(&self) -> &str {
        MODEL_ID
    }
    fn is_local(&self) -> bool {
        true
    }
    fn embed(&mut self, text: &str, query: bool) -> Result<Vec<f32>> {
        serde_json::to_writer(
            &mut self.input,
            &EmbedRequest {
                text: text.into(),
                query,
            },
        )?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        let line = match self.output.recv_timeout(Duration::from_secs(180)) {
            Ok(line) => line?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                return Err(Error::Embedding("Воркер не ответил за 180 с".into()));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Embedding(
                    "Воркер поиска завершился: проверьте модель в настройках".into(),
                ));
            }
        };
        let response: EmbedResponse = serde_json::from_str(&line)?;
        if let Some(error) = response.error {
            return Err(Error::Embedding(error));
        }
        Ok(response.vector)
    }
}
impl Drop for LocalEmbedder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, unix))]
mod shutdown_tests {
    use super::*;
    #[test]
    fn shutdown_reaps_all_registered_local_search_workers() {
        let mut asr = notetaker_runtime::process::spawn(
            Command::new("/bin/sh").args(["-c", "exec sleep 60"]),
            Kind::Asr,
        )
        .unwrap();
        let mut child = notetaker_runtime::process::spawn(
            Command::new("/bin/sh").args(["-c", "exec sleep 60"]),
            Kind::Embedding,
        )
        .unwrap();
        shutdown_workers();
        assert!(
            asr.try_wait().unwrap().is_none(),
            "normal exit must allow the final ASR pass to finish"
        );
        asr.kill().unwrap();
        assert!(!asr.wait().unwrap().success());
        assert!(!child.wait().unwrap().success());
        assert!(
            notetaker_runtime::process::spawn(&mut Command::new("/bin/sh"), Kind::Embedding)
                .is_err()
        );
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;
    #[test]
    fn installed_embedding_model_rejects_truncated_missing_or_non_file_entries() {
        let root = tempfile::tempdir().unwrap();
        for (_, name, size, _) in MODEL_FILES {
            std::fs::File::create(root.path().join(name))
                .unwrap()
                .set_len(*size)
                .unwrap();
        }
        assert!(installed_at(root.path()));
        for (_, name, size, _) in MODEL_FILES {
            std::fs::OpenOptions::new()
                .write(true)
                .open(root.path().join(name))
                .unwrap()
                .set_len(size - 1)
                .unwrap();
            assert!(!installed_at(root.path()), "truncated {name}");
            std::fs::remove_file(root.path().join(name)).unwrap();
            assert!(!installed_at(root.path()), "missing {name}");
            std::fs::create_dir(root.path().join(name)).unwrap();
            assert!(!installed_at(root.path()), "directory {name}");
            std::fs::remove_dir(root.path().join(name)).unwrap();
            std::fs::File::create(root.path().join(name))
                .unwrap()
                .set_len(*size)
                .unwrap();
            assert!(installed_at(root.path()));
        }
    }
}
