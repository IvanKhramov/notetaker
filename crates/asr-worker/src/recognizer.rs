use crate::models::{self, ModelManifest};
use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::{AudioInput, Provider, Segment};
use sherpa_onnx::{
    LinearResampler, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use std::{io::Write, path::Path};

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
    pub fn load(id: &str, provider: Provider, hotwords: &[String]) -> Result<Self> {
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
        // NeMo RNNT and TDT support contextual biasing in sherpa 2.0.0.
        // Derive equal-score BPE pieces as in upstream's Parakeet example.
        let _hotword_files = configure_hotwords(&mut config, &directory, hotwords)?;
        let asr = OfflineRecognizer::create(&config)
            .context("cannot create recognizer; see stderr for sherpa diagnostics")?;
        let vad_config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(native_path(&vad_directory.join("silero_vad.onnx"))?),
                threshold: 0.5,
                min_silence_duration: 0.4,
                min_speech_duration: 0.15,
                window_size: 512,
                max_speech_duration: 29.0,
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
        self.vad.reset();
        let mut segments = Vec::new();
        let mut pending = Vec::new();
        let mut processed = 0usize;
        progress(0.0)?;
        let mut feed = |samples: &[f32], total: usize| -> Result<()> {
            ensure!(
                offset_ms.checked_add(total as u64 / 16).is_some(),
                "timestamp overflow"
            );
            pending.extend_from_slice(samples);
            let count = pending.len() / 512 * 512;
            for chunk in pending[..count].as_chunks::<512>().0 {
                self.vad.accept_waveform(chunk);
                processed += chunk.len();
                self.drain(&mut segments, offset_ms, processed)?;
                if processed.is_multiple_of(16384) {
                    progress((processed as f32 / total.max(1) as f32).min(1.0))?;
                }
            }
            pending.drain(..count);
            Ok(())
        };
        match input {
            AudioInput::WavPath(path) => stream_wav(&path, &mut feed)?,
            AudioInput::Pcm {
                samples_f32_16k_mono,
            } => {
                let samples =
                    AudioInput::decode_pcm(&samples_f32_16k_mono).map_err(anyhow::Error::msg)?;
                feed(&samples, samples.len())?;
            }
        }
        if !pending.is_empty() {
            self.vad.accept_waveform(&pending);
            processed += pending.len();
        }
        self.vad.flush();
        self.drain(&mut segments, offset_ms, processed)?;
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

/// Bounded WAV decoding; VAD owns at most one speech chunk, never a full track.
fn stream_wav(path: &Path, mut emit: impl FnMut(&[f32], usize) -> Result<()>) -> Result<()> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("read WAV {}", path.display()))?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1 && (8000..=192000).contains(&spec.sample_rate),
        "expected mono WAV at 8-192 kHz"
    );
    ensure!(
        (1..=32).contains(&spec.bits_per_sample),
        "unsupported WAV bit depth"
    );
    let total = reader.duration() as usize * SAMPLE_RATE as usize / spec.sample_rate as usize;
    let resampler = if spec.sample_rate != SAMPLE_RATE as u32 {
        Some(
            LinearResampler::create(spec.sample_rate as i32, SAMPLE_RATE as i32)
                .context("cannot create WAV resampler")?,
        )
    } else {
        None
    };
    let scale = 2f32.powi(i32::from(spec.bits_per_sample) - 1);
    let mut samples: Box<dyn Iterator<Item = Result<f32, hound::Error>> + '_> =
        match spec.sample_format {
            hound::SampleFormat::Float => Box::new(reader.samples::<f32>()),
            hound::SampleFormat::Int => {
                Box::new(reader.samples::<i32>().map(|s| s.map(|v| v as f32 / scale)))
            }
        };
    loop {
        let block: Vec<_> = samples.by_ref().take(8192).collect::<Result<_, _>>()?;
        if block.is_empty() {
            break;
        }
        ensure!(
            block.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
            "WAV samples must be finite and normalized"
        );
        if let Some(resampler) = &resampler {
            emit(&resampler.resample(&block, false), total)?;
        } else {
            emit(&block, total)?;
        }
    }
    if let Some(resampler) = &resampler {
        emit(&resampler.resample(&[], true), total)?;
    }
    Ok(())
}

#[cfg(test)]
fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut samples = Vec::new();
    stream_wav(path, |block, _| {
        samples.extend_from_slice(block);
        Ok(())
    })?;
    Ok(samples)
}

fn configure_hotwords(
    config: &mut OfflineRecognizerConfig,
    directory: &Path,
    hotwords: &[String],
) -> Result<Option<tempfile::TempDir>> {
    if hotwords.is_empty() {
        return Ok(None);
    }
    ensure!(
        hotwords.iter().all(|s| !s.trim().is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || c == ' ' || c == '-')),
        "invalid hotword"
    );
    let temporary = tempfile::tempdir()?;
    let bpe = temporary.path().join("bpe.vocab");
    let mut vocab = std::fs::File::create(&bpe)?;
    for line in std::fs::read_to_string(directory.join("tokens.txt"))?.lines() {
        if let Some((piece, _)) = line.rsplit_once(' ') {
            writeln!(vocab, "{piece}\t-1.0")?;
        }
    }
    drop(vocab);
    let words = temporary.path().join("hotwords.txt");
    // Include both cases: the English/Russian models have different casing.
    let words_text = hotwords
        .iter()
        .flat_map(|s| [s.clone(), s.to_lowercase(), s.to_uppercase()])
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&words, words_text)?;
    config.model_config.modeling_unit = Some("bpe".into());
    config.model_config.bpe_vocab = Some(native_path(&bpe)?);
    config.decoding_method = Some("modified_beam_search".into());
    config.max_active_paths = 4;
    config.hotwords_score = 1.5;
    config.hotwords_file = Some(native_path(&words)?);
    Ok(Some(temporary))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hotwords_use_beam_search_and_temporary_bpe_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("tokens.txt"),
            "<unk> 0\n▁ 1\nwerf 2\n<blk> 3\n",
        )
        .unwrap();
        let mut config = OfflineRecognizerConfig::default();
        assert!(
            configure_hotwords(&mut config, directory.path(), &[])
                .unwrap()
                .is_none()
        );
        let files = configure_hotwords(&mut config, directory.path(), &["werf".into()])
            .unwrap()
            .unwrap();
        assert_eq!(
            config.decoding_method.as_deref(),
            Some("modified_beam_search")
        );
        assert_eq!(config.model_config.modeling_unit.as_deref(), Some("bpe"));
        assert_eq!(config.hotwords_score, 1.5);
        assert!(
            std::fs::read_to_string(files.path().join("bpe.vocab"))
                .unwrap()
                .contains("werf\t-1.0")
        );
        let words = std::fs::read_to_string(files.path().join("hotwords.txt")).unwrap();
        assert_eq!(words.lines().count(), 2);
        assert!(configure_hotwords(&mut config, directory.path(), &["bad:999".into()]).is_err());
    }

    #[test]
    fn wav_stream_bounds_memory_and_preserves_all_samples() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("long.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for _ in 0..160017 {
            writer.write_sample(0.25_f32).unwrap();
        }
        writer.finalize().unwrap();
        let mut count = 0;
        let mut blocks = 0;
        stream_wav(&path, |samples, total| {
            assert_eq!(total, 160017);
            assert!(samples.len() <= 8192);
            assert!(samples.iter().all(|s| *s == 0.25));
            count += samples.len();
            blocks += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 160017);
        assert!(blocks > 1);
    }

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
