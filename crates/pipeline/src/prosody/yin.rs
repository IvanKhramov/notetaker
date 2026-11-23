//! YIN on downsampled, fixed-length audio frames.
use notetaker_core::prosody::thresholds::{FRAME_MS, MAX_F0_HZ, MIN_F0_HZ, YIN_THRESHOLD};

pub(super) const SAMPLE_RATE: usize = 16_000;
pub(super) const FRAME_SAMPLES: usize = SAMPLE_RATE * FRAME_MS / 1000;
const DECIMATION: usize = 4;
const RATE: usize = SAMPLE_RATE / DECIMATION;
const SIZE: usize = FRAME_SAMPLES / DECIMATION;
const MIN_LAG: usize = RATE / MAX_F0_HZ;
const MAX_LAG: usize = RATE.div_ceil(MIN_F0_HZ);

pub(super) fn pitch(frame: &[f32; FRAME_SAMPLES]) -> Option<f32> {
    let mut samples = [0.0; SIZE];
    for (value, group) in samples.iter_mut().zip(frame.as_chunks::<DECIMATION>().0) {
        *value = group.iter().sum::<f32>() / DECIMATION as f32;
    }
    let mut normalized = [1.0; MAX_LAG + 2];
    let mut cumulative = 0.0;
    // Use the same comparison length at each lag.
    let window = SIZE - MAX_LAG - 1;
    for lag in 1..normalized.len() {
        let difference = samples[..window]
            .iter()
            .zip(&samples[lag..lag + window])
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>();
        cumulative += difference;
        if cumulative > 0.0 {
            normalized[lag] = difference * lag as f32 / cumulative;
        }
        if lag > MIN_LAG
            && normalized[lag - 1] < YIN_THRESHOLD
            && normalized[lag - 1] <= normalized[lag]
            && normalized[lag - 1] <= normalized[lag - 2]
        {
            let [left, center, right] = [normalized[lag - 2], normalized[lag - 1], normalized[lag]];
            let curvature = left + right - 2.0 * center;
            let adjustment = if curvature > 0.0 {
                (left - right) / (2.0 * curvature)
            } else {
                0.0
            };
            let hz = RATE as f32 / ((lag - 1) as f32 + adjustment);
            return (MIN_F0_HZ as f32..=MAX_F0_HZ as f32)
                .contains(&hz)
                .then_some(hz);
        }
    }
    None
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::prosody) fn sine(hz: f32, amplitude: f32) -> [f32; FRAME_SAMPLES] {
        std::array::from_fn(|i| {
            amplitude * (std::f32::consts::TAU * hz * i as f32 / SAMPLE_RATE as f32).sin()
        })
    }

    #[test]
    fn finds_fundamental_and_rejects_unvoiced_frames() {
        for hz in [120.0, 220.0] {
            let detected = pitch(&sine(hz, 0.25)).unwrap();
            assert!((detected - hz).abs() < 1.0, "{detected} Hz");
        }
        assert_eq!(pitch(&[0.0; FRAME_SAMPLES]), None);
        let mut state = 42_u32;
        let noise = std::array::from_fn(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state as f32 / u32::MAX as f32 - 0.5
        });
        assert_eq!(pitch(&noise), None);
    }
}
