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

/// Delay quiet process frames as well as endpoint frames: once process silence
/// reaches TrackWriter it cannot be replaced by earlier endpoint audio.
#[cfg(any(windows, test))]
pub(crate) struct LoopbackBuffer {
    window: Duration,
    process: std::collections::VecDeque<crate::AudioFrame>,
    endpoint: std::collections::VecDeque<crate::AudioFrame>,
}
#[cfg(any(windows, test))]
impl LoopbackBuffer {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            process: Default::default(),
            endpoint: Default::default(),
        }
    }

    pub fn select(
        &mut self,
        now: Duration,
        process_active: bool,
        process_audible: bool,
        process: Vec<crate::AudioFrame>,
        endpoint: Vec<crate::AudioFrame>,
    ) -> Vec<crate::AudioFrame> {
        self.process.extend(process);
        self.endpoint.extend(endpoint);
        if !process_active {
            self.process.clear();
            return self.endpoint.drain(..).collect();
        }
        if process_audible {
            self.endpoint.clear();
            return self.process.drain(..).collect();
        }
        let cutoff = now.saturating_sub(self.window);
        let end =
            |f: &crate::AudioFrame| f.captured_at + crate::sample_offset(f.samples.len() as u64);
        while self.endpoint.front().is_some_and(|f| end(f) <= cutoff) {
            self.endpoint.pop_front();
        }
        let mut ready = Vec::new();
        while self.process.front().is_some_and(|f| end(f) <= cutoff) {
            ready.push(self.process.pop_front().expect("front exists"));
        }
        ready
    }

    /// Coalesce the final quiet tail into one message for nonblocking stop.
    pub fn finish(&mut self) -> Option<crate::AudioFrame> {
        self.endpoint.clear();
        let mut frame = self.process.pop_front()?;
        for next in self.process.drain(..) {
            let position =
                crate::sample_position(next.captured_at.saturating_sub(frame.captured_at)) as usize;
            if position > frame.samples.len() {
                frame.samples.resize(position, 0.0);
            }
            let skip = frame
                .samples
                .len()
                .saturating_sub(position)
                .min(next.samples.len());
            frame.samples.extend_from_slice(&next.samples[skip..]);
        }
        Some(frame)
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
    fn frame(at_ms: u64, value: f32) -> crate::AudioFrame {
        crate::AudioFrame {
            channel: crate::Channel::System,
            captured_at: Duration::from_millis(at_ms),
            samples: vec![value; 16000],
        }
    }
    #[test]
    fn fallback_replays_endpoint_window_before_any_process_silence_is_committed() {
        let mut buffer = LoopbackBuffer::new(at(10));
        for second in 0..10 {
            let selected = buffer.select(
                at(second + 1),
                true,
                false,
                vec![frame(second * 1000, 0.0)],
                vec![frame(second * 1000, 0.5)],
            );
            assert!(selected.is_empty());
        }
        let selected = buffer.select(at(10), false, false, vec![], vec![]);
        assert_eq!(selected.len(), 10);
        for (second, frame) in selected.iter().enumerate() {
            assert_eq!(frame.captured_at, at(second as u64));
            assert!(frame.samples.iter().all(|s| *s == 0.5));
        }
        assert!(buffer.finish().is_none());
    }
    #[test]
    fn process_sound_discards_endpoint_and_buffer_is_bounded_when_both_are_quiet() {
        let mut buffer = LoopbackBuffer::new(at(10));
        buffer.select(at(1), true, false, vec![frame(0, 0.0)], vec![frame(0, 0.5)]);
        let selected = buffer.select(
            at(2),
            true,
            true,
            vec![frame(1000, 0.25)],
            vec![frame(1000, 0.5)],
        );
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[1].samples[0], 0.25);
        assert!(buffer.endpoint.is_empty());
        for second in 2..100 {
            buffer.select(
                at(second + 1),
                true,
                false,
                vec![frame(second * 1000, 0.0)],
                vec![frame(second * 1000, 0.0)],
            );
            assert!(buffer.process.len() <= 10);
            assert!(buffer.endpoint.len() <= 10);
        }
        let tail = buffer.finish().unwrap();
        assert_eq!(tail.captured_at, at(90));
        assert_eq!(tail.samples.len(), 160000);
    }
}
