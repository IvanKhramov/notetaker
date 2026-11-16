//! Polling stays off the UI thread. Prompt policy is platform independent.
use notetaker_detect::{DetectEvent, DetectedCall};

use crate::state::Phase;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Countdown {
    pub remaining: Duration,
    last_tick: Instant,
}
impl Countdown {
    pub fn new(now: Instant) -> Self {
        Self {
            remaining: Duration::from_secs(30),
            last_tick: now,
        }
    }
    pub fn tick(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        if elapsed > Duration::from_secs(3) {
            self.remaining = Duration::from_secs(30);
        } else {
            self.remaining = self
                .remaining
                .saturating_sub(elapsed.min(Duration::from_secs(1)));
        }
        self.remaining.is_zero()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallPrompt {
    Start {
        started_at: u64,
        app: String,
        pid: u32,
        meeting_title: Option<String>,
    },
    Stop {
        pid: u32,
        started_at: u64,
        countdown: Countdown,
    },
}

/// Microphone release alone cannot distinguish a hang-up from mute.
pub fn proposal(
    event: &DetectEvent,
    phase: Phase,
    recorded_call: Option<(u32, u64)>,
    stop_requested: bool,
    auto_stop: bool,
    now: Instant,
) -> Option<CallPrompt> {
    match event {
        DetectEvent::CallStarted {
            app,
            pid,
            started_at,
            meeting_title,
            ..
        } if phase == Phase::Idle => Some(CallPrompt::Start {
            app: app.clone(),
            pid: *pid,
            started_at: *started_at,
            meeting_title: meeting_title.clone(),
        }),
        DetectEvent::CallEnded {
            pid,
            started_at,
            reason: notetaker_detect::CallEndReason::ProcessExited,
            ..
        } if auto_stop
            && *started_at > 0
            && matches!(phase, Phase::Starting | Phase::Recording)
            && recorded_call == Some((*pid, *started_at))
            && !stop_requested =>
        {
            Some(CallPrompt::Stop {
                pid: *pid,
                started_at: *started_at,
                countdown: Countdown::new(now),
            })
        }
        _ => None,
    }
}

pub enum StopAction {
    None,
    Stop,
    Continue,
}
pub fn stop_card(ui: &mut eframe::egui::Ui, remaining: Duration) -> StopAction {
    use crate::theme::{self, ButtonStyle};
    let mut action = StopAction::None;
    let frame = eframe::egui::Frame::popup(ui.style());
    frame.show(ui, |ui| {
        ui.set_width(
            ui.available_width()
                .min(340.0 - frame.total_margin().sum().x),
        );
        let seconds = remaining.as_millis().div_ceil(1000);
        ui.label(
            eframe::egui::RichText::new(format!(
                "Звонок завершён. Запись остановится через {seconds} с"
            ))
            .font(theme::medium(theme::BODY)),
        );
        theme::footer(ui, |ui| {
            if theme::button(ui, "Продолжить запись", ButtonStyle::Ghost).clicked()
            {
                action = StopAction::Continue;
            }
            if theme::button(ui, "Остановить сейчас", ButtonStyle::Primary).clicked()
            {
                action = StopAction::Stop;
            }
        });
    });
    action
}

#[derive(Default)]
pub struct Monitor {
    apps_input: String,
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
                let mut previous = None;
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
                            if !update.events.is_empty()
                                || previous.as_ref()
                                    != Some(&(update.generation, update.calls.clone()))
                            {
                                previous = Some((update.generation, update.calls.clone()));
                                if events.send(update).is_err() {
                                    return;
                                }
                                crate::worker::wake_ui();
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
        if self.apps_input == apps
            && self.settings.as_ref().is_some_and(|s| {
                s.enabled == enabled && s.browsers == browsers && s.any_microphone == any_microphone
            })
        {
            return;
        }
        self.apps_input = apps.to_owned();
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

/// Suppressing presentation preserves the pending call until the blocker closes.
pub fn start_visible(interaction_blocked: bool, speech_ready: bool) -> bool {
    !interaction_blocked && speech_ready
}
pub fn start_card(
    ui: &mut eframe::egui::Ui,
    app: &str,
    title: Option<&str>,
    reason: Option<&str>,
) -> (bool, bool, eframe::egui::Vec2) {
    use crate::theme::{self, ButtonStyle, Palette};
    let mut accept = false;
    let mut dismiss = false;
    let panel = eframe::egui::Frame::new()
        .fill(Palette::of(ui).card)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(316.0);
            ui.add(
                eframe::egui::Label::new(
                    eframe::egui::RichText::new(format!("Звонок в {app}")).strong(),
                )
                .truncate(),
            );
            ui.add(eframe::egui::Label::new(title.unwrap_or("Записать встречу?")).truncate());
            ui.horizontal(|ui| {
                accept = ui
                    .add_enabled_ui(reason.is_none(), |ui| {
                        theme::button(ui, "Записать", ButtonStyle::Primary)
                    })
                    .inner
                    .clicked();
                dismiss = theme::button(ui, "Не сейчас", ButtonStyle::Secondary).clicked();
            });
            if let Some(reason) = reason {
                ui.weak(reason);
            }
        });
    (accept, dismiss, panel.response.rect.size())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_detect::DetectSource;

    #[test]
    fn offers_start_only_while_idle() {
        let event = DetectEvent::CallStarted {
            started_at: 100,
            app: "Zoom".into(),
            pid: 42,
            source: DetectSource::Microphone,
            meeting_title: Some("Planning".into()),
        };
        assert_eq!(
            proposal(&event, Phase::Idle, None, false, true, Instant::now()),
            Some(CallPrompt::Start {
                started_at: 100,
                app: "Zoom".into(),
                pid: 42,
                meeting_title: Some("Planning".into()),
            })
        );
        let now = Instant::now();
        for phase in [Phase::Starting, Phase::Recording, Phase::Finalizing] {
            assert_eq!(
                proposal(&event, phase, Some((42, 100)), false, true, now),
                None
            );
        }
    }
    #[test]
    fn multiple_detected_calls_require_an_explicit_pid() {
        let monitor = Monitor::with_calls(vec![
            DetectedCall {
                started_at: 100,
                app: "Zoom".into(),
                pid: 1,
                meeting_title: Some("A".into()),
            },
            DetectedCall {
                started_at: 100,
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
        let now = Instant::now();
        let event = DetectEvent::CallEnded {
            started_at: 100,
            reason: notetaker_detect::CallEndReason::ProcessExited,
            app: "Zoom".into(),
            pid: 42,
        };
        for phase in [Phase::Starting, Phase::Recording] {
            assert_eq!(
                proposal(&event, phase, Some((42, 100)), false, true, now),
                Some(CallPrompt::Stop {
                    pid: 42,
                    started_at: 100,
                    countdown: Countdown::new(now)
                })
            );
            for pid in [None, Some((7, 100)), Some((42, 200))] {
                assert_eq!(proposal(&event, phase, pid, false, true, now), None);
            }
            assert_eq!(
                proposal(&event, phase, Some((42, 100)), true, true, now),
                None
            );
        }
        for phase in [Phase::Idle, Phase::Finalizing] {
            assert_eq!(
                proposal(&event, phase, Some((42, 100)), false, true, now),
                None
            );
        }
    }
}
