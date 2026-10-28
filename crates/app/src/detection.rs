//! Polling stays off the UI thread. Prompt policy is platform independent.
use notetaker_detect::{DetectEvent, DetectedCall};

use crate::state::Phase;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallPrompt {
    Start {
        app: String,
        pid: u32,
        meeting_title: Option<String>,
    },
    Stop {
        pid: u32,
    },
}

/// A call ending is only a hint, never an instruction to stop the pipeline.
pub fn proposal(
    event: &DetectEvent,
    phase: Phase,
    recorded_pid: Option<u32>,
    stop_requested: bool,
) -> Option<CallPrompt> {
    match event {
        DetectEvent::CallStarted {
            app,
            pid,
            meeting_title,
            ..
        } if phase == Phase::Idle => Some(CallPrompt::Start {
            app: app.clone(),
            pid: *pid,
            meeting_title: meeting_title.clone(),
        }),
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
    #[cfg(any(windows, target_os = "macos"))]
    channels: Option<(
        std::sync::mpsc::Sender<Command>,
        std::sync::mpsc::Receiver<Update>,
    )>,
    settings: Option<DetectionSettings>,
    calls: Vec<DetectedCall>,
    #[cfg(any(windows, target_os = "macos"))]
    generation: u64,
}
#[derive(Clone, PartialEq, Eq)]
struct DetectionSettings {
    enabled: bool,
    browsers: bool,
    any_microphone: bool,
    apps: Vec<String>,
}
#[cfg(any(windows, target_os = "macos"))]
struct Update {
    generation: u64,
    events: Vec<DetectEvent>,
    calls: Vec<DetectedCall>,
}
#[cfg(any(windows, target_os = "macos"))]
enum Command {
    Configure {
        generation: u64,
        settings: DetectionSettings,
    },
    Dismiss(u32),
    RecordingStopped,
}
impl Monitor {
    pub fn start(&mut self) {
        #[cfg(any(windows, target_os = "macos"))]
        {
            use notetaker_detect::{CallDetector, DetectConfig};
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
                let mut generation = 0;
                #[cfg(windows)]
                let provider = notetaker_detect::WindowsSnapshot;
                #[cfg(target_os = "macos")]
                let provider = notetaker_detect::MacosSnapshot;
                let mut detector = CallDetector::new(provider, DetectConfig::default());
                let mut next_poll = Instant::now();
                loop {
                    match commands.recv_timeout(next_poll.saturating_duration_since(Instant::now()))
                    {
                        Ok(Command::Configure {
                            generation: next,
                            settings,
                        }) => {
                            generation = next;
                            enabled = settings.enabled;
                            detector.set_detect_browsers(settings.browsers);
                            detector.configure_apps(settings.any_microphone, &settings.apps);
                        }
                        Ok(Command::Dismiss(pid)) => detector.dismiss(pid),
                        Ok(Command::RecordingStopped) => detector.recording_stopped(),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if Instant::now() >= next_poll {
                        if enabled {
                            let update = Update {
                                generation,
                                events: detector.poll(origin.elapsed()),
                                calls: detector.current_calls(),
                            };
                            if events.send(update).is_err() {
                                return;
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
    pub fn configure(&mut self, enabled: bool, browsers: bool, any_microphone: bool, apps: &str) {
        let settings = DetectionSettings {
            enabled,
            browsers,
            any_microphone,
            apps: apps
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        };
        if self.settings.as_ref() == Some(&settings) {
            return;
        }
        self.calls.clear();
        self.settings = Some(settings.clone());
        #[cfg(any(windows, target_os = "macos"))]
        if let Some((tx, _)) = &self.channels {
            self.generation += 1;
            let _ = tx.send(Command::Configure {
                generation: self.generation,
                settings,
            });
        }
    }

    #[cfg(test)]
    pub fn with_calls(calls: Vec<DetectedCall>) -> Self {
        Self {
            calls,
            ..Self::default()
        }
    }

    /// Do not choose between several simultaneous calls without an explicit PID.
    pub fn current_call(&self, pid: Option<u32>) -> Option<&DetectedCall> {
        match pid {
            Some(pid) => self.calls.iter().find(|call| call.pid == pid),
            None if self.calls.len() == 1 => self.calls.first(),
            None => None,
        }
    }

    pub fn dismiss(&self, _pid: u32) {
        #[cfg(any(windows, target_os = "macos"))]
        if let Some((tx, _)) = &self.channels {
            let _ = tx.send(Command::Dismiss(_pid));
        }
    }
    pub fn recording_stopped(&self) {
        #[cfg(any(windows, target_os = "macos"))]
        if let Some((tx, _)) = &self.channels {
            let _ = tx.send(Command::RecordingStopped);
        }
    }
    pub fn poll(&mut self) -> Vec<DetectEvent> {
        #[cfg(any(windows, target_os = "macos"))]
        if let Some((_, rx)) = &self.channels {
            let mut events = vec![];
            for update in rx.try_iter() {
                if update.generation != self.generation {
                    continue;
                }
                self.calls = update.calls;
                events.extend(update.events);
            }
            return events;
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
            meeting_title: Some("Planning".into()),
        };
        assert_eq!(
            proposal(&event, Phase::Idle, None, false),
            Some(CallPrompt::Start {
                app: "Zoom".into(),
                pid: 42,
                meeting_title: Some("Planning".into()),
            })
        );
        for phase in [Phase::Starting, Phase::Recording, Phase::Finalizing] {
            assert_eq!(proposal(&event, phase, Some(42), false), None);
        }
    }
    #[test]
    fn multiple_detected_calls_require_an_explicit_pid() {
        let monitor = Monitor::with_calls(vec![
            DetectedCall {
                app: "Zoom".into(),
                pid: 1,
                meeting_title: Some("A".into()),
            },
            DetectedCall {
                app: "Телемост".into(),
                pid: 2,
                meeting_title: Some("B".into()),
            },
        ]);
        assert!(monitor.current_call(None).is_none());
        assert_eq!(
            monitor
                .current_call(Some(2))
                .unwrap()
                .meeting_title
                .as_deref(),
            Some("B")
        );
        assert!(monitor.current_call(Some(3)).is_none());
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
