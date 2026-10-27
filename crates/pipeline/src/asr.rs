use std::{path::PathBuf, process::Command, time::Duration};

use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::{AsrClient, AudioInput, Provider, Segment};
use notetaker_capture::Channel;

/// `start` must dispose of any old worker before starting its replacement.
/// Results already include `offset_ms`, just like the wire protocol.
pub trait Asr: Send + 'static {
    fn start(&mut self) -> Result<()>;
    fn set_timeout(&mut self, _timeout: Duration) {}
    fn take_notice(&mut self) -> Option<String> {
        None
    }
    fn cancellation_handle(&self) -> Option<notetaker_asr_proto::CancellationHandle> {
        None
    }
    fn diarize(
        &mut self,
        _wav: &std::path::Path,
        _speakers: Option<u32>,
        _embeddings: bool,
    ) -> Result<notetaker_asr_proto::Diarization> {
        anyhow::bail!("диаризация недоступна")
    }
    fn embed(
        &mut self,
        _wav: &std::path::Path,
        _ranges: Vec<notetaker_asr_proto::TimeRange>,
    ) -> Result<Vec<f32>> {
        anyhow::bail!("извлечение голоса недоступно")
    }
    fn transcribe(
        &mut self,
        input: AudioInput,
        channel: Channel,
        offset_ms: u64,
    ) -> Result<Vec<Segment>>;
    fn transcribe_with_progress(
        &mut self,
        input: AudioInput,
        channel: Channel,
        offset_ms: u64,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<Segment>> {
        let result = self.transcribe(input, channel, offset_ms)?;
        progress(1.0);
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub struct AsrConfig {
    pub binary: PathBuf,
    pub model: String,
    pub provider: Provider,
    /// Zero selects min(physical cores, 8).
    pub num_threads: u32,
    pub timeout: Duration,
    pub vocabulary: notetaker_core::Vocabulary,
}

impl AsrConfig {
    /// NOTETAKER_ASR_BIN, a sibling of the executable (or its deps directory),
    /// then PATH. Models use the worker's NOTETAKER_MODELS_DIR/ProjectDirs policy.
    pub fn new(model: impl Into<String>) -> Result<Self> {
        let binary = if let Some(path) = std::env::var_os("NOTETAKER_ASR_BIN") {
            path.into()
        } else {
            let name = format!("notetaker-asr{}", std::env::consts::EXE_SUFFIX);
            let exe = notetaker_runtime::paths::executable()?;
            let sibling = exe.with_file_name(&name);
            let parent = exe.parent().and_then(|p| p.parent()).map(|p| p.join(&name));
            if sibling.is_file() {
                sibling
            } else if let Some(parent) = parent.filter(|p| p.is_file()) {
                parent
            } else {
                name.into()
            }
        };
        Ok(Self {
            binary,
            model: model.into(),
            provider: Provider::Auto,
            num_threads: 0,
            timeout: Duration::from_secs(300),
            vocabulary: notetaker_core::Vocabulary::default(),
        })
    }

    /// Optional addon workers never affect the portable CPU package. An explicit
    /// override is useful for testing a separately validated future GPU build.
    pub fn gpu_binary(&self) -> Option<PathBuf> {
        if !cfg!(windows) {
            return None;
        }
        if let Some(path) = std::env::var_os("NOTETAKER_ASR_GPU_BIN") {
            return Some(path.into());
        }
        let name = "notetaker-asr-gpu.exe";
        let sibling = self.binary.with_file_name(name);
        sibling.is_file().then_some(sibling)
    }

    /// The caller supplies process flags (CREATE_NO_WINDOW in the desktop app).
    /// Require a DirectML acknowledgement so a CPU worker cannot impersonate GPU.
    pub fn start_client(
        &self,
        command: impl FnMut(&std::path::Path) -> Command,
    ) -> Result<(AsrClient, Option<String>)> {
        self.start_client_with_gpu(self.gpu_binary(), command)
    }

    fn start_client_with_gpu(
        &self,
        gpu: Option<PathBuf>,
        mut command: impl FnMut(&std::path::Path) -> Command,
    ) -> Result<(AsrClient, Option<String>)> {
        let load = |path: &std::path::Path,
                    provider,
                    command: &mut dyn FnMut(&std::path::Path) -> Command|
         -> Result<AsrClient> {
            let mut client = AsrClient::spawn_command(command(path), self.timeout)
                .with_context(|| format!("launch {}", path.display()))?;
            let actual = client.load_model_with_options(
                &self.model,
                provider,
                &self.vocabulary.hotwords(),
                self.num_threads,
            )?;
            ensure!(
                actual == provider,
                "worker did not initialize requested provider {provider:?}"
            );
            Ok(client)
        };
        let mut notice = None;
        if matches!(self.provider, Provider::Auto | Provider::Directml) {
            if let Some(gpu) = gpu {
                match load(&gpu, Provider::Directml, &mut command) {
                    Ok(client) => return Ok((client, None)),
                    Err(error) if self.provider == Provider::Directml => return Err(error),
                    Err(_) => {
                        notice = Some("GPU недоступен. Распознавание продолжится на CPU.".into())
                    }
                }
            } else if self.provider == Provider::Directml {
                anyhow::bail!("GPU (DirectML): недоступно в этой сборке");
            }
        }
        let provider = if self.provider == Provider::Auto {
            Provider::Cpu
        } else {
            self.provider
        };
        Ok((load(&self.binary, provider, &mut command)?, notice))
    }
}

pub struct WorkerAsr {
    config: AsrConfig,
    client: Option<AsrClient>,
    request: u64,
    notice: Option<String>,
}

impl WorkerAsr {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            client: None,
            request: 0,
            notice: None,
        }
    }
}

impl Asr for WorkerAsr {
    fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }
    fn set_timeout(&mut self, timeout: Duration) {
        self.config.timeout = self.config.timeout.max(timeout);
        if let Some(client) = &mut self.client {
            client.set_timeout(self.config.timeout);
        }
    }
    fn cancellation_handle(&self) -> Option<notetaker_asr_proto::CancellationHandle> {
        self.client.as_ref().map(AsrClient::cancellation_handle)
    }
    fn start(&mut self) -> Result<()> {
        self.client.take(); // Reap the old process before spawning another.
        let (client, notice) = self.config.start_client(|path| Command::new(path))?;
        self.notice = notice;
        self.client = Some(client);
        Ok(())
    }

    fn diarize(
        &mut self,
        wav: &std::path::Path,
        speakers: Option<u32>,
        embeddings: bool,
    ) -> Result<notetaker_asr_proto::Diarization> {
        Ok(self
            .client
            .as_mut()
            .context("ASR is not started")?
            .diarize(wav, "system", speakers, embeddings)?)
    }
    fn embed(
        &mut self,
        wav: &std::path::Path,
        ranges: Vec<notetaker_asr_proto::TimeRange>,
    ) -> Result<Vec<f32>> {
        Ok(self
            .client
            .as_mut()
            .context("ASR is not started")?
            .embed(wav, ranges)?)
    }
    fn transcribe(
        &mut self,
        input: AudioInput,
        channel: Channel,
        offset_ms: u64,
    ) -> Result<Vec<Segment>> {
        self.transcribe_with_progress(input, channel, offset_ms, &mut |_| {})
    }
    fn transcribe_with_progress(
        &mut self,
        input: AudioInput,
        channel: Channel,
        offset_ms: u64,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<Segment>> {
        self.request += 1;
        let channel = match channel {
            Channel::Mic => "mic",
            Channel::System => "system",
        };
        let mut segments = self
            .client
            .as_mut()
            .context("ASR is not started")?
            .transcribe_with_progress(
                &self.request.to_string(),
                input,
                channel,
                offset_ms,
                progress,
            )?;
        for segment in &mut segments {
            segment.text = self.config.vocabulary.correct(&segment.text);
        }
        Ok(segments)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    fn worker(path: &Path, provider: &str) {
        std::fs::write(
            path,
            format!(
                r#"echo '{{"type":"ready","protocol_version":1}}'
read -r request
echo '{{"type":"model_loaded","model_id":"auto","provider":"{provider}"}}'
while read -r request; do :; done
"#
            ),
        )
        .unwrap();
    }

    fn shell(path: &Path) -> Command {
        let mut command = Command::new("sh");
        command.arg(path);
        command
    }

    #[test]
    fn auto_falls_back_after_gpu_load_failure_or_cpu_acknowledgement() {
        let directory = tempfile::tempdir().unwrap();
        let cpu = directory.path().join("cpu.sh");
        let gpu = directory.path().join("gpu.sh");
        worker(&cpu, "cpu");
        let mut config = AsrConfig::new("auto").unwrap();
        config.binary = cpu;
        config.timeout = Duration::from_secs(5);
        // Missing/unloadable addon, then a worker silently using CPU.
        for silently_cpu in [false, true] {
            if silently_cpu {
                worker(&gpu, "cpu");
            }
            let (client, notice) = config
                .start_client_with_gpu(Some(gpu.clone()), shell)
                .unwrap();
            assert!(notice.unwrap().contains("CPU"));
            drop(client);
        }
        config.provider = Provider::Directml;
        assert!(config.start_client_with_gpu(Some(gpu), shell).is_err());
        assert!(config.start_client_with_gpu(None, shell).is_err());
    }

    #[test]
    fn cpu_selection_skips_gpu_and_auto_uses_acknowledged_gpu() {
        let directory = tempfile::tempdir().unwrap();
        let gpu = directory.path().join("gpu.sh");
        worker(&gpu, "directml");
        let mut config = AsrConfig::new("auto").unwrap();
        config.binary = directory.path().join("missing-cpu.sh");
        let (client, notice) = config
            .start_client_with_gpu(Some(gpu.clone()), shell)
            .unwrap();
        assert!(notice.is_none());
        drop(client);
        config.provider = Provider::Cpu;
        assert!(config.start_client_with_gpu(Some(gpu), shell).is_err());
    }
}
