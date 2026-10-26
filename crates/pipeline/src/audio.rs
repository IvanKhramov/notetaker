use std::{fs::File, io::BufWriter, path::Path, time::Duration};

use anyhow::{Result, ensure};
use notetaker_capture::{
    AudioFrame, Channel, FRAME_SAMPLES, SAMPLE_RATE, TrackWriter, sample_position,
};

#[derive(Clone, Debug)]
pub struct ChunkConfig {
    pub min: Duration,
    pub max: Duration,
    pub silence: Duration,
    /// Linear RMS amplitude; 0.01 is -40 dBFS.
    pub silence_level: f32,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            min: Duration::from_secs(15),
            max: Duration::from_secs(30),
            silence: Duration::from_millis(300),
            silence_level: 0.01,
        }
    }
}

impl ChunkConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.min >= Duration::from_millis(20) && self.max >= self.min,
            "chunk durations must satisfy 20 ms <= min <= max"
        );
        ensure!(
            self.max <= Duration::from_secs(30),
            "live chunks must be <= 30 seconds"
        );
        ensure!(
            !self.silence.is_zero() && self.silence <= self.min,
            "invalid silence duration"
        );
        ensure!(
            self.silence_level.is_finite() && (0.0..=1.0).contains(&self.silence_level),
            "invalid silence level"
        );
        Ok(())
    }
}

pub(crate) struct Chunk {
    pub channel: Channel,
    pub start: u64,
    pub samples: Vec<f32>,
}

pub(crate) struct Chunker {
    config: ChunkConfig,
    samples: Vec<f32>,
    start: u64,
    quiet: u64,
}

impl Chunker {
    pub fn new(config: ChunkConfig) -> Self {
        Self {
            config,
            samples: Vec::new(),
            start: 0,
            quiet: 0,
        }
    }

    // No overlap: every input sample belongs to exactly one request. Silence is
    // kept on the preceding chunk; the next starts at that chunk's exact end.
    pub fn push(&mut self, channel: Channel, samples: &[f32], mut emit: impl FnMut(Chunk)) {
        for block in samples.chunks(FRAME_SAMPLES) {
            let rms = (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt();
            self.quiet = if rms <= self.config.silence_level {
                self.quiet + block.len() as u64
            } else {
                0
            };
            self.samples.extend_from_slice(block);
            let len = self.samples.len() as u64;
            if len >= sample_position(self.config.max)
                || (len >= sample_position(self.config.min)
                    && self.quiet >= sample_position(self.config.silence))
            {
                self.flush(channel, &mut emit);
            }
        }
    }

    pub fn flush(&mut self, channel: Channel, mut emit: impl FnMut(Chunk)) {
        if self.samples.is_empty() {
            return;
        }
        let samples = std::mem::take(&mut self.samples);
        let start = self.start;
        self.start += samples.len() as u64;
        self.quiet = 0;
        emit(Chunk {
            channel,
            start,
            samples,
        });
    }

    pub fn clear(&mut self, position: u64) {
        self.samples.clear();
        self.quiet = 0;
        self.start = position;
    }
}

pub(crate) struct Track {
    opus: TrackWriter,
    wav: hound::WavWriter<BufWriter<File>>,
    pub chunker: Chunker,
    pub channel: Channel,
}

impl Track {
    pub fn create(opus: &Path, wav: &Path, channel: Channel, config: ChunkConfig) -> Result<Self> {
        Ok(Self {
            opus: TrackWriter::create(opus)?,
            wav: hound::WavWriter::create(
                wav,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: SAMPLE_RATE,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )?,
            chunker: Chunker::new(config),
            channel,
        })
    }

    pub fn position(&self) -> u64 {
        self.opus.samples_written()
    }

    pub fn frame(
        &mut self,
        frame: AudioFrame,
        live: bool,
        mut emit: impl FnMut(Chunk),
    ) -> Result<()> {
        ensure!(
            frame.samples.iter().all(|s| s.is_finite()),
            "non-finite audio sample"
        );
        let start = sample_position(frame.captured_at);
        self.pad_to(start, live, &mut emit)?;
        let overlap = self
            .position()
            .saturating_sub(start)
            .min(frame.samples.len() as u64) as usize;
        let samples: Vec<_> = frame.samples[overlap..]
            .iter()
            .map(|s| s.clamp(-1.0, 1.0))
            .collect();
        self.append(&samples, live, emit)
    }

    pub fn pad_to(&mut self, end: u64, live: bool, mut emit: impl FnMut(Chunk)) -> Result<()> {
        while self.position() < end {
            let count = (end - self.position()).min(FRAME_SAMPLES as u64) as usize;
            self.append(&[0.0; FRAME_SAMPLES][..count], live, &mut emit)?;
        }
        Ok(())
    }

    fn append(&mut self, samples: &[f32], live: bool, emit: impl FnMut(Chunk)) -> Result<()> {
        self.opus.write_samples(samples)?;
        for sample in samples {
            self.wav
                .write_sample((sample * 32768.0).clamp(-32768.0, 32767.0) as i16)?;
        }
        if live {
            self.chunker.push(self.channel, samples, emit);
        } else {
            self.chunker.clear(self.position());
        }
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        self.wav.finalize()?;
        self.opus.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_at_silence_or_max_without_overlap_or_missing_samples() {
        let config = ChunkConfig {
            min: Duration::from_millis(40),
            max: Duration::from_millis(100),
            silence: Duration::from_millis(20),
            silence_level: 0.01,
        };
        let samples: Vec<_> = [vec![0.5; 640], vec![0.0; 320], vec![0.25; 2000]].concat();
        let mut chunker = Chunker::new(config);
        let mut chunks = Vec::new();
        chunker.push(Channel::Mic, &samples, |c| chunks.push(c));
        chunker.flush(Channel::Mic, |c| chunks.push(c));
        assert_eq!(chunks[0].samples.len(), 960); // 40 ms speech + 20 ms quiet.
        assert_eq!(chunks[1].samples.len(), 1600); // Hard cap during speech.
        let mut end = 0;
        let mut restored = Vec::new();
        for chunk in chunks {
            assert_eq!(chunk.start, end);
            end += chunk.samples.len() as u64;
            restored.extend(chunk.samples);
        }
        assert_eq!(restored, samples);
    }
}

#[cfg(test)]
mod timeline_tests {
    use super::*;
    use notetaker_capture::sample_offset;

    #[test]
    fn wav_and_live_use_the_same_padding_overlap_trimming_and_clamping() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("track.wav");
        let mut track = Track::create(
            &dir.path().join("track.opus"),
            &wav,
            Channel::Mic,
            ChunkConfig::default(),
        )
        .unwrap();
        let mut chunks = Vec::new();
        for (start, samples) in [(320, vec![0.5; 320]), (480, vec![-2.0; 320])] {
            track
                .frame(
                    AudioFrame {
                        channel: Channel::Mic,
                        captured_at: sample_offset(start),
                        samples,
                    },
                    true,
                    |c| chunks.push(c),
                )
                .unwrap();
        }
        track.pad_to(960, true, |c| chunks.push(c)).unwrap();
        track.chunker.flush(Channel::Mic, |c| chunks.push(c));
        assert_eq!(track.position(), 960);
        track.finish().unwrap();
        let live: Vec<_> = chunks.into_iter().flat_map(|c| c.samples).collect();
        let expected = [
            vec![0.0; 320],
            vec![0.5; 320],
            vec![-1.0; 160],
            vec![0.0; 160],
        ]
        .concat();
        assert_eq!(live, expected);
        let stored: Vec<_> = hound::WavReader::open(wav)
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            stored,
            expected
                .iter()
                .map(|s| (s * 32768.0) as i16)
                .collect::<Vec<_>>()
        );
    }
}
