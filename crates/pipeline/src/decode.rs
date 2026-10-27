//! Streaming media decoding. PCM is staged on disk for the final VAD pass.
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail, ensure};
use notetaker_capture::Normalizer;
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS, DecoderOptions},
    errors::Error,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

pub(crate) fn decode(
    path: &Path,
    wav: &Path,
    mut progress: impl FnMut(f32) -> Result<()>,
) -> Result<u64> {
    let native = decode_native(path, wav, &mut progress);
    match native {
        Ok(count) => Ok(count),
        Err(error) => {
            // A failed native attempt never leaves partial audio in the result.
            progress(0.0)?;
            decode_ffmpeg(path, wav, &mut progress)
                .with_context(|| format!("Rust-декодер: {error:#}"))
        }
    }
}

struct Output<F> {
    writer: hound::WavWriter<std::io::BufWriter<File>>,
    count: u64,
    progress: F,
}
impl<F: FnMut(f32) -> Result<()>> Output<F> {
    fn new(path: &Path, progress: F) -> Result<Self> {
        Ok(Self {
            writer: hound::WavWriter::create(
                path,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16000,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )?,
            count: 0,
            progress,
        })
    }
    fn write(&mut self, samples: &[f32], fraction: f32) -> Result<()> {
        (self.progress)(fraction)?;
        for &sample in samples {
            ensure!(sample.is_finite(), "Некорректные аудиосэмплы");
            self.writer.write_sample(sample.clamp(-1.0, 1.0))?;
        }
        self.count += samples.len() as u64;
        Ok(())
    }
    fn finish(self) -> Result<u64> {
        self.writer.finalize()?;
        ensure!(self.count > 0, "В файле нет аудио");
        Ok(self.count)
    }
}

fn decode_native(
    path: &Path,
    wav: &Path,
    progress: &mut dyn FnMut(f32) -> Result<()>,
) -> Result<u64> {
    let mut file = File::open(path)?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;
    if &magic == b"OggS" {
        let mut ogg = ogg::PacketReader::new(file);
        let first = ogg.read_packet()?.context("Пустой Ogg")?;
        if first.data.starts_with(b"OpusHead") {
            return decode_ogg_opus(ogg, first, wav, progress);
        }
        file = File::open(path)?;
    }
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )?
        .format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("Не найдена аудиодорожка")?;
    let id = track.id;
    let params = track.codec_params.clone();
    let opus = params.codec == CODEC_TYPE_OPUS;
    let mut decoder = if opus {
        None
    } else {
        Some(symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?)
    };
    let (channels, mut delay, gain) = if opus {
        let (channels, skip, gain) =
            opus_header(params.extra_data.as_deref().context("Нет OpusHead")?)?;
        (
            channels,
            params.delay.map_or(skip, |delay| delay as usize),
            gain,
        )
    } else {
        (0, 0, 1.0)
    };
    let mut opus_decoder = if opus {
        Some(opus_rs::OpusDecoder::new(48000, channels).map_err(anyhow::Error::msg)?)
    } else {
        None
    };
    // Some containers supply channel layout only after decoding the first packet.
    let mut current_spec = if opus { Some((48000, channels)) } else { None };
    let mut normalizer = if opus {
        Some(Normalizer::new(48000, channels)?)
    } else {
        None
    };
    let mut output = Output::new(wav, progress)?;
    let mut opus_buffer = vec![0.0; 5760 * channels];
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(error) => return Err(error.into()),
        };
        if packet.track_id() != id {
            continue;
        }
        let fraction = params
            .n_frames
            .filter(|n| *n > 0)
            .map_or(0.0, |n| (packet.ts() as f32 / n as f32).min(1.0));
        let mono = if let Some(decoder) = &mut opus_decoder {
            let frames = decoder
                .decode(&packet.data, 5760, &mut opus_buffer)
                .map_err(anyhow::Error::msg)?;
            let start = delay.max(packet.trim_start() as usize).min(frames);
            delay = delay.saturating_sub(frames);
            let end = frames.saturating_sub(packet.trim_end() as usize).max(start);
            for sample in &mut opus_buffer[start * channels..end * channels] {
                *sample *= gain;
            }
            normalizer
                .as_mut()
                .unwrap()
                .push(&opus_buffer[start * channels..end * channels])?
        } else {
            let decoded = decoder.as_mut().unwrap().decode(&packet)?;
            let spec = (decoded.spec().rate, decoded.spec().channels.count());
            if let Some(previous) = current_spec {
                ensure!(previous == spec, "Смена параметров аудио требует ffmpeg");
            } else {
                current_spec = Some(spec);
                normalizer = Some(Normalizer::new(spec.0, spec.1)?);
            }
            let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
            buffer.copy_interleaved_ref(decoded);
            normalizer.as_mut().unwrap().push(buffer.samples())?
        };
        output.write(&mono, fraction)?;
    }
    output.write(&normalizer.context("В файле нет аудио")?.finish()?, 1.0)?;
    output.finish()
}

fn opus_header(header: &[u8]) -> Result<(usize, usize, f32)> {
    ensure!(
        header.len() >= 19 && header.starts_with(b"OpusHead") && header[8] <= 15 && header[18] == 0,
        "Неподдерживаемый OpusHead: требуется ffmpeg"
    );
    let channels = header[9] as usize;
    ensure!(
        (1..=2).contains(&channels),
        "Неподдерживаемое число каналов Opus"
    );
    let skip = u16::from_le_bytes([header[10], header[11]]) as usize;
    let gain = i16::from_le_bytes([header[16], header[17]]) as f32 / 256.0;
    Ok((channels, skip, 10f32.powf(gain / 20.0)))
}

fn decode_ogg_opus(
    mut reader: ogg::PacketReader<File>,
    first: ogg::Packet,
    wav: &Path,
    progress: &mut dyn FnMut(f32) -> Result<()>,
) -> Result<u64> {
    let serial = first.stream_serial();
    let (channels, mut skip, gain) = opus_header(&first.data)?;
    ensure!(
        reader
            .read_packet()?
            .is_some_and(|p| p.data.starts_with(b"OpusTags") && p.stream_serial() == serial),
        "Нет OpusTags"
    );
    let mut decoder = opus_rs::OpusDecoder::new(48000, channels).map_err(anyhow::Error::msg)?;
    let mut normalizer = Normalizer::new(48000, channels)?;
    let mut output = Output::new(wav, progress)?;
    let mut buffer = vec![0.0; 5760 * channels];
    let mut position = 0u64;
    let mut eos = false;
    while let Some(packet) = reader.read_packet()? {
        ensure!(
            packet.stream_serial() == serial && !eos,
            "Составной Ogg требует ffmpeg"
        );
        let frames = decoder
            .decode(&packet.data, 5760, &mut buffer)
            .map_err(anyhow::Error::msg)?;
        let start = skip.min(frames);
        skip = skip.saturating_sub(frames);
        let end = if packet.last_in_stream() {
            eos = true;
            packet
                .absgp_page()
                .saturating_sub(position)
                .min(frames as u64) as usize
        } else {
            frames
        }
        .max(start);
        position += frames as u64;
        for sample in &mut buffer[start * channels..end * channels] {
            *sample *= gain;
        }
        output.write(
            &normalizer.push(&buffer[start * channels..end * channels])?,
            0.0,
        )?;
    }
    ensure!(eos, "Незавершённый Ogg Opus");
    output.write(&normalizer.finish()?, 1.0)?;
    output.finish()
}

fn decode_ffmpeg(
    path: &Path,
    wav: &Path,
    progress: &mut dyn FnMut(f32) -> Result<()>,
) -> Result<u64> {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    // stderr goes to a temporary file: no pipe deadlock or unbounded log in RAM.
    let stderr = tempfile::tempfile()?;
    command.stderr(stderr.try_clone()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .context("Не удалось декодировать файл. Установите ffmpeg и добавьте его в PATH")?;
    let result = (|| -> Result<u64> {
        let mut stdout = child.stdout.take().context("Нет stdout ffmpeg")?;
        let mut output = Output::new(wav, progress)?;
        let mut block = [0; 16384];
        let mut pending = Vec::new();
        loop {
            let count = stdout.read(&mut block)?;
            if count == 0 {
                break;
            }
            pending.extend_from_slice(&block[..count]);
            let count = pending.len() / 4 * 4;
            let samples: Vec<_> = pending[..count]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            output.write(&samples, 0.0)?;
            pending.drain(..count);
        }
        ensure!(pending.is_empty(), "Неполный PCM от ffmpeg");
        if !child.wait()?.success() {
            let mut stderr = stderr;
            stderr.seek(SeekFrom::Start(0))?;
            let mut message = String::new();
            stderr.take(4096).read_to_string(&mut message)?;
            bail!("ffmpeg: {}", message.trim());
        }
        output.finish()
    })();
    // Kill/reap on output, cancellation and I/O failures as well as success.
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_formats_need_no_external_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for extension in ["mp3", "m4a", "ogg", "flac", "mp4", "webm", "mkv"] {
            let count = decode_native(
                &fixtures.join(format!("tone.{extension}")),
                &dir.path().join("out.wav"),
                &mut |_| Ok(()),
            )
            .unwrap_or_else(|error| panic!("{extension}: {error:#}"));
            // AAC padding and WebM discard padding vary by container support.
            assert!((3700..=4500).contains(&count), "{extension}: {count}");
        }
    }

    #[test]
    fn native_stereo_wav_is_streamed_resampled_and_downmixed() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.wav");
        let output = dir.path().join("output.wav");
        let mut writer = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..48017 {
            writer.write_sample(16384_i16).unwrap();
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();
        let count = decode_native(&input, &output, &mut |_| Ok(())).unwrap();
        assert_eq!(count, 48017 * 16000 / 48000);
        let samples: Vec<_> = hound::WavReader::open(output)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect();
        assert!((samples[800] - 0.25).abs() < 0.001);
    }
    #[test]
    fn native_opus_preserves_preskip_and_final_granule() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.opus");
        let output = dir.path().join("output.wav");
        let mut writer = notetaker_capture::TrackWriter::create(&input).unwrap();
        writer.write_samples(&vec![0.1; 16017]).unwrap();
        writer.finish().unwrap();
        assert_eq!(
            decode_native(&input, &output, &mut |_| Ok(())).unwrap(),
            16017
        );
    }
}
