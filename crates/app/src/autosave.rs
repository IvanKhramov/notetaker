//! Coalesce edits without losing work while a worker or meeting is busy.
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Autosave {
    pub pending: bool,
    pub llm: bool,
    pub error: Option<String>,
    due: Option<Instant>,
}
impl Autosave {
    pub fn wake_after(&self, now: Instant) -> Option<Duration> {
        (self.pending && self.error.is_none())
            .then_some(self.due)
            .flatten()
            .map(|due| due.saturating_duration_since(now))
    }
    pub fn edit(&mut self, now: Instant, delayed: bool, llm: bool) {
        self.pending = true;
        self.llm |= llm;
        self.error = None;
        self.due = Some(
            now + if delayed {
                Duration::from_millis(700)
            } else {
                Duration::ZERO
            },
        );
    }
    pub fn ready(&self, now: Instant, allowed: bool) -> bool {
        self.pending && self.error.is_none() && allowed && self.due.is_some_and(|due| now >= due)
    }
    pub fn sent(&mut self) {
        self.pending = false;
        self.llm = false;
        self.due = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_edits_wait_and_coalesce() {
        let mut save = Autosave::default();
        let now = Instant::now();
        save.edit(now, false, true);
        assert!(!save.ready(now, false));
        save.edit(now, false, false);
        assert!(save.llm);
        assert!(save.ready(now, true));
        save.sent();
        assert!(!save.ready(now, true));
    }
    #[test]
    fn typing_restarts_debounce_and_blur_flushes() {
        let mut save = Autosave::default();
        let now = Instant::now();
        save.edit(now, true, false);
        assert!(!save.ready(now + Duration::from_millis(699), true));
        save.edit(now + Duration::from_millis(600), true, true);
        assert!(!save.ready(now + Duration::from_millis(700), true));
        assert!(save.ready(now + Duration::from_millis(1300), true));
        save.edit(now, false, false);
        assert!(save.ready(now, true));
    }
    #[test]
    fn invalid_edits_wait_for_a_correction() {
        let mut save = Autosave::default();
        let now = Instant::now();
        save.edit(now, false, true);
        save.error = Some("Invalid provider".into());
        assert!(!save.ready(now, true));
        save.edit(now, false, true);
        assert!(save.ready(now, true));
    }
}
