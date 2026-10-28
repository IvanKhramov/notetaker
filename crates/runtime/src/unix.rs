//! Per-user instance lock and local show/quit signalling.
#![allow(unsafe_code)]

use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Signals {
    show: AtomicBool,
    quit: AtomicBool,
    denied: AtomicBool,
    stop: AtomicBool,
}
static SIGNALS: Mutex<Weak<Signals>> = Mutex::new(Weak::new());

fn directory() -> Result<PathBuf> {
    Ok(directories::ProjectDirs::from("", "", "notetaker")
        .context("Нет каталога приложения")?
        .data_local_dir()
        .join("run"))
}
fn lock(directory: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("instance.lock"))?;
    // SAFETY: flock borrows a live descriptor; File releases the lock on close.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(error.into())
    }
}
fn connect(directory: &Path) -> Result<UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(directory.join("instance.sock")) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(error).context("Не удалось связаться с запущенным notetaker"),
        }
    }
}

pub struct Instance {
    _lock: File,
    socket: PathBuf,
    signals: Arc<Signals>,
    thread: Option<JoinHandle<()>>,
}
impl Instance {
    pub fn acquire() -> Result<Option<Self>> {
        let instance = Self::acquire_in(&directory()?)?;
        if let Some(instance) = &instance {
            *SIGNALS.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&instance.signals);
        }
        Ok(instance)
    }
    fn acquire_in(directory: &Path) -> Result<Option<Self>> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        let Some(file) = lock(directory)? else {
            connect(directory)?.write_all(b"S")?;
            return Ok(None);
        };
        let socket = directory.join("instance.sock");
        // Only the lock owner can remove a stale socket. Never unlink the lock.
        match fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let signals = Arc::new(Signals::default());
        let state = Arc::clone(&signals);
        let thread = thread::Builder::new()
            .name("instance-signals".into())
            .spawn(move || {
                let mut quitters = Vec::new();
                while !state.stop.load(Ordering::Acquire) {
                    while let Ok((mut stream, _)) = listener.accept() {
                        // Accepted sockets inherit O_NONBLOCK on macOS. Wait for a
                        // client that connects before it has written its command.
                        if stream.set_nonblocking(false).is_err()
                            || stream
                                .set_read_timeout(Some(Duration::from_millis(100)))
                                .is_err()
                            || stream
                                .set_write_timeout(Some(Duration::from_millis(100)))
                                .is_err()
                        {
                            continue;
                        }
                        let mut command = [0];
                        if stream.read_exact(&mut command).is_ok() {
                            match command[0] {
                                b'S' => state.show.store(true, Ordering::Release),
                                b'Q' => {
                                    state.quit.store(true, Ordering::Release);
                                    quitters.push(stream);
                                }
                                _ => {}
                            }
                        }
                    }
                    if state.denied.swap(false, Ordering::AcqRel) {
                        for mut stream in quitters.drain(..) {
                            let _ = stream.write_all(b"B");
                        }
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                for mut stream in quitters {
                    let _ = stream.write_all(b"X");
                }
            })?;
        Ok(Some(Self {
            _lock: file,
            socket,
            signals,
            thread: Some(thread),
        }))
    }
    fn take(signal: impl FnOnce(&Signals) -> &AtomicBool) -> bool {
        SIGNALS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
            .is_some_and(|s| signal(&s).swap(false, Ordering::AcqRel))
    }
    pub fn show_requested() -> bool {
        Self::take(|s| &s.show)
    }
    pub fn quit_requested() -> bool {
        Self::take(|s| &s.quit)
    }
    pub fn refuse_quit() {
        if let Some(signals) = SIGNALS.lock().unwrap_or_else(|e| e.into_inner()).upgrade() {
            signals.denied.store(true, Ordering::Release);
        }
    }
    pub fn request_quit() -> Result<()> {
        Self::request_quit_in(&directory()?)
    }
    fn request_quit_in(directory: &Path) -> Result<()> {
        if !directory.exists() || lock(directory)?.is_some() {
            return Ok(());
        }
        let mut stream = connect(directory)?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        stream.write_all(b"Q")?;
        let mut reply = [0];
        stream
            .read_exact(&mut reply)
            .context("Приложение не завершилось")?;
        match reply[0] {
            b'X' => {
                // The server sends X from Drop before its lock File is closed.
                // Do not let an updater relaunch against the exiting instance.
                wait_for_unlock(directory, Duration::from_secs(2))
            }
            b'B' => bail!("Приложение занято записью или чистовым проходом"),
            _ => bail!("Неизвестный ответ notetaker"),
        }
    }
}
fn wait_for_unlock(directory: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if lock(directory)?.is_some() {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Приложение не освободило блокировку"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.signals.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}

pub fn open_url(url: &str) -> Result<()> {
    open(Path::new(url))
}
pub fn open(path: &Path) -> Result<()> {
    let status = std::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(path)
    .status()?;
    ensure!(status.success(), "Не удалось открыть файл или ссылку");
    Ok(())
}

/// Route GUI stderr through the existing content filter, just like Windows.
pub fn capture_app_stderr() -> Result<()> {
    use std::os::fd::FromRawFd;
    let mut descriptors = [-1; 2];
    // SAFETY: pipe initializes two owned descriptors; dup2 retains the write end
    // as stderr. Children must not inherit the separate pipe descriptors.
    unsafe {
        ensure!(
            libc::pipe(descriptors.as_mut_ptr()) == 0,
            "Не удалось открыть stderr pipe"
        );
        let reader = File::from_raw_fd(descriptors[0]);
        let writer = File::from_raw_fd(descriptors[1]);
        for descriptor in descriptors {
            ensure!(
                libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) != -1,
                "Не удалось настроить stderr pipe"
            );
        }
        ensure!(
            libc::dup2(writer.as_raw_fd(), libc::STDERR_FILENO) != -1,
            "Не удалось подключить stderr pipe"
        );
        crate::diagnostics::capture_app_stderr(reader);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_agent() -> Result<PathBuf> {
    Ok(directories::UserDirs::new()
        .context("Нет домашнего каталога")?
        .home_dir()
        .join("Library/LaunchAgents/com.notetaker.app.plist"))
}
#[cfg(target_os = "macos")]
pub fn autostart() -> bool {
    launch_agent().is_ok_and(|p| p.is_file())
}
#[cfg(target_os = "macos")]
pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = launch_agent()?;
    if enabled {
        let executable = crate::paths::executable()?;
        let executable = xml(&executable.to_string_lossy());
        fs::create_dir_all(path.parent().context("Нет каталога LaunchAgents")?)?;
        fs::write(
            path,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.notetaker.app</string>
<key>ProgramArguments</key><array><string>{executable}</string></array>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#
            ),
        )?;
    } else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // RunAtLoad applies on the next login. Loading it now would start a copy.
    Ok(())
}
#[cfg(any(target_os = "macos", test))]
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wait(signal: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !signal.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn second_launch_requests_show_and_lock_survives_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("instance.sock"), "stale").unwrap();
        let instance = Instance::acquire_in(dir.path()).unwrap().unwrap();
        assert!(Instance::acquire_in(dir.path()).unwrap().is_none());
        wait(&instance.signals.show);
        drop(instance);
        assert!(Instance::acquire_in(dir.path()).unwrap().is_some());
    }
    #[test]
    fn quit_can_be_refused_then_acknowledged_on_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let instance = Instance::acquire_in(dir.path()).unwrap().unwrap();
        let path = dir.path().to_owned();
        let request = thread::spawn(move || Instance::request_quit_in(&path));
        wait(&instance.signals.quit);
        instance.signals.denied.store(true, Ordering::Release);
        assert!(request.join().unwrap().is_err());
        instance.signals.quit.store(false, Ordering::Release);
        let path = dir.path().to_owned();
        let request = thread::spawn(move || Instance::request_quit_in(&path));
        wait(&instance.signals.quit);
        drop(instance);
        request.join().unwrap().unwrap();
        let relaunched = Instance::acquire_in(dir.path()).unwrap().unwrap();
        drop(relaunched);
        Instance::request_quit_in(dir.path()).unwrap();
    }
    #[test]
    fn accepts_commands_written_after_connect() {
        let dir = tempfile::tempdir().unwrap();
        let instance = Instance::acquire_in(dir.path()).unwrap().unwrap();
        let mut stream = connect(dir.path()).unwrap();
        // Let the nonblocking listener accept before the command arrives.
        thread::sleep(Duration::from_millis(50));
        stream.write_all(b"S").unwrap();
        wait(&instance.signals.show);
    }
    #[test]
    fn quit_ack_waits_for_lock_release_before_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let file = lock(dir.path()).unwrap().unwrap();
        let listener = UnixListener::bind(dir.path().join("instance.sock")).unwrap();
        let path = dir.path().to_owned();
        let request = thread::spawn(move || {
            Instance::request_quit_in(&path).unwrap();
            Instance::acquire_in(&path).unwrap().unwrap()
        });
        let (mut stream, _) = listener.accept().unwrap();
        let mut command = [0];
        stream.read_exact(&mut command).unwrap();
        assert_eq!(command, *b"Q");
        stream.write_all(b"X").unwrap();
        thread::sleep(Duration::from_millis(50));
        assert!(!request.is_finished());
        assert!(wait_for_unlock(dir.path(), Duration::from_millis(20)).is_err());
        drop(file);
        drop(request.join().unwrap());
    }
    #[test]
    fn plist_escapes_paths() {
        assert_eq!(xml("/a&b/<c>\"'"), "/a&amp;b/&lt;c&gt;&quot;&apos;");
    }
}
