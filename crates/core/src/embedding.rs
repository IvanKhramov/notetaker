//! Small stdio client: the main app never links an inference runtime/tokenizer.
use crate::{Embedder, Error, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        mpsc::{self, Receiver},
    },
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
    model_dir().is_ok_and(|p| p.join("model.onnx").is_file() && p.join("tokenizer.json").is_file())
}
pub struct LocalEmbedder {
    child: Arc<Mutex<Child>>,
    input: ChildStdin,
    output: Receiver<std::io::Result<String>>,
}
struct Workers {
    stopped: bool,
    children: Vec<Weak<Mutex<Child>>>,
}
static WORKERS: Mutex<Workers> = Mutex::new(Workers {
    stopped: false,
    children: Vec::new(),
});
/// Terminal application shutdown: stop indexing and cached query workers too.
pub fn shutdown_workers() {
    let mut workers = WORKERS.lock().unwrap_or_else(|e| e.into_inner());
    workers.stopped = true;
    for child in workers.children.iter().filter_map(Weak::upgrade) {
        let mut child = child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
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
        let mut workers = WORKERS.lock().unwrap_or_else(|e| e.into_inner());
        if workers.stopped {
            return Err(Error::Embedding("Приложение завершает работу".into()));
        }
        workers.children.retain(|child| child.strong_count() > 0);
        let mut child = command.spawn().map_err(|_| {
            Error::Embedding("Не найден notetaker-embed рядом с приложением".into())
        })?;
        let input = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let child = Arc::new(Mutex::new(child));
        workers.children.push(Arc::downgrade(&child));
        drop(workers);
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
                let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).kill();
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
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(all(test, unix))]
mod shutdown_tests {
    use super::*;
    #[test]
    fn shutdown_reaps_all_registered_local_search_workers() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exec sleep 60"])
            .spawn()
            .unwrap();
        let child = Arc::new(Mutex::new(child));
        WORKERS
            .lock()
            .unwrap()
            .children
            .push(Arc::downgrade(&child));
        shutdown_workers();
        assert!(WORKERS.lock().unwrap().stopped);
        assert!(child.lock().unwrap().try_wait().unwrap().is_some());
    }
}
