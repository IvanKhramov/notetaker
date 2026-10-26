use serde::Serialize;

/// A finite reporting floor; digital silence has mathematically -infinite dBFS.
pub const SILENCE_DBFS: f64 = -120.0;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Levels {
    pub samples: u64,
    pub rms: f64,
    pub average_dbfs: f64,
    pub peak_dbfs: f64,
}

impl Levels {
    pub fn is_silent(&self, threshold_dbfs: f64) -> bool {
        self.average_dbfs <= threshold_dbfs
    }
}

/// Accumulates energy, rather than averaging logarithmic per-packet readings.
#[derive(Default)]
pub struct LevelMeter {
    samples: u64,
    energy: f64,
    peak: f64,
}

impl LevelMeter {
    pub fn push(&mut self, samples: &[f32]) {
        for &sample in samples {
            let value = if sample.is_finite() {
                f64::from(sample)
            } else {
                0.0
            };
            self.energy += value * value;
            self.peak = self.peak.max(value.abs());
        }
        self.samples += samples.len() as u64;
    }

    pub fn levels(&self) -> Levels {
        let rms = (self.energy / self.samples.max(1) as f64).sqrt();
        Levels {
            samples: self.samples,
            rms,
            average_dbfs: dbfs(rms),
            peak_dbfs: dbfs(self.peak),
        }
    }
}

fn dbfs(amplitude: f64) -> f64 {
    (20.0 * amplitude.max(1e-6).log10()).max(SILENCE_DBFS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_and_energy_weighted_levels() {
        let mut meter = LevelMeter::default();
        assert!(meter.levels().is_silent(-60.0));
        meter.push(&[0.0; 3]);
        assert_eq!(meter.levels().peak_dbfs, SILENCE_DBFS);
        meter.push(&[1.0]);
        let levels = meter.levels();
        assert_eq!(levels.samples, 4);
        assert!((levels.rms - 0.5).abs() < 1e-9);
        assert!((levels.average_dbfs + 6.0206).abs() < 1e-4);
        assert_eq!(levels.peak_dbfs, 0.0);
        assert!(!levels.is_silent(-60.0));
    }
}
