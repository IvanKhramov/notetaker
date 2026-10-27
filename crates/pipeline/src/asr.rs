use std::{path::PathBuf, process::Command, time::Duration};

use anyhow::{Context, Result};
use notetaker_asr_proto::{AsrClient, AudioInput, Provider, Segment};
use notetaker_capture::Channel;

/// `start` must dispose of any old worker before starting its replacement.
/// Results already include `offset_ms`, just like the wire protocol.
pub trait Asr: Send + 'static {
    fn start(&mut self) -> Result<()>;
    fn set_timeout(&mut self, _timeout: Duration) {}
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
            provider: Provider::Cpu,
            timeout: Duration::from_secs(300),
            vocabulary: notetaker_core::Vocabulary::default(),
        })
    }
}

pub struct WorkerAsr {
    config: AsrConfig,
    client: Option<AsrClient>,
    request: u64,
}

impl WorkerAsr {
    pub fn new(config: AsrConfig) -> Self {
        Self {
            config,
            client: None,
            request: 0,
        }
    }
}

impl Asr for WorkerAsr {
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
        let mut client =
            AsrClient::spawn_command(Command::new(&self.config.binary), self.config.timeout)
                .with_context(|| format!("launch {}", self.config.binary.display()))?;
        client.load_model_with_hotwords(
            &self.config.model,
            self.config.provider,
            &self.config.vocabulary.hotwords(),
        )?;
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
