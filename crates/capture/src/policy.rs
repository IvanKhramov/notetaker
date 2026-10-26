use std::time::Duration;

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum FallbackReason {
    ProcessSilent,
    ActivationFailed { message: String },
    ProcessCaptureFailed { message: String },
}

/// One-way selection: only an audible endpoint can prove process-loopback silence.
/// Call on a wall-clock tick even when Windows delivers no audio packets.
pub struct FallbackPolicy {
    timeout: Duration,
    last_process_sound: Duration,
    threshold_dbfs: f64,
    selected_endpoint: bool,
}

impl FallbackPolicy {
    pub fn new(timeout: Duration, threshold_dbfs: f64) -> Self {
        Self {
            timeout,
            last_process_sound: Duration::ZERO,
            threshold_dbfs,
            selected_endpoint: false,
        }
    }

    pub fn observe(
        &mut self,
        now: Duration,
        process_dbfs: f64,
        endpoint_dbfs: f64,
    ) -> Option<FallbackReason> {
        if process_dbfs > self.threshold_dbfs {
            self.last_process_sound = now;
        }
        if !self.selected_endpoint
            && now.saturating_sub(self.last_process_sound) >= self.timeout
            && endpoint_dbfs > self.threshold_dbfs
        {
            self.select(FallbackReason::ProcessSilent)
        } else {
            None
        }
    }

    pub fn failed(&mut self, reason: FallbackReason) -> Option<FallbackReason> {
        self.select(reason)
    }

    fn select(&mut self, reason: FallbackReason) -> Option<FallbackReason> {
        if self.selected_endpoint {
            None
        } else {
            self.selected_endpoint = true;
            Some(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> FallbackPolicy {
        FallbackPolicy::new(Duration::from_secs(10), -60.0)
    }
    fn at(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn quiet_endpoint_does_not_trigger_and_fallback_is_latched() {
        let mut p = policy();
        assert_eq!(p.observe(at(30), -120.0, -120.0), None);
        assert_eq!(
            p.observe(at(31), -120.0, -20.0),
            Some(FallbackReason::ProcessSilent)
        );
        assert_eq!(p.observe(at(60), -120.0, -20.0), None);
    }

    #[test]
    fn process_sound_resets_timeout_including_boundary() {
        let mut p = policy();
        assert_eq!(p.observe(at(9), -120.0, -20.0), None);
        assert_eq!(p.observe(at(10), -20.0, -20.0), None);
        assert_eq!(p.observe(at(19), -60.0, -20.0), None);
        assert_eq!(
            p.observe(at(20), -60.0, -20.0),
            Some(FallbackReason::ProcessSilent)
        );
    }

    #[test]
    fn activation_failure_is_immediate_and_once() {
        let mut p = policy();
        let reason = FallbackReason::ActivationFailed {
            message: "unsupported build".into(),
        };
        assert_eq!(p.failed(reason.clone()), Some(reason.clone()));
        assert_eq!(p.failed(reason), None);
        assert_eq!(p.observe(at(20), -120.0, -20.0), None);
    }
}
