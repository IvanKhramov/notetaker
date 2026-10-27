//! Blocking network and archive work never run on eframe's event thread.
use anyhow::{Context, Result, ensure};
use eframe::egui;
use notetaker_runtime::{
    diagnostics, paths,
    update::{self, Release},
    windows,
};
use std::{
    fs::{self, File},
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_INSTALLER: u64 = 1024 * 1024 * 1024;

enum Message {
    Checked(Option<Release>),
    Prepared(Result<Prepared>),
}
struct Prepared {
    directory: tempfile::TempDir,
    install: PathBuf,
}
struct Helper {
    child: Child,
    prepared: Prepared,
    started: Instant,
}
pub struct Updates {
    next_check: Instant,
    receiver: Option<Receiver<Message>>,
    release: Option<Release>,
    prepared: Option<Prepared>,
    helper: Option<Helper>,
    message: Option<String>,
    exit: bool,
}
impl Default for Updates {
    fn default() -> Self {
        Self {
            next_check: Instant::now(),
            receiver: None,
            release: None,
            prepared: None,
            helper: None,
            message: None,
            exit: false,
        }
    }
}
impl Updates {
    /// Returns true only after the copied helper acknowledges startup. The app
    /// then uses its normal flush/shutdown path before any files are replaced.
    pub fn tick(&mut self, ctx: &egui::Context, enabled: bool, busy: bool) -> bool {
        if !enabled {
            self.release = None;
        }
        if enabled
            && !self.exit
            && self.receiver.is_none()
            && self.prepared.is_none()
            && self.helper.is_none()
            && Instant::now() >= self.next_check
        {
            self.next_check = Instant::now() + INTERVAL;
            let (tx, rx) = mpsc::channel();
            self.receiver = Some(rx);
            let context = ctx.clone();
            std::thread::spawn(move || {
                let release = check().ok().flatten(); // Offline/rate limits are quiet.
                let _ = tx.send(Message::Checked(release));
                context.request_repaint();
            });
        }
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(message) => {
                    self.receiver = None;
                    match message {
                        Message::Checked(release) => {
                            if enabled {
                                self.release = release;
                            }
                        }
                        Message::Prepared(Ok(prepared)) => self.prepared = Some(prepared),
                        Message::Prepared(Err(_)) => {
                            diagnostics::event("update preparation failed");
                            self.message = Some("Не удалось подготовить обновление. Проверьте соединение и повторите позже.".into());
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.receiver = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !busy
            && !self.exit
            && let Some(prepared) = self.prepared.take()
        {
            match start_helper(prepared) {
                Ok(helper) => self.helper = Some(helper),
                Err(_) => {
                    self.message = Some(
                        "Не удалось запустить обновление. Приложение продолжает работать.".into(),
                    )
                }
            }
        }
        if let Some(helper) = &mut self.helper {
            if helper.started.elapsed() > Duration::from_secs(15)
                || !matches!(helper.child.try_wait(), Ok(None))
            {
                let _ = helper.child.kill();
                let _ = helper.child.wait();
                self.helper = None;
                self.message = Some("Обновление не запустилось. Повторите позже.".into());
                return false;
            }
            if !busy && helper.prepared.directory.path().join("ready").is_file() {
                // The helper now owns the temp directory and waits for our PID.
                let helper = self.helper.take().expect("helper");
                let _ = helper.prepared.directory.keep();
                self.exit = true;
                return true;
            }
        }
        false
    }
    pub fn ui(&mut self, ctx: &egui::Context, busy: bool) {
        if let Some(release) = self.release.clone() {
            let mut later = false;
            egui::Window::new(format!(
                "Доступна версия {}",
                release.tag_name.trim_start_matches('v')
            ))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_BOTTOM, [-20.0, -20.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !busy
                                && self.receiver.is_none()
                                && self.prepared.is_none()
                                && self.helper.is_none()
                                && !self.exit,
                            egui::Button::new("Обновить"),
                        )
                        .clicked()
                    {
                        let (tx, rx) = mpsc::channel();
                        self.receiver = Some(rx);
                        let context = ctx.clone();
                        let release = release.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(Message::Prepared(prepare(&release)));
                            context.request_repaint();
                        });
                    }
                    if ui.button("Что нового").clicked() {
                        let _ = windows::open_url(&release.notes_url());
                    }
                    later = ui
                        .add_enabled(
                            self.receiver.is_none()
                                && self.prepared.is_none()
                                && self.helper.is_none(),
                            egui::Button::new("Позже"),
                        )
                        .clicked();
                });
                if busy {
                    ui.small("Обновление доступно после завершения текущих задач.");
                }
                if self.receiver.is_some() || self.prepared.is_some() || self.helper.is_some() {
                    ui.small("Подготовка обновления…");
                }
            });
            if later {
                self.release = None;
            }
        }
        if let Some(message) = self.message.clone() {
            egui::Window::new("Обновление")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(message);
                    if ui.button("Закрыть").clicked() {
                        self.message = None;
                    }
                });
        }
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        // Only unacknowledged helpers remain owned here. They must not outlive
        // a normal user-requested exit and lose their temporary installer.
        if let Some(helper) = &mut self.helper {
            let _ = helper.child.kill();
            let _ = helper.child.wait();
        }
    }
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("notetaker/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}
fn check() -> Result<Option<Release>> {
    let response = client()?
        .get("https://api.github.com/repos/IvanKhramov/notetaker/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()?
        .error_for_status()?;
    let mut text = String::new();
    response.take(1024 * 1024).read_to_string(&mut text)?;
    let release: Release = serde_json::from_str(&text)?;
    if release.newer_than(env!("CARGO_PKG_VERSION"))? && release.assets().is_ok() {
        Ok(Some(release))
    } else {
        Ok(None)
    }
}
fn prepare(release: &Release) -> Result<Prepared> {
    let started = Instant::now();
    let executable = paths::executable()?;
    let install = executable
        .parent()
        .context("Нет каталога установки")?
        .to_owned();
    let root = update_directory()?;
    fs::create_dir_all(&root)?;
    let directory = tempfile::Builder::new()
        .prefix("notetaker-update-")
        .tempdir_in(root)?;
    // Copy the current CLI, not an unverified downloaded executable.
    fs::copy(
        install.join("notetaker.exe"),
        directory.path().join("notetaker-updater.exe"),
    )?;
    let (installer, checksum) = release.assets()?;
    let client = client()?;
    let mut checksum_text = String::new();
    client
        .get(&checksum.browser_download_url)
        .send()?
        .error_for_status()?
        .take(4096)
        .read_to_string(&mut checksum_text)?;
    let mut response = client
        .get(&installer.browser_download_url)
        .send()?
        .error_for_status()?;
    ensure!(
        response.content_length().is_none_or(|n| n <= MAX_INSTALLER),
        "Слишком большой установщик"
    );
    let setup_path = directory.path().join("setup.exe");
    let mut setup = File::create(&setup_path)?;
    let bytes = std::io::copy(&mut (&mut response).take(MAX_INSTALLER + 1), &mut setup)?;
    ensure!(bytes <= MAX_INSTALLER, "Слишком большой установщик");
    drop(setup);
    update::verify_sha256(&setup_path, &checksum_text, &installer.name)?;
    diagnostics::event("update prepared");
    diagnostics::event(&format!(
        "update preparation duration_ms={}",
        started.elapsed().as_millis()
    ));
    Ok(Prepared { directory, install })
}

fn update_directory() -> Result<PathBuf> {
    diagnostics::directory()
        .and_then(|p| p.parent().map(|p| p.join("updates")))
        .context("Нет каталога обновлений")
}

pub fn cleanup_finished() {
    std::thread::spawn(|| {
        let Ok(root) = update_directory() else {
            return;
        };
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("notetaker-update-")
                && entry.path().join("finished").is_file()
            {
                // The helper may still be returning from CreateProcess. Windows
                // keeps its exe locked until then; retry without blocking egui.
                for _ in 0..5 {
                    if fs::remove_dir_all(entry.path()).is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
}
fn start_helper(prepared: Prepared) -> Result<Helper> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(prepared.directory.path().join("notetaker-updater.exe"));
    command
        .arg("self-update-apply")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--install")
        .arg(&prepared.install)
        .arg("--ready-file")
        .arg(prepared.directory.path().join("ready"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000); // CREATE_NO_WINDOW
    command
        .arg("--installer")
        .arg(prepared.directory.path().join("setup.exe"));
    let mut child = command.spawn()?;
    if let Some(stderr) = child.stderr.take() {
        diagnostics::capture_stderr(stderr);
    }
    Ok(Helper {
        child,
        prepared,
        started: Instant::now(),
    })
}
