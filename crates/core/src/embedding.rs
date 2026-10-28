//! Small stdio client: the main app never links an inference runtime/tokenizer.
use crate::{Embedder, Error, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

pub const MODEL_ID: &str = "multilingual-e5-small-int8-761b726-mean-v1";
pub const MODEL_DIRECTORY: &str = "multilingual-e5-small";
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
pub fn models_dir() -> Result<PathBuf> {
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
    model_dir().is_ok_and(|p| p.join("model.onnx").is_file() && p.join("tokenizer.json").is_file())
}
pub struct LocalEmbedder {
    child: Child,
    input: ChildStdin,
    output: Receiver<std::io::Result<String>>,
}
impl LocalEmbedder {
    pub fn start() -> Result<Self> {
        if !installed() {
            return Err(Error::Embedding(
                "Скачайте модель «Поиск по смыслу» в настройках".into(),
            ));
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
        let mut child = command.spawn().map_err(|_| {
            Error::Embedding("Не найден notetaker-embed рядом с приложением".into())
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
