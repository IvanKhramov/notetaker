//! Polling call hints only: callers decide whether to offer or stop recording.
//! `now` is elapsed monotonic time from a shared origin, never a FILETIME.
mod registry;
#[cfg(windows)]
mod windows;

pub use registry::{Process, RegistryApp, RegistryUsage, registry_snapshot, registry_timestamp};
#[cfg(windows)]
pub use windows::WindowsSnapshot;

use std::{collections::BTreeMap, time::Duration};

use anyhow::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRule {
    pub exe: String,
    pub name: String,
    pub browser: bool,
}

/// Exact executable names, case insensitive. Settings may append or override rules.
pub const KNOWN_APPS: &[(&str, &str, bool)] = &[
    ("Zoom.exe", "Zoom", false),
    ("Teams.exe", "Teams", false),
    ("ms-teams.exe", "Teams", false),
    ("Webex.exe", "Webex", false),
    ("CiscoCollabHost.exe", "Webex", false),
    ("webexmta.exe", "Webex", false),
    ("webexmeetings.exe", "Webex", false),
    ("Telegram.exe", "Telegram", false),
    ("Discord.exe", "Discord", false),
    ("slack.exe", "Slack", false),
    ("chrome.exe", "Chrome", true),
    ("msedge.exe", "Edge", true),
    ("firefox.exe", "Firefox", true),
    ("browser.exe", "Yandex Browser", true),
];

#[derive(Clone, Debug)]
pub struct DetectConfig {
    pub start_delay: Duration,
    pub end_delay: Duration,
    pub dismiss_cooldown: Duration,
    /// Opt in: browser dictation/voice messages can also hold the microphone.
    pub detect_browsers: bool,
    /// Later entries override earlier ones with the same executable name.
    pub apps: Vec<AppRule>,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            start_delay: Duration::from_secs(5),
            end_delay: Duration::from_secs(20),
            dismiss_cooldown: Duration::from_secs(10 * 60),
            detect_browsers: false,
            apps: KNOWN_APPS
                .iter()
                .map(|&(exe, name, browser)| AppRule {
                    exe: exe.into(),
                    name: name.into(),
                    browser,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectSource {
    Microphone,
    MicrophoneAndOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetectEvent {
    CallStarted {
        app: String,
        pid: u32,
        source: DetectSource,
    },
    CallEnded {
        app: String,
        pid: u32,
    },
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub pid: u32,
    /// Process creation time, to distinguish a reused PID from a previous call.
    pub started_at: u64,
    pub exe: String,
    pub microphone: bool,
    /// Supporting evidence only; playback alone never starts a call.
    pub output_active: bool,
}

#[derive(Default)]
pub struct Snapshot {
    pub processes: Vec<Observation>,
    pub warnings: Vec<String>,
}

pub trait SnapshotProvider {
    fn snapshot(&mut self) -> Result<Snapshot>;
}

struct State {
    app: String,
    since: Duration,
    missing_since: Option<Duration>,
    active: bool,
    dismissed: Option<Duration>,
    repeat: bool,
}

pub struct CallDetector<P> {
    provider: P,
    config: DetectConfig,
    states: BTreeMap<(u32, u64), State>,
    now: Duration,
    warnings: Vec<String>,
}

impl<P: SnapshotProvider> CallDetector<P> {
    pub fn new(provider: P, config: DetectConfig) -> Self {
        Self {
            provider,
            config,
            states: BTreeMap::new(),
            now: Duration::ZERO,
            warnings: vec![],
        }
    }

    /// Errors preserve established calls and reset debounce evidence. Drain
    /// diagnostics with `take_warnings`; a failed poll is not microphone release.
    pub fn poll(&mut self, now: Duration) -> Vec<DetectEvent> {
        self.now = self.now.max(now);
        let now = self.now;
        let snapshot = match self.provider.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.warnings = vec![format!("snapshot: {error:#}")];
                for state in self.states.values_mut() {
                    state.since = now;
                    state.missing_since = (!state.active).then_some(now);
                }
                return vec![];
            }
        };
        self.warnings = snapshot.warnings;
        let mut present = BTreeMap::new();
        let mut live = BTreeMap::new();
        for process in snapshot.processes {
            let key = (process.pid, process.started_at);
            live.insert(key, ());
            let rule = self
                .config
                .apps
                .iter()
                .rev()
                .find(|a| a.exe.eq_ignore_ascii_case(&process.exe));
            if process.microphone
                && let Some(rule) = rule
                && (!rule.browser || self.config.detect_browsers)
            {
                let state = self.states.entry(key).or_insert_with(|| State {
                    app: rule.name.clone(),
                    since: now,
                    missing_since: None,
                    active: false,
                    dismissed: None,
                    repeat: false,
                });
                if state.missing_since.take().is_some() {
                    state.since = now;
                }
                present.insert(key, process.output_active);
            }
        }
        let mut events = vec![];
        self.states.retain(|&(pid, started_at), state| {
            let key = (pid, started_at);
            if let Some(&output) = present.get(&key) {
                let ready = now.saturating_sub(state.since) >= self.config.start_delay;
                let cooldown = state
                    .dismissed
                    .is_none_or(|at| now.saturating_sub(at) >= self.config.dismiss_cooldown);
                if ready && cooldown && (!state.active || state.repeat) {
                    state.active = true;
                    state.repeat = false;
                    events.push(DetectEvent::CallStarted {
                        app: state.app.clone(),
                        pid,
                        source: if output {
                            DetectSource::MicrophoneAndOutput
                        } else {
                            DetectSource::Microphone
                        },
                    });
                }
            } else {
                state.since = now;
                let missing = *state.missing_since.get_or_insert(now);
                if state.active && now.saturating_sub(missing) >= self.config.end_delay {
                    state.active = false;
                    state.repeat = false;
                    events.push(DetectEvent::CallEnded {
                        app: state.app.clone(),
                        pid,
                    });
                }
            }
            // Keep a dismissal across separate calls in the same process.
            state.active
                || present.contains_key(&key)
                || (live.contains_key(&key)
                    && state
                        .dismissed
                        .is_some_and(|at| now.saturating_sub(at) < self.config.dismiss_cooldown))
        });
        events
    }

    /// Dismiss a hint at the last poll's monotonic timestamp. A continuing call
    /// can offer again once the cooldown expires; CallEnded still reports release.
    pub fn dismiss(&mut self, pid: u32) {
        for (&(id, _), state) in &mut self.states {
            if id == pid && state.active {
                state.dismissed = Some(self.now);
                state.repeat = true;
            }
        }
    }

    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }
}
