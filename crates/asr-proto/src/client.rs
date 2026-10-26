use crate::{
    AudioInput, PROTOCOL_VERSION, Provider, Request, Response, Segment, read_message, write_message,
};
use std::{
    io::{self, BufReader},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
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

/// Owns and reaps a worker. Dropping the client also terminates a stuck worker.
/// Responses are bounded by `timeout`; stderr is inherited, never parsed as JSON.
/// No ML dependency is linked into the caller.
pub struct AsrClient {
    child: Child,
    stdin: Option<ChildStdin>,
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
        let stdin = child.stdin.take();
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
            stdin,
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

    pub fn send(&mut self, request: &Request) -> Result<(), ClientError> {
        if let Some(status) = self.child.try_wait()? {
            return Err(ClientError::Exited(status.to_string()));
        }
        write_message(
            self.stdin
                .as_mut()
                .ok_or_else(|| ClientError::Protocol("stdin closed".into()))?,
            request,
        )?;
        Ok(())
    }

    pub fn receive(&mut self) -> Result<Response, ClientError> {
        match self.responses.recv_timeout(self.timeout) {
            Ok(Ok(Some(response))) => Ok(response),
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = self
                    .child
                    .try_wait()?
                    .map_or_else(|| "stdout closed".into(), |s| s.to_string());
                Err(ClientError::Exited(status))
            }
            Ok(Err(e)) => Err(e.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ClientError::Timeout),
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
        self.stdin.take();
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
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
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
}
