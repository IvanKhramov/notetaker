//! Deliberately no arbitrary application payloads in logs. Untrusted stderr and
//! panic payloads are reduced to categories, rather than regex-based scrubbing.
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: u64 = 2 * 1024 * 1024;
static LOG: OnceLock<Mutex<RollingLog>> = OnceLock::new();

pub fn directory() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("notetaker/logs"))
    }
    #[cfg(not(windows))]
    {
        directories::ProjectDirs::from("", "", "notetaker").map(|p| p.data_local_dir().join("logs"))
    }
}

pub fn init() -> io::Result<()> {
    let path = directory().ok_or_else(|| io::Error::other("No log directory"))?;
    fs::create_dir_all(&path)?;
    let _ = LOG.set(Mutex::new(RollingLog { path, limit: LIMIT }));
    event(concat!("app start version=", env!("CARGO_PKG_VERSION")));
    std::panic::set_hook(Box::new(|info| {
        // Payloads may contain transcript text or credentials from a dependency.
        event(&format!(
            "panic {}",
            redact(
                info.payload()
                    .downcast_ref::<&str>()
                    .copied()
                    .unwrap_or("panic")
            )
        ));
        if let Some(location) = info.location() {
            event(&format!(
                "panic location {}:{}",
                location.file(),
                location.line()
            ));
        }
        event(&format!(
            "backtrace {}",
            std::backtrace::Backtrace::force_capture()
        ));
        if let Some(path) = directory() {
            let _ = fs::write(path.join("crash.flag"), b"panic");
        }
    }));
    Ok(())
}

/// Only use with static event descriptions and non-content metadata.
pub fn event(message: &str) {
    if let Some(log) = LOG.get()
        && let Ok(mut log) = log.lock()
    {
        let _ = log.write(message);
    }
}

pub fn enabled() -> bool {
    LOG.get().is_some()
}

pub fn previous_crash() -> bool {
    directory().is_some_and(|p| p.join("crash.flag").is_file())
}

pub fn acknowledge_crash() {
    if let Some(path) = directory() {
        let _ = fs::remove_file(path.join("crash.flag"));
    }
}

pub fn redact(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") || lower.contains("panic") {
        "error [details redacted]"
    } else if lower.contains("download") {
        "download [details redacted]"
    } else {
        "output [details redacted]"
    }
}

pub fn worker_stderr(text: &str) {
    event(&format!("worker stderr: {}", redact(text)));
}

/// Bound memory even when a native library writes a line without a newline.
pub fn capture_stderr(reader: impl Read + Send + 'static) {
    capture(reader, "worker stderr");
}

pub fn capture_app_stderr(reader: impl Read + Send + 'static) {
    capture(reader, "app stderr");
}

fn capture(reader: impl Read + Send + 'static, source: &'static str) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut chunk = Vec::new();
            match reader.by_ref().take(4096).read_until(b'\n', &mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(_) => event(&format!(
                    "{source}: {}",
                    redact(&String::from_utf8_lossy(&chunk))
                )),
            }
        }
    });
}

struct RollingLog {
    path: PathBuf,
    limit: u64,
}
impl RollingLog {
    fn write(&mut self, message: &str) -> io::Result<()> {
        let line = format!(
            "{} {}\n",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            message.chars().take(16_000).collect::<String>()
        );
        let path = self.path.join("notetaker.log");
        if fs::metadata(&path).map_or(0, |m| m.len()) + line.len() as u64 > self.limit {
            for i in (1..=4).rev() {
                let from = if i == 1 {
                    path.clone()
                } else {
                    self.path.join(format!("notetaker.{}.log", i - 1))
                };
                let to = self.path.join(format!("notetaker.{i}.log"));
                if to.exists() {
                    fs::remove_file(&to)?;
                }
                if from.exists() {
                    fs::rename(from, to)?;
                }
            }
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(line.as_bytes())
    }
}

pub fn is_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "notetaker.log" || (1..=4).any(|i| n == format!("notetaker.{i}.log")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_stderr_never_leaks_content() {
        for text in [
            "token=sk-secret",
            "error transcript: private",
            "https://calendar/secret.ics",
            "My meeting notes",
            "Downloading key=private",
        ] {
            let safe = redact(text);
            assert!(!safe.contains("private"));
            assert!(!safe.contains("secret"));
            assert!(!safe.contains("https"));
            assert!(safe.contains("redacted"));
        }
    }
    #[test]
    fn rotation_keeps_five_bounded_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RollingLog {
            path: dir.path().into(),
            limit: 100,
        };
        for _ in 0..20 {
            log.write("test event with padding to force rotation")
                .unwrap();
        }
        let files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), 5);
        assert!(files.iter().all(|f| f.metadata().unwrap().len() <= 100));
    }
}
