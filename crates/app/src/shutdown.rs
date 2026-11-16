//! Monotonic shutdown deadlines, independent of wall-clock changes.
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Shutdown {
    pub cancel: notetaker_llm::Cancellation,
    started: Option<Instant>,
    forced: Option<Instant>,
}
impl Shutdown {
    pub fn request(&mut self, now: Instant) {
        self.started.get_or_insert(now);
    }
    pub fn can_force(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(10))
    }
    pub fn force(&mut self, now: Instant) -> bool {
        if !self.can_force(now) || self.forced.is_some() {
            return false;
        }
        self.forced = Some(now);
        true
    }
    pub fn forced(&self) -> bool {
        self.forced.is_some()
    }
    pub fn deadline_passed(&self, now: Instant) -> bool {
        self.forced
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn force_delay_and_backstop_use_injected_monotonic_time() {
        let start = Instant::now();
        let mut exit = Shutdown::default();
        exit.request(start);
        exit.request(start + Duration::from_secs(8));
        assert!(!exit.force(start + Duration::from_secs(9)));
        assert!(exit.force(start + Duration::from_secs(10)));
        assert!(!exit.force(start + Duration::from_secs(20)));
        assert!(!exit.deadline_passed(start + Duration::from_secs(39)));
        assert!(exit.deadline_passed(start + Duration::from_secs(40)));
    }
}
