//! Owned worker processes. External applications launched by the shell are not workers.
use std::{
    collections::HashSet,
    io::{self, Read},
    process::{
        Child as SystemChild, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output,
    },
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Asr,
    Embedding,
    Background,
}
#[derive(Default)]
struct Registry {
    stopped: bool,
    stopped_kinds: HashSet<Kind>,
    children: Vec<Weak<Inner>>,
    next: u64,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}
struct State {
    child: SystemChild,
    reaped: bool,
}
struct Inner {
    state: Mutex<State>,
    kind: Kind,
    id: u64,
    #[cfg(windows)]
    job: Job,
}
impl Inner {
    fn kill(&self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.reaped {
            return Ok(());
        }
        #[cfg(unix)]
        {
            terminate_tree(state.child.id());
            state.child.kill()?;
        }
        #[cfg(windows)]
        {
            self.job.terminate()?;
        }
        if state.child.try_wait()?.is_some() {
            state.reaped = true;
        }
        Ok(())
    }
    fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let status = state.child.try_wait()?;
        if status.is_some() && !state.reaped {
            terminate_tree(
                state.child.id(),
                #[cfg(windows)]
                &self.job,
            );
            state.reaped = true;
        }
        Ok(status)
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if !state.reaped {
            terminate_tree(
                state.child.id(),
                #[cfg(windows)]
                &self.job,
            );
            let _ = state.child.kill();
            let _ = state.child.wait();
        }
    }
}
/// A cancellation token keeps a weak reference, never a reusable PID.
#[derive(Clone)]
pub struct Handle {
    inner: Weak<Inner>,
    id: u64,
}
impl Handle {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn terminate(&self) {
        if let Some(inner) = self.inner.upgrade() {
            let _ = inner.kill();
        }
    }
}
pub struct Child {
    inner: Arc<Inner>,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}
impl Child {
    pub fn id(&self) -> u32 {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .child
            .id()
    }
    pub fn handle(&self) -> Handle {
        Handle {
            inner: Arc::downgrade(&self.inner),
            id: self.inner.id,
        }
    }
    pub fn kill(&mut self) -> io::Result<()> {
        self.inner.kill()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.inner.try_wait()
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin.take();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        fn read(stream: Option<impl Read>) -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                stream.read_to_end(&mut bytes)?;
            }
            Ok(bytes)
        }
        let stdout = self.stdout.take();
        let stderr = self.stderr.take();
        let out = std::thread::spawn(move || read(stdout));
        let err = std::thread::spawn(move || read(stderr));
        let status = self.wait()?;
        Ok(Output {
            status,
            stdout: out
                .join()
                .map_err(|_| io::Error::other("worker stdout reader stopped"))??,
            stderr: err
                .join()
                .map_err(|_| io::Error::other("worker stderr reader stopped"))??,
        })
    }
}
pub fn spawn(command: &mut Command, kind: Kind) -> io::Result<Child> {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    if registry.stopped || registry.stopped_kinds.contains(&kind) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Приложение завершает работу",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let job = match Job::new(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    registry.next += 1;
    let inner = Arc::new(Inner {
        state: Mutex::new(State {
            child,
            reaped: false,
        }),
        kind,
        id: registry.next,
        #[cfg(windows)]
        job,
    });
    registry.children.retain(|child| child.strong_count() > 0);
    registry.children.push(Arc::downgrade(&inner));
    Ok(Child {
        inner,
        stdin,
        stdout,
        stderr,
    })
}
pub fn output(command: &mut Command) -> io::Result<Output> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    spawn(command, Kind::Background)?.wait_with_output()
}
/// Signal every process tree before returning, without waiting for a blocked read.
/// Owners reap their children; an immediate nonblocking reap also handles idle clients.
fn terminate(kind: Option<Kind>) {
    let children = {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(kind) = kind {
            registry.stopped_kinds.insert(kind);
        } else {
            registry.stopped = true;
        }
        registry
            .children
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|child| kind.is_none_or(|kind| child.kind == kind))
            .collect::<Vec<_>>()
    };
    for child in &children {
        let _ = child.kill();
    }
    for child in &children {
        let _ = child.try_wait();
    }
}
pub fn terminate_all() {
    terminate(None);
}
pub fn terminate_kind(kind: Kind) {
    terminate(Some(kind));
}

// Same process-group termination as CLI sign-in, shared by every worker.
#[cfg(unix)]
fn terminate_tree(pid: u32) {
    // SAFETY: managed children start in their own process group; its id is the child pid.
    #[allow(unsafe_code)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}
#[cfg(windows)]
fn terminate_tree(_: u32, job: &Job) {
    let _ = job.terminate();
}

#[cfg(windows)]
struct Job(std::os::windows::io::OwnedHandle);
#[cfg(windows)]
impl Job {
    // https://learn.microsoft.com/windows/win32/procthread/job-objects
    #[allow(unsafe_code)]
    fn new(child: &SystemChild) -> io::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows::{
            Win32::{Foundation::HANDLE, System::JobObjects::*},
            core::PCWSTR,
        };
        // SAFETY: unnamed job, owned handle, initialized limits and a live process handle.
        unsafe {
            let handle = CreateJobObjectW(None, PCWSTR::null()).map_err(io::Error::other)?;
            let job = Self(std::os::windows::io::OwnedHandle::from_raw_handle(handle.0));
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
            .map_err(io::Error::other)?;
            AssignProcessToJobObject(handle, HANDLE(child.as_raw_handle()))
                .map_err(io::Error::other)?;
            Ok(job)
        }
    }
    #[allow(unsafe_code)]
    fn terminate(&self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the owned job handle stays live for this call.
        unsafe {
            windows::Win32::System::JobObjects::TerminateJobObject(
                windows::Win32::Foundation::HANDLE(self.0.as_raw_handle()),
                1,
            )
        }
        .map_err(io::Error::other)
    }
}
