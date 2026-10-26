use std::collections::VecDeque;

use anyhow::{Result, ensure};
use rubato::{FftFixedInOut, Resampler};

use crate::SAMPLE_RATE;

/// Stateful downmix + antialiased resampling. `finish` drains filter latency and
/// trims padding to the exact floor(input_frames * 16000 / input_rate) length.
pub struct Normalizer {
    rate: u32,
    channels: usize,
    resampler: Option<FftFixedInOut<f32>>,
    pending: VecDeque<f32>,
    skip: usize,
    input_frames: usize,
    output_frames: usize,
    finished: bool,
}

impl Normalizer {
    pub fn new(rate: u32, channels: usize) -> Result<Self> {
        ensure!(
            (8_000..=384_000).contains(&rate),
            "unsupported sample rate: {rate}"
        );
        ensure!(
            (1..=32).contains(&channels),
            "unsupported channel count: {channels}"
        );
        let resampler = if rate == SAMPLE_RATE {
            None
        } else {
            Some(FftFixedInOut::new(
                rate as usize,
                SAMPLE_RATE as usize,
                (rate / 100) as usize,
                1,
            )?)
        };
        let skip = resampler.as_ref().map_or(0, Resampler::output_delay);
        Ok(Self {
            rate,
            channels,
            resampler,
            pending: VecDeque::new(),
            skip,
            input_frames: 0,
            output_frames: 0,
            finished: false,
        })
    }

    pub fn push(&mut self, interleaved: &[f32]) -> Result<Vec<f32>> {
        ensure!(!self.finished, "normalizer already finished");
        ensure!(
            interleaved.len().is_multiple_of(self.channels),
            "incomplete interleaved frame"
        );
        ensure!(
            interleaved.iter().all(|s| s.is_finite()),
            "non-finite audio sample"
        );
        let mono: Vec<f32> = interleaved
            .chunks_exact(self.channels)
            .map(|frame| frame.iter().sum::<f32>() / self.channels as f32)
            .collect();
        self.input_frames += mono.len();
        if self.resampler.is_none() {
            self.output_frames += mono.len();
            return Ok(mono);
        }
        self.pending.extend(mono);
        let mut output = Vec::new();
        while self.pending.len() >= self.resampler.as_ref().unwrap().input_frames_next() {
            output.extend(self.process_chunk()?);
        }
        self.output_frames += output.len();
        Ok(output)
    }

    fn process_chunk(&mut self) -> Result<Vec<f32>> {
        let resampler = self.resampler.as_mut().unwrap();
        let input: Vec<f32> = self
            .pending
            .drain(..resampler.input_frames_next())
            .collect();
        let mut output = resampler.process(&[input], None)?.remove(0);
        let skip = self.skip.min(output.len());
        output.drain(..skip);
        self.skip -= skip;
        Ok(output)
    }

    pub fn finish(&mut self) -> Result<Vec<f32>> {
        ensure!(!self.finished, "normalizer already finished");
        self.finished = true;
        let wanted = self.input_frames * SAMPLE_RATE as usize / self.rate as usize;
        let mut output = Vec::new();
        while self.output_frames + output.len() < wanted {
            let count = self.resampler.as_ref().unwrap().input_frames_next();
            self.pending.resize(count, 0.0);
            output.extend(self.process_chunk()?);
        }
        output.truncate(wanted.saturating_sub(self.output_frames));
        self.output_frames += output.len();
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LevelMeter;

    #[test]
    fn stereo_48k_to_mono_16k_preserves_length_level_and_chunk_state() {
        let mut input = Vec::new();
        for n in 0..48_017 {
            let sine = (std::f32::consts::TAU * 440.0 * n as f32 / 48_000.0).sin();
            input.extend([0.6 * sine, 0.2 * sine]);
        }
        let mut resampler = Normalizer::new(48_000, 2).unwrap();
        let mut output = Vec::new();
        for chunk in input.chunks(358) {
            output.extend(resampler.push(chunk).unwrap());
        }
        output.extend(resampler.finish().unwrap());
        assert_eq!(output.len(), 48_017 * 16_000 / 48_000);
        let mut meter = LevelMeter::default();
        meter.push(&output[100..output.len() - 100]);
        assert!((meter.levels().rms - 0.4 / 2_f64.sqrt()).abs() < 0.003);
        assert!(resampler.push(&[0.0, 0.0]).is_err());
    }

    #[test]
    fn antialiases_above_nyquist_and_handles_short_or_empty_input() {
        let mut resampler = Normalizer::new(48_000, 1).unwrap();
        let input: Vec<f32> = (0..4800)
            .map(|n| (std::f32::consts::TAU * 12_000.0 * n as f32 / 48_000.0).sin())
            .collect();
        let mut output = resampler.push(&input).unwrap();
        output.extend(resampler.finish().unwrap());
        let mut meter = LevelMeter::default();
        meter.push(&output[100..output.len() - 100]);
        assert!(meter.levels().rms < 0.01);
        let mut short = Normalizer::new(44_100, 2).unwrap();
        assert!(short.push(&[0.5, 0.5]).unwrap().is_empty());
        assert!(short.finish().unwrap().is_empty());
        assert!(
            Normalizer::new(48_000, 1)
                .unwrap()
                .finish()
                .unwrap()
                .is_empty()
        );
        assert!(
            Normalizer::new(16_000, 1)
                .unwrap()
                .finish()
                .unwrap()
                .is_empty()
        );
    }
}
