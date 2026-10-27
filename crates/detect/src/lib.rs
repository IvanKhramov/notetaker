//! Polling call hints only: callers decide whether to offer or stop recording.
//! `now` is elapsed monotonic time from a shared origin, never a FILETIME.
mod registry;
mod title;
#[cfg(windows)]
mod windows;

pub use registry::{
    Process, RegistryApp, RegistryUsage, process_tree_pids, registry_snapshot, registry_timestamp,
};
pub use title::{CallIdentity, identify_call, parse_call_title};
#[cfg(windows)]
pub use windows::WindowsSnapshot;

use std::{collections::BTreeMap, time::Duration};

use anyhow::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRule {
    pub exe: String,
    /// Empty uses FileDescription or the executable stem.
    pub name: String,
    pub browser: bool,
}

/// Exact executable names, case insensitive. Settings may append or override rules.
pub const KNOWN_APPS: &[(&str, &str, bool)] = &[
    // Best-effort desktop names; confirm in Task Manager > Details.
    ("Telemost.exe", "Телемост", false),
    ("YandexTelemost.exe", "Телемост", false),
    ("Kontur.Talk.exe", "Контур.Толк", false),
    ("KonturTalk.exe", "Контур.Толк", false),
    ("ktalk.exe", "Контур.Толк", false),
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
    /// Browser dictation/voice messages can also hold the microphone.
    pub detect_browsers: bool,
    pub detect_any_microphone: bool,
    /// Later entries override earlier ones with the same executable name.
    pub apps: Vec<AppRule>,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            start_delay: Duration::from_secs(5),
            end_delay: Duration::from_secs(20),
            dismiss_cooldown: Duration::from_secs(10 * 60),
            detect_browsers: true,
            detect_any_microphone: true,
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
        meeting_title: Option<String>,
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
    pub system: bool,
    pub description: Option<String>,
    pub call: Option<CallIdentity>,
}

#[derive(Default)]
pub struct Snapshot {
    pub processes: Vec<Observation>,
    pub warnings: Vec<String>,
}

pub trait SnapshotProvider {
    fn snapshot(&mut self) -> Result<Snapshot>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedCall {
    pub app: String,
    pub pid: u32,
    pub meeting_title: Option<String>,
}

/// Never offer our own capture or common Windows recording/system processes.
fn denied_exe(exe: &str) -> bool {
    [
        "notetaker.exe",
        "notetaker-app.exe",
        "notetaker-asr-worker.exe",
        "SoundRecorder.exe",
        "VoiceRecorder.exe",
        "WindowsSoundRecorder.exe",
        "svchost.exe",
        "audiodg.exe",
        "System",
        "Idle",
        "RuntimeBroker.exe",
        "backgroundTaskHost.exe",
        "SpeechRuntime.exe",
        "SpeechRuntimeWorker.exe",
        "SearchHost.exe",
        "SearchApp.exe",
        "Cortana.exe",
    ]
    .iter()
    .any(|name| exe.eq_ignore_ascii_case(name))
}

fn exe_stem(exe: &str) -> &str {
    if exe
        .get(exe.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".exe"))
    {
        &exe[..exe.len() - 4]
    } else {
        exe
    }
}

struct State {
    app: String,
    meeting_title: Option<String>,
    can_offer: bool,
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
    snapshot_ok: bool,
}

impl<P: SnapshotProvider> CallDetector<P> {
    pub fn new(provider: P, config: DetectConfig) -> Self {
        Self {
            provider,
            config,
            states: BTreeMap::new(),
            now: Duration::ZERO,
            warnings: vec![],
            snapshot_ok: false,
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
                self.snapshot_ok = false;
                self.warnings = vec![format!("snapshot: {error:#}")];
                for state in self.states.values_mut() {
                    state.since = now;
                    state.missing_since = (!state.active).then_some(now);
                }
                return vec![];
            }
        };
        self.snapshot_ok = true;
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
            let allowed = rule.map_or(self.config.detect_any_microphone, |rule| {
                !rule.browser || self.config.detect_browsers
            });
            if process.microphone
                && !process.system
                && !denied_exe(&process.exe)
                && (allowed || self.states.get(&key).is_some_and(|state| state.active))
            {
                let name = rule
                    .map(|r| r.name.as_str())
                    .filter(|name| !name.is_empty())
                    .or(process.description.as_deref())
                    .unwrap_or_else(|| exe_stem(&process.exe));
                let app = match &process.call {
                    Some(call) if rule.is_some_and(|r| r.browser) => {
                        format!("{} ({name})", call.service)
                    }
                    Some(call) => call.service.clone(),
                    None => name.to_owned(),
                };
                let meeting_title = process.call.and_then(|call| call.meeting_title);
                let state = self.states.entry(key).or_insert_with(|| State {
                    app: app.clone(),
                    meeting_title: meeting_title.clone(),
                    can_offer: allowed,
                    since: now,
                    missing_since: None,
                    active: false,
                    dismissed: None,
                    repeat: false,
                });
                state.can_offer = allowed;
                state.app = app;
                state.meeting_title = meeting_title;
                if state.missing_since.take().is_some() {
                    state.since = now;
                }
                present.insert(key, (process.output_active, allowed));
            }
        }
        let mut events = vec![];
        self.states.retain(|&(pid, started_at), state| {
            let key = (pid, started_at);
            if let Some(&(output, can_offer)) = present.get(&key) {
                let ready = now.saturating_sub(state.since) >= self.config.start_delay;
                let cooldown = state
                    .dismissed
                    .is_none_or(|at| now.saturating_sub(at) >= self.config.dismiss_cooldown);
                if can_offer && ready && cooldown && (!state.active || state.repeat) {
                    state.active = true;
                    state.repeat = false;
                    events.push(DetectEvent::CallStarted {
                        app: state.app.clone(),
                        pid,
                        meeting_title: state.meeting_title.clone(),
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

    /// Change eligibility without discarding established calls or dismissals.
    pub fn set_detect_browsers(&mut self, enabled: bool) {
        self.config.detect_browsers = enabled;
    }

    /// Replace user rules while retaining debounce and dismissal history.
    pub fn configure_apps(&mut self, any_microphone: bool, apps: &[String]) {
        self.config.detect_any_microphone = any_microphone;
        self.config.apps = DetectConfig::default().apps;
        for exe in apps
            .iter()
            .map(|exe| exe.trim())
            .filter(|exe| !exe.is_empty())
        {
            // Listing a known browser must not bypass the browser opt-out.
            if !self
                .config
                .apps
                .iter()
                .any(|rule| rule.exe.eq_ignore_ascii_case(exe))
            {
                self.config.apps.push(AppRule {
                    exe: exe.into(),
                    name: String::new(),
                    browser: false,
                });
            }
        }
    }

    /// Current confirmed microphone owners, including dismissed prompts. A released
    /// microphone is removed immediately, independently of the end debounce.
    pub fn current_calls(&self) -> Vec<DetectedCall> {
        if !self.snapshot_ok {
            return vec![];
        }
        self.states
            .iter()
            .filter(|(_, state)| state.active && state.can_offer && state.missing_since.is_none())
            .map(|(&(pid, _), state)| DetectedCall {
                app: state.app.clone(),
                pid,
                meeting_title: state.meeting_title.clone(),
            })
            .collect()
    }

    /// Calls offered while recording were intentionally ignored by the UI.
    /// Rearm all continuing calls after the usual dismissal cooldown.
    pub fn recording_stopped(&mut self) {
        for state in self.states.values_mut().filter(|state| state.active) {
            state.dismissed = Some(self.now);
            state.repeat = true;
        }
    }

    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }
}
