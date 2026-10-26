use crate::{
    AudioInput, PROTOCOL_VERSION, Provider, Request, Response, Segment, read_message, write_message,
};
use std::{
    io::{self, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("ASR worker I/O: {0}")]
    Io(#[from] io::Error),
    #[error("ASR worker exited or closed stdout (status: {0})")]
    Exited(String),
    #[error("ASR worker response timed out")]
    Timeout,
    #[error("ASR protocol: {0}")]
    Protocol(String),
    #[error("ASR worker: {0}")]
    Worker(String),
}

/// Cancels an in-flight request; the owning client kills/reaps the worker.
#[derive(Clone, Default)]
pub struct CancellationHandle(Arc<AtomicBool>);
impl CancellationHandle {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Owns and reaps a worker. Dropping the client also terminates a stuck worker.
/// One deadline covers serialization, stdin writes and all responses (including
/// progress). stderr is inherited, never parsed as JSON.
/// No ML dependency is linked into the caller.
pub struct AsrClient {
    child: Child,
    requests: Option<Sender<Request>>,
    written: Receiver<io::Result<()>>,
    writer: Option<JoinHandle<()>>,
    deadline: Option<Instant>,
    cancel: CancellationHandle,
    responses: Receiver<io::Result<Option<Response>>>,
    timeout: Duration,
}

impl AsrClient {
    pub fn spawn(binary: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::spawn_command(Command::new(binary.as_ref()), Duration::from_secs(300))
    }

    pub fn spawn_command(mut command: Command, timeout: Duration) -> Result<Self, ClientError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let (requests_tx, requests_rx) = mpsc::channel();
        let (written_tx, written) = mpsc::sync_channel(1);
        let writer = thread::spawn(move || {
            for request in requests_rx {
                let result = write_message(&mut stdin, &request);
                let done = result.is_err();
                if written_tx.send(result).is_err() || done {
                    break;
                }
            }
        });
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, responses) = mpsc::sync_channel(8);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = read_message(&mut reader);
                let done = !matches!(&message, Ok(Some(_)));
                if tx.send(message).is_err() || done {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            requests: Some(requests_tx),
            written,
            writer: Some(writer),
            deadline: None,
            cancel: CancellationHandle::default(),
            responses,
            timeout,
        };
        match client.receive()? {
            Response::Ready { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                Ok(client)
            }
            response => Err(ClientError::Protocol(format!(
                "expected ready v{PROTOCOL_VERSION}, got {response:?}"
            ))),
        }
    }

    pub fn cancellation_handle(&self) -> CancellationHandle {
        self.cancel.clone()
    }

    pub fn send(&mut self, request: &Request) -> Result<(), ClientError> {
        let deadline = Instant::now() + self.timeout;
        self.deadline = Some(deadline);
        if let Some(status) = self.child.try_wait()? {
            return Err(ClientError::Exited(status.to_string()));
        }
        self.requests
            .as_ref()
            .ok_or_else(|| ClientError::Protocol("stdin closed".into()))?
            .send(request.clone())
            .map_err(|_| ClientError::Exited("stdin closed".into()))?;
        let result = wait_until(&self.written, deadline, &self.cancel.0);
        match self.enforce_deadline(result)? {
            Ok(()) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn receive(&mut self) -> Result<Response, ClientError> {
        let deadline = *self
            .deadline
            .get_or_insert_with(|| Instant::now() + self.timeout);
        let result = wait_until(&self.responses, deadline, &self.cancel.0);
        match self.enforce_deadline(result)? {
            Ok(Some(response)) => Ok(response),
            Ok(None) => {
                let status = self
                    .child
                    .try_wait()?
                    .map_or_else(|| "stdout closed".into(), |s| s.to_string());
                Err(ClientError::Exited(status))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn enforce_deadline<T>(&mut self, result: Result<T, ClientError>) -> Result<T, ClientError> {
        if matches!(result, Err(ClientError::Timeout)) {
            self.terminate();
        }
        result
    }

    fn terminate(&mut self) {
        self.requests.take();
        // Closing stdin alone cannot interrupt write_all. Killing its reader
        // breaks the pipe first, then the writer can be joined safely.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }

    pub fn load_model(
        &mut self,
        model_id: &str,
        provider: Provider,
    ) -> Result<Provider, ClientError> {
        self.send(&Request::LoadModel {
            model_id: model_id.into(),
            provider,
        })?;
        match self.receive()? {
            Response::ModelLoaded {
                model_id: loaded,
                provider,
            } if loaded == model_id => Ok(provider),
            Response::Error { message, .. } => Err(ClientError::Worker(message)),
            response => Err(ClientError::Protocol(format!(
                "expected model_loaded, got {response:?}"
            ))),
        }
    }

    pub fn transcribe(
        &mut self,
        id: &str,
        input: AudioInput,
        channel: &str,
        offset_ms: u64,
    ) -> Result<Vec<Segment>, ClientError> {
        self.send(&Request::Transcribe {
            id: id.into(),
            input,
            channel: channel.into(),
            offset_ms,
        })?;
        loop {
            match self.receive()? {
                Response::Progress { id: received, .. } if received == id => {}
                Response::Segments {
                    id: received,
                    segments,
                } if received == id => return Ok(segments),
                Response::Error {
                    id: received,
                    message,
                } if received.as_deref().is_none_or(|r| r == id) => {
                    return Err(ClientError::Worker(message));
                }
                response => {
                    return Err(ClientError::Protocol(format!(
                        "unexpected transcription response: {response:?}"
                    )));
                }
            }
        }
    }

    pub fn shutdown(mut self) -> Result<(), ClientError> {
        self.send(&Request::Shutdown)?;
        self.requests.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(ClientError::Exited(status.to_string()))
                };
            }
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for AsrClient {
    fn drop(&mut self) {
        self.requests.take();
        self.terminate();
    }
}

fn wait_until<T>(
    receiver: &Receiver<T>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<T, ClientError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || cancel.load(Ordering::Acquire) {
            return Err(ClientError::Timeout);
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ClientError::Exited("pipe closed".into()));
            }
        }
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    // Fake workers are shell/PowerShell processes; on small CI runners several
    // cold PowerShell starts at once blow any startup timeout, so run one at a time.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn fake_command(script: &str) -> Command {
        #[cfg(unix)]
        {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            command
        }
        #[cfg(windows)]
        {
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script.replace("read -r request", "$null = [Console]::ReadLine()"),
            ]);
            command
        }
    }
    fn fake(script: &str) -> Result<AsrClient, ClientError> {
        // Cold PowerShell start on CI runners can exceed several seconds.
        AsrClient::spawn_command(fake_command(script), Duration::from_secs(30))
    }
    #[test]
    fn fake_worker_lifecycle() {
        let _serial = serial();
        let mut c = fake(r#"
            echo '{"type":"ready","protocol_version":1}'
            read -r request
            echo '{"type":"model_loaded","model_id":"gigaam-v3","provider":"cpu"}'
            read -r request
            echo '{"type":"progress","id":"1","fraction":0.5}'
            echo '{"type":"segments","id":"1","segments":[{"start_ms":5,"end_ms":10,"text":"hello"}]}'
            read -r request
        "#).unwrap();
        assert_eq!(
            c.load_model("gigaam-v3", Provider::Cpu).unwrap(),
            Provider::Cpu
        );
        assert_eq!(
            c.transcribe("1", AudioInput::pcm(&[]), "mic", 5).unwrap()[0].text,
            "hello"
        );
        c.shutdown().unwrap();
    }
    #[test]
    fn death_and_invalid_protocol() {
        let _serial = serial();
        assert!(fake("exit 7").is_err());
        assert!(fake("echo nonsense").is_err());
        assert!(fake(r#"echo '{"type":"ready","protocol_version":999}'"#).is_err());
        let mut c =
            fake(r#"echo '{"type":"ready","protocol_version":1}'; read -r request; exit 9"#)
                .unwrap();
        assert!(c.load_model("gigaam-v3", Provider::Cpu).is_err());
    }
    #[test]
    fn worker_errors_and_mismatched_ids() {
        let _serial = serial();
        let mut c = fake(r#"echo '{"type":"ready","protocol_version":1}'; read -r request; echo '{"type":"error","message":"missing model"}'; read -r request"#).unwrap();
        assert!(
            matches!(c.load_model("absent", Provider::Cpu), Err(ClientError::Worker(message)) if message == "missing model")
        );
        c.shutdown().unwrap();
        let mut c = fake(r#"echo '{"type":"ready","protocol_version":1}'; read -r request; echo '{"type":"segments","id":"wrong","segments":[]}'"#).unwrap();
        assert!(matches!(
            c.transcribe("expected", AudioInput::pcm(&[]), "mic", 0),
            Err(ClientError::Protocol(_))
        ));
    }

    #[test]
    fn timeout_and_drop_reap_worker() {
        let _serial = serial();
        let command = fake_command("read -r request");
        assert!(matches!(
            AsrClient::spawn_command(command, Duration::from_millis(30)),
            Err(ClientError::Timeout)
        ));
        let mut c =
            fake(r#"echo '{"type":"ready","protocol_version":1}'; read -r request"#).unwrap();
        #[cfg(target_os = "linux")]
        let pid = c.child.id();
        c.child.kill().unwrap();
        assert!(c.receive().is_err());
        drop(c);
        #[cfg(target_os = "linux")]
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    #[test]
    fn unread_stdin_times_out_for_large_requests_and_reaps_worker() {
        let _serial = serial();
        #[cfg(unix)]
        let script = r#"echo '{"type":"ready","protocol_version":1}'; exec sleep 2"#;
        #[cfg(windows)]
        let script = r#"echo '{"type":"ready","protocol_version":1}'; Start-Sleep -Seconds 5"#;
        let mut client = fake(script).unwrap();
        client.timeout = Duration::from_millis(200);
        let input = AudioInput::pcm(&vec![0.1; 1_000_000]);
        let start = Instant::now();
        let result = client.transcribe("blocked", input, "mic", 0);
        assert!(matches!(result, Err(ClientError::Timeout)), "{result:?}");
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(client.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn cancellation_interrupts_blocked_stdin_and_joins_writer() {
        let _serial = serial();
        #[cfg(unix)]
        let script = r#"echo '{"type":"ready","protocol_version":1}'; exec sleep 5"#;
        #[cfg(windows)]
        let script = r#"echo '{"type":"ready","protocol_version":1}'; Start-Sleep -Seconds 5"#;
        let mut client = fake(script).unwrap();
        let cancel = client.cancellation_handle();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let start = Instant::now();
        assert!(matches!(
            client.transcribe("blocked", AudioInput::pcm(&vec![0.1; 1_000_000]), "mic", 0),
            Err(ClientError::Timeout)
        ));
        thread.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(client.writer.is_none());
        assert!(client.child.try_wait().unwrap().is_some());
    }
    #[test]
    fn progress_cannot_extend_request_deadline() {
        let _serial = serial();
        #[cfg(unix)]
        let script = r#"echo '{"type":"ready","protocol_version":1}'; read -r request; while true; do echo '{"type":"progress","id":"p","fraction":0.5}'; sleep 0.02; done"#;
        #[cfg(windows)]
        let script = r#"echo '{"type":"ready","protocol_version":1}'; read -r request; while ($true) { echo '{"type":"progress","id":"p","fraction":0.5}'; Start-Sleep -Milliseconds 20 }"#;
        let mut client = fake(script).unwrap();
        client.timeout = Duration::from_millis(200);
        let start = Instant::now();
        assert!(matches!(
            client.transcribe("p", AudioInput::pcm(&[]), "mic", 0),
            Err(ClientError::Timeout)
        ));
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(client.child.try_wait().unwrap().is_some());
    }
}
