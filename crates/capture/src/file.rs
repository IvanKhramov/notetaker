use std::path::{Path, PathBuf};
use std::sync::{atomic::Ordering, mpsc::SyncSender};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use hound::{SampleFormat, WavReader};

use crate::source::send;
use crate::{
    AudioFrame, AudioSource, CaptureMessage, Channel, FRAME_SAMPLES, Normalizer, SourceTask,
    sample_offset,
};

#[derive(Clone, Copy, Debug)]
pub enum PlaybackPace {
    Realtime,
    Accelerated,
}

pub struct FileSource {
    path: PathBuf,
    channel: Channel,
    pace: PlaybackPace,
    task: SourceTask,
}

impl FileSource {
    pub fn new(path: impl AsRef<Path>, channel: Channel, pace: PlaybackPace) -> Self {
        Self {
            path: path.as_ref().to_owned(),
            channel,
            pace,
            task: SourceTask::default(),
        }
    }
}

impl AudioSource for FileSource {
    fn start(&mut self, sender: SyncSender<CaptureMessage>) -> Result<()> {
        let reader = WavReader::open(&self.path)?;
        let spec = reader.spec();
        ensure!(
            spec.sample_format == SampleFormat::Int || spec.bits_per_sample == 32,
            "only float32 WAV supported"
        );
        let mut normalizer = Normalizer::new(spec.sample_rate, usize::from(spec.channels))?;
        let channel = self.channel;
        let pace = self.pace;
        self.task.start(channel, sender, move |stop, sender| {
            let clock = Instant::now();
            let mut position = 0;
            let chunk_size = spec.channels as usize * 1024;
            let mut input = Vec::with_capacity(chunk_size);
            let mut emit = |samples: Vec<f32>| -> bool {
                for chunk in samples.chunks(FRAME_SAMPLES) {
                    let captured_at = sample_offset(position);
                    if matches!(pace, PlaybackPace::Realtime) {
                        while clock.elapsed() < captured_at {
                            if stop.load(Ordering::Relaxed) {
                                return false;
                            }
                            std::thread::sleep(
                                captured_at
                                    .saturating_sub(clock.elapsed())
                                    .min(Duration::from_millis(5)),
                            );
                        }
                    }
                    if !send(
                        sender,
                        stop,
                        CaptureMessage::Frame(AudioFrame {
                            channel,
                            samples: chunk.to_vec(),
                            captured_at,
                        }),
                    ) {
                        return false;
                    }
                    position += chunk.len() as u64;
                }
                true
            };
            let samples: Box<dyn Iterator<Item = Result<f32>> + '_> = match spec.sample_format {
                SampleFormat::Float => Box::new(reader.into_samples::<f32>().map(|s| Ok(s?))),
                SampleFormat::Int => {
                    let scale = 2_f32.powi(i32::from(spec.bits_per_sample) - 1);
                    Box::new(
                        reader
                            .into_samples::<i32>()
                            .map(move |s| Ok(s? as f32 / scale)),
                    )
                }
            };
            for sample in samples {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                input.push(sample?);
                if input.len() == chunk_size {
                    if !emit(normalizer.push(&input)?) {
                        return Ok(());
                    }
                    input.clear();
                }
            }
            if !emit(normalizer.push(&input)?) {
                return Ok(());
            }
            emit(normalizer.finish()?);
            Ok(())
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.task.stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureEvent;
    use std::sync::mpsc;

    #[test]
    fn wav_source_normalizes_and_timestamps_in_both_paces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..2400 {
            wav.write_sample(8192_i16).unwrap();
            wav.write_sample(16384_i16).unwrap();
        }
        wav.finalize().unwrap();
        for pace in [PlaybackPace::Realtime, PlaybackPace::Accelerated] {
            let mut source = FileSource::new(&path, Channel::System, pace);
            let (tx, rx) = mpsc::sync_channel(4);
            source.start(tx).unwrap();
            let mut count = 0;
            loop {
                match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                    CaptureMessage::Frame(frame) => {
                        assert_eq!(frame.channel, Channel::System);
                        assert_eq!(frame.captured_at, sample_offset(count));
                        assert!(frame.samples.iter().all(|s| s.is_finite()));
                        count += frame.samples.len() as u64;
                    }
                    CaptureMessage::Event(CaptureEvent::Stopped { .. }) => break,
                    CaptureMessage::Event(event) => panic!("unexpected {event:?}"),
                }
            }
            source.stop().unwrap();
            assert_eq!(count, 800);
        }
    }

    #[test]
    fn stop_cancels_a_full_channel_and_source_can_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.wav");
        let mut wav = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 32,
                sample_format: SampleFormat::Float,
            },
        )
        .unwrap();
        for _ in 0..16000 {
            wav.write_sample(0.0_f32).unwrap();
        }
        wav.finalize().unwrap();
        let mut source = FileSource::new(path, Channel::Mic, PlaybackPace::Accelerated);
        for _ in 0..2 {
            let (tx, rx) = mpsc::sync_channel(1);
            source.start(tx).unwrap();
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
            // Leave the next frame queued so the producer hits backpressure.
            std::thread::sleep(Duration::from_millis(10));
            source.stop().unwrap();
        }
    }
}
