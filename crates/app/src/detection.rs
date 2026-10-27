//! Polling stays off the UI thread. Prompt policy is platform independent.
use notetaker_detect::DetectEvent;

use crate::state::Phase;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallPrompt {
    Start { app: String, pid: u32 },
    Stop { pid: u32 },
}

/// A call ending is only a hint, never an instruction to stop the pipeline.
pub fn proposal(
    event: &DetectEvent,
    phase: Phase,
    recorded_pid: Option<u32>,
    stop_requested: bool,
) -> Option<CallPrompt> {
    match event {
        DetectEvent::CallStarted { app, pid, .. } if phase == Phase::Idle => {
            Some(CallPrompt::Start {
                app: app.clone(),
                pid: *pid,
            })
        }
        DetectEvent::CallEnded { pid, .. }
            if matches!(phase, Phase::Starting | Phase::Recording)
                && recorded_pid == Some(*pid)
                && !stop_requested =>
        {
            Some(CallPrompt::Stop { pid: *pid })
        }
        _ => None,
    }
}

#[derive(Default)]
pub struct Monitor {
    #[cfg(windows)]
    channels: Option<(
        std::sync::mpsc::Sender<Command>,
        std::sync::mpsc::Receiver<DetectEvent>,
    )>,
    settings: Option<(bool, bool)>,
}
#[cfg(windows)]
enum Command {
    Configure { enabled: bool, browsers: bool },
    Dismiss(u32),
    RecordingStopped,
}
impl Monitor {
    pub fn start(&mut self) {
        #[cfg(windows)]
        {
            use notetaker_detect::{CallDetector, DetectConfig, WindowsSnapshot};
            use std::{
                sync::mpsc,
                thread,
                time::{Duration, Instant},
            };
            let (tx, commands) = mpsc::channel();
            let (events, rx) = mpsc::channel();
            self.channels = Some((tx, rx));
            thread::spawn(move || {
                let origin = Instant::now();
                let mut enabled = false;
                let mut detector = CallDetector::new(WindowsSnapshot, DetectConfig::default());
                let mut next_poll = Instant::now();
                loop {
                    match commands.recv_timeout(next_poll.saturating_duration_since(Instant::now()))
                    {
                        Ok(Command::Configure {
                            enabled: on,
                            browsers,
                        }) => {
                            enabled = on;
                            detector.set_detect_browsers(browsers);
                        }
                        Ok(Command::Dismiss(pid)) => detector.dismiss(pid),
                        Ok(Command::RecordingStopped) => detector.recording_stopped(),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if Instant::now() >= next_poll {
                        if enabled {
                            for event in detector.poll(origin.elapsed()) {
                                if events.send(event).is_err() {
                                    return;
                                }
                            }
                            for warning in detector.take_warnings() {
                                eprintln!("Call detection: {warning}");
                            }
                        }
                        next_poll = Instant::now() + Duration::from_secs(2);
                    }
                }
            });
        }
    }
    pub fn configure(&mut self, enabled: bool, browsers: bool) {
        if self.settings == Some((enabled, browsers)) {
            return;
        }
        self.settings = Some((enabled, browsers));
        #[cfg(windows)]
        if let Some((tx, _)) = &self.channels {
            let _ = tx.send(Command::Configure { enabled, browsers });
        }
    }
    pub fn dismiss(&self, _pid: u32) {
        #[cfg(windows)]
        if let Some((tx, _)) = &self.channels {
            let _ = tx.send(Command::Dismiss(_pid));
        }
    }
    pub fn recording_stopped(&self) {
        #[cfg(windows)]
        if let Some((tx, _)) = &self.channels {
            let _ = tx.send(Command::RecordingStopped);
        }
    }
    pub fn poll(&self) -> Vec<DetectEvent> {
        #[cfg(windows)]
        if let Some((_, rx)) = &self.channels {
            return rx.try_iter().collect();
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_detect::DetectSource;

    #[test]
    fn offers_start_only_while_idle() {
        let event = DetectEvent::CallStarted {
            app: "Zoom".into(),
            pid: 42,
            source: DetectSource::Microphone,
        };
        assert_eq!(
            proposal(&event, Phase::Idle, None, false),
            Some(CallPrompt::Start {
                app: "Zoom".into(),
                pid: 42
            })
        );
        for phase in [Phase::Starting, Phase::Recording, Phase::Finalizing] {
            assert_eq!(proposal(&event, phase, Some(42), false), None);
        }
    }
    #[test]
    fn offers_stop_only_for_current_process_before_stopping() {
        let event = DetectEvent::CallEnded {
            app: "Zoom".into(),
            pid: 42,
        };
        for phase in [Phase::Starting, Phase::Recording] {
            assert_eq!(
                proposal(&event, phase, Some(42), false),
                Some(CallPrompt::Stop { pid: 42 })
            );
            for pid in [None, Some(7)] {
                assert_eq!(proposal(&event, phase, pid, false), None);
            }
            assert_eq!(proposal(&event, phase, Some(42), true), None);
        }
        for phase in [Phase::Idle, Phase::Finalizing] {
            assert_eq!(proposal(&event, phase, Some(42), false), None);
        }
    }
}
