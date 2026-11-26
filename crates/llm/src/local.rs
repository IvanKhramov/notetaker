//! The built-in local AI: a pinned llama.cpp server, started on demand on the
//! loopback interface with a per-launch key, shared by the requests of this
//! process and stopped after it has been idle.
use anyhow::{Context, Result, bail, ensure};
use notetaker_runtime::local_llm::CONTEXT_TOKENS;
use notetaker_runtime::process::{self, Child, Kind};
use std::{
    fs::File,
    hash::{BuildHasher, RandomState},
    net::TcpListener,
    process::{Command, Stdio},
    sync::{Mutex, MutexGuard, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// Free the memory of the model when no request needs it.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// Loading a few gigabytes of weights from a slow disk.
const STARTUP: Duration = Duration::from_secs(180);

struct Server {
    child: Child,
    base_url: String,
    key: String,
    active: usize,
    last_used: Instant,
}

fn server() -> MutexGuard<'static, Option<Server>> {
    static SERVER: Mutex<Option<Server>> = Mutex::new(None);
    SERVER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Keeps the server running while a request uses it.
pub struct Lease(());

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(server) = server().as_mut() {
            server.active = server.active.saturating_sub(1);
            server.last_used = Instant::now();
        }
    }
}

/// The OpenAI-compatible base URL and key of the running server, starting it when needed.
pub fn acquire() -> Result<(String, String, Lease)> {
    let mut slot = server();
    if let Some(running) = slot.as_mut() {
        if running.child.try_wait()?.is_none() {
            running.active += 1;
            return Ok((running.base_url.clone(), running.key.clone(), Lease(())));
        }
        *slot = None;
    }
    let mut started = start()?;
    started.active = 1;
    let endpoint = (started.base_url.clone(), started.key.clone());
    *slot = Some(started);
    drop(slot);
    watch_idle();
    Ok((endpoint.0, endpoint.1, Lease(())))
}

fn start() -> Result<Server> {
    let executable = notetaker_core::local_llm::server_path()?;
    let model = notetaker_core::local_llm::model_path()?;
    ensure!(
        executable.is_file() && model.is_file(),
        "Локальный AI не скачан. Откройте «Настройки → AI» и включите «Локально»."
    );
    let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    let key = random_key();
    let log = executable
        .parent()
        .context("Нет каталога сервера")?
        .join("server.log");
    let mut command = Command::new(&executable);
    command
        .arg("--model")
        .arg(&model)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["--api-key", &key])
        .args(["--ctx-size", &CONTEXT_TOKENS.to_string()])
        // Notes need the answer, not a long hidden chain of thought.
        .args(["--reasoning-budget", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(&log)?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // llama-server is a console program; no window may flash per summary.
        command.creation_flags(0x0800_0000);
    }
    let mut child = process::spawn(&mut command, Kind::Background)
        .context("Не удалось запустить локальный AI")?;
    let health = format!("http://127.0.0.1:{port}/health");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let begun = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            bail!(
                "Локальный AI завершился при запуске ({status}): {}",
                tail(&log)
            );
        }
        // 503 while the weights load, 200 when ready.
        if client
            .get(&health)
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        if begun.elapsed() > STARTUP {
            let _ = child.kill();
            bail!(
                "Локальный AI не запустился за {} с: {}",
                STARTUP.as_secs(),
                tail(&log)
            );
        }
        thread::sleep(Duration::from_millis(250));
    }
    Ok(Server {
        child,
        base_url: format!("http://127.0.0.1:{port}/v1"),
        key,
        active: 0,
        last_used: Instant::now(),
    })
}

fn watch_idle() {
    static WATCHING: OnceLock<()> = OnceLock::new();
    WATCHING.get_or_init(|| {
        thread::spawn(|| {
            loop {
                thread::sleep(Duration::from_secs(30));
                let mut slot = server();
                if slot
                    .as_ref()
                    .is_some_and(|server| server.active == 0 && server.last_used.elapsed() > IDLE)
                    && let Some(mut server) = slot.take()
                {
                    let _ = server.child.kill();
                    let _ = server.child.wait();
                }
            }
        });
    });
}

/// Only this process may use the server, even though it listens on loopback.
fn random_key() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let first = RandomState::new().hash_one(nanos);
    let second = RandomState::new().hash_one((std::process::id(), nanos));
    format!("{first:016x}{second:016x}")
}

fn tail(log: &std::path::Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(3)..]
        .join(" ")
        .chars()
        .take(300)
        .collect()
}
