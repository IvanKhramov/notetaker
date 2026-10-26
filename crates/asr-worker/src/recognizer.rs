use crate::models::{self, ModelManifest};
use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::{AudioInput, Provider, Segment};
use sherpa_onnx::{
    LinearResampler, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use std::path::Path;

const SAMPLE_RATE: u64 = 16_000;

pub struct Recognizer {
    asr: OfflineRecognizer,
    vad: VoiceActivityDetector,
}

pub fn provider_name(provider: Provider) -> Result<&'static str> {
    match provider {
        // Auto deliberately favors portable CPU; explicit CUDA opts into GPU.
        Provider::Cpu | Provider::Auto => Ok("cpu"),
        Provider::Cuda => {
            ensure!(
                cfg!(feature = "cuda"),
                "CUDA requires the cuda Cargo feature and a CUDA-enabled sherpa native library"
            );
            Ok("cuda")
        }
    }
}

fn native_path(path: &Path) -> Result<String> {
    let path = path.to_str().context("sherpa requires UTF-8 model paths")?;
    ensure!(!path.contains('\0'), "NUL in model path");
    Ok(path.into())
}

impl Recognizer {
    pub fn load(id: &str, provider: Provider) -> Result<Self> {
        let provider = provider_name(provider)?;
        let model = models::manifest(id)?;
        ensure!(
            id != models::SILERO.id,
            "silero-vad is a detector, not an ASR model"
        );
        let root = models::models_dir()?
            .canonicalize()
            .context("model directory missing; run notetaker-asr download <model_id>")?;
        let directory = models::installed(&root, model)?;
        let vad_directory = models::installed(&root, &models::SILERO)?;
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer = transducer_config(&directory, model)?;
        config.model_config.tokens = Some(native_path(&directory.join("tokens.txt"))?);
        config.model_config.num_threads = 2;
        config.model_config.provider = Some(provider.into());
        config.model_config.model_type = Some("nemo_transducer".into());
        config.decoding_method = Some("greedy_search".into());
        let asr = OfflineRecognizer::create(&config)
            .context("cannot create recognizer; see stderr for sherpa diagnostics")?;
        let vad_config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(native_path(&vad_directory.join("silero_vad.onnx"))?),
                threshold: 0.5,
                min_silence_duration: 0.4,
                min_speech_duration: 0.15,
                window_size: 512,
                max_speech_duration: 20.0,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        let vad =
            VoiceActivityDetector::create(&vad_config, 30.0).context("cannot create Silero VAD")?;
        Ok(Self { asr, vad })
    }

    pub fn transcribe(
        &self,
        input: AudioInput,
        offset_ms: u64,
        mut progress: impl FnMut(f32) -> Result<()>,
    ) -> Result<Vec<Segment>> {
        let samples = match input {
            AudioInput::WavPath(path) => read_wav(&path)?,
            AudioInput::Pcm {
                samples_f32_16k_mono,
            } => AudioInput::decode_pcm(&samples_f32_16k_mono).map_err(anyhow::Error::msg)?,
        };
        let duration_ms = samples.len() as u64 * 1000 / SAMPLE_RATE;
        ensure!(
            offset_ms.checked_add(duration_ms).is_some(),
            "timestamp overflow"
        );
        self.vad.reset();
        let mut segments = Vec::new();
        progress(0.0)?;
        for (index, chunk) in samples.chunks(512).enumerate() {
            self.vad.accept_waveform(chunk);
            self.drain(&mut segments, offset_ms, samples.len())?;
            // Progress also advances through silence; at most one event per second.
            if index % 32 == 31 {
                progress(((index + 1) * 512).min(samples.len()) as f32 / samples.len() as f32)?;
            }
        }
        self.vad.flush();
        self.drain(&mut segments, offset_ms, samples.len())?;
        progress(1.0)?;
        Ok(segments)
    }

    fn drain(&self, segments: &mut Vec<Segment>, offset_ms: u64, input_len: usize) -> Result<()> {
        while let Some(speech) = self.vad.front() {
            let start = usize::try_from(speech.start()).context("negative VAD start")?;
            let end = (start + speech.samples().len()).min(input_len);
            if start < end {
                let stream = self.asr.create_stream();
                stream.accept_waveform(SAMPLE_RATE as i32, &speech.samples()[..end - start]);
                self.asr.decode(&stream);
                let result = stream
                    .get_result()
                    .context("recognizer returned no result")?;
                let text = result.text.trim();
                if !text.is_empty() {
                    segments.push(Segment {
                        start_ms: offset_ms + start as u64 * 1000 / SAMPLE_RATE,
                        end_ms: offset_ms + end as u64 * 1000 / SAMPLE_RATE,
                        text: text.into(),
                    });
                }
            }
            self.vad.pop();
        }
        Ok(())
    }
}

fn transducer_config(
    directory: &Path,
    model: &ModelManifest,
) -> Result<OfflineTransducerModelConfig> {
    Ok(OfflineTransducerModelConfig {
        encoder: Some(native_path(&directory.join(model.files[0]))?),
        decoder: Some(native_path(&directory.join(model.files[1]))?),
        joiner: Some(native_path(&directory.join(model.files[2]))?),
    })
}

/// WAV input is mono PCM/float; convert to the fixed 16 kHz VAD/ASR timebase.
fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("read WAV {}", path.display()))?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1 && (8000..=192000).contains(&spec.sample_rate),
        "expected mono WAV at 8–192 kHz, got {} Hz / {} channels",
        spec.sample_rate,
        spec.channels
    );
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            ensure!(
                (1..=32).contains(&spec.bits_per_sample),
                "unsupported WAV bit depth"
            );
            let scale = 2f32.powi(i32::from(spec.bits_per_sample) - 1);
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()?
        }
    };
    ensure!(
        samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
        "WAV samples must be finite and normalized"
    );
    if spec.sample_rate != SAMPLE_RATE as u32 && !samples.is_empty() {
        ensure!(
            samples.len() <= i32::MAX as usize,
            "WAV too large for resampling"
        );
        let resampler = LinearResampler::create(spec.sample_rate as i32, SAMPLE_RATE as i32)
            .context("cannot create WAV resampler")?;
        return Ok(resampler.resample(&samples, true));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_defaults() {
        assert_eq!(provider_name(Provider::Auto).unwrap(), "cpu");
        assert_eq!(provider_name(Provider::Cpu).unwrap(), "cpu");
        assert_eq!(
            provider_name(Provider::Cuda).is_ok(),
            cfg!(feature = "cuda")
        );
    }
    #[test]
    fn wav_resampling_preserves_duration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("24k.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 24000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..2400 {
            writer.write_sample(16384i16).unwrap();
        }
        writer.finalize().unwrap();
        let samples = read_wav(&path).unwrap();
        assert!((1599..=1601).contains(&samples.len()));
        assert!((samples[800] - 0.5).abs() < 0.01);
    }

    #[test]
    fn wav_formats() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for sample in [0i16, 16384, -32768] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        assert_eq!(read_wav(&path).unwrap(), [0.0, 0.5, -1.0]);
        let writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 2,
                ..spec
            },
        )
        .unwrap();
        writer.finalize().unwrap();
        assert!(read_wav(&path).is_err());
    }
}
