use std::{collections::VecDeque, time::Duration};

use anyhow::{Result, anyhow};
use notetaker_detect::{
    CallDetector, DetectConfig, DetectEvent, DetectSource, Observation, Snapshot, SnapshotProvider,
};

struct Fake(VecDeque<Result<Snapshot>>);
impl SnapshotProvider for Fake {
    fn snapshot(&mut self) -> Result<Snapshot> {
        self.0.pop_front().expect("unexpected poll")
    }
}
fn observation(exe: &str, microphone: bool) -> Observation {
    Observation {
        pid: 7,
        started_at: 100,
        exe: exe.into(),
        microphone,
        output_active: true,
    }
}
fn detector(exe: &str, mic: &[bool], config: DetectConfig) -> CallDetector<Fake> {
    CallDetector::new(
        Fake(
            mic.iter()
                .map(|&microphone| {
                    Ok(Snapshot {
                        processes: vec![observation(exe, microphone)],
                        warnings: vec![],
                    })
                })
                .collect(),
        ),
        config,
    )
}
fn at(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}
fn started(events: &[DetectEvent]) -> bool {
    matches!(events, [DetectEvent::CallStarted { app, pid: 7, source: DetectSource::MicrophoneAndOutput }] if app == "Teams")
}

#[test]
fn start_requires_continuous_microphone_use_and_emits_once() {
    let mut d = detector(
        "MS-TEAMS.EXE",
        &[true, true, false, true, true, true, true],
        DetectConfig::default(),
    );
    for t in [0, 4, 5, 6, 10] {
        assert!(d.poll(at(t)).is_empty());
    }
    assert!(started(&d.poll(at(11))));
    assert!(d.poll(at(30)).is_empty());
}

#[test]
fn release_is_debounced_and_resume_cancels_end_timer() {
    let mut d = detector(
        "Teams.exe",
        &[true, true, false, false, true, false, false, false, false],
        DetectConfig::default(),
    );
    assert!(d.poll(at(0)).is_empty());
    assert!(started(&d.poll(at(5))));
    for t in [6, 25, 26, 27, 46] {
        assert!(d.poll(at(t)).is_empty());
    }
    assert_eq!(
        d.poll(at(47)),
        vec![DetectEvent::CallEnded {
            app: "Teams".into(),
            pid: 7
        }]
    );
    assert!(d.poll(at(100)).is_empty());
}

#[test]
fn dismissal_reoffers_a_continuing_call_only_after_cooldown() {
    let mut d = detector("Teams.exe", &[true; 7], DetectConfig::default());
    assert!(d.poll(at(0)).is_empty());
    assert!(started(&d.poll(at(5))));
    d.dismiss(7);
    assert!(d.poll(at(604)).is_empty());
    assert!(started(&d.poll(at(605))));
    assert!(d.poll(at(700)).is_empty());
    d.dismiss(7);
    assert!(d.poll(at(1299)).is_empty());
    assert!(started(&d.poll(at(1300))));
}

#[test]
fn dismissal_survives_separate_calls_in_same_process() {
    let mut d = detector(
        "Teams.exe",
        &[true, true, false, false, true, true, true, true],
        DetectConfig::default(),
    );
    d.poll(at(0));
    assert!(started(&d.poll(at(5))));
    d.dismiss(7);
    assert!(d.poll(at(6)).is_empty());
    assert!(matches!(
        d.poll(at(26)).as_slice(),
        [DetectEvent::CallEnded { .. }]
    ));
    for t in [30, 35, 604] {
        assert!(d.poll(at(t)).is_empty());
    }
    assert!(started(&d.poll(at(605))));
}

#[test]
fn browsers_are_enabled_by_default_and_playback_alone_is_never_a_call() {
    for (exe, config) in [
        (
            "chrome.exe",
            DetectConfig {
                detect_browsers: false,
                ..DetectConfig::default()
            },
        ),
        ("unknown.exe", DetectConfig::default()),
    ] {
        let mut d = detector(exe, &[true, true], config);
        assert!(d.poll(at(0)).is_empty());
        assert!(d.poll(at(60)).is_empty());
    }
    let config = DetectConfig {
        detect_browsers: true,
        ..DetectConfig::default()
    };
    let mut d = detector("chrome.exe", &[false, false, true, true], config);
    for t in [0, 50, 51] {
        assert!(d.poll(at(t)).is_empty());
    }
    assert!(
        matches!(d.poll(at(56)).as_slice(), [DetectEvent::CallStarted { app, .. }] if app == "Chrome")
    );
}

#[test]
fn settings_can_extend_and_override_known_apps() {
    let mut config = DetectConfig::default();
    config.apps.push(notetaker_detect::AppRule {
        exe: "custom.exe".into(),
        name: "Custom".into(),
        browser: false,
    });
    let mut d = detector("custom.exe", &[true, true], config);
    d.poll(at(0));
    assert!(
        matches!(d.poll(at(5)).as_slice(), [DetectEvent::CallStarted { app, .. }] if app == "Custom")
    );
}

#[test]
fn failed_snapshots_do_not_end_a_call_or_count_toward_release() {
    let frames = [
        Some(true),
        Some(true),
        Some(false),
        None,
        Some(false),
        Some(false),
        Some(false),
    ];
    let provider = Fake(
        frames
            .into_iter()
            .map(|mic| {
                mic.map_or_else(
                    || Err(anyhow!("access denied")),
                    |mic| {
                        Ok(Snapshot {
                            processes: vec![observation("Teams.exe", mic)],
                            warnings: vec![],
                        })
                    },
                )
            })
            .collect(),
    );
    let mut d = CallDetector::new(provider, DetectConfig::default());
    d.poll(at(0));
    assert!(started(&d.poll(at(5))));
    for t in [6, 40, 50, 69] {
        assert!(d.poll(at(t)).is_empty());
    }
    assert!(matches!(
        d.poll(at(70)).as_slice(),
        [DetectEvent::CallEnded { .. }]
    ));
}

#[test]
fn pid_reuse_does_not_inherit_a_dismissal() {
    let old = || {
        Ok(Snapshot {
            processes: vec![observation("Teams.exe", true)],
            warnings: vec![],
        })
    };
    let new = || {
        let mut p = observation("Teams.exe", true);
        p.started_at = 200;
        Ok(Snapshot {
            processes: vec![p],
            warnings: vec![],
        })
    };
    let mut d = CallDetector::new(
        Fake(VecDeque::from([old(), old(), new(), new()])),
        DetectConfig::default(),
    );
    d.poll(at(0));
    d.poll(at(5));
    d.dismiss(7);
    assert!(d.poll(at(6)).is_empty());
    assert!(started(&d.poll(at(11))));
}

#[test]
fn a_new_call_during_cooldown_still_needs_five_seconds_of_microphone_use() {
    let mut d = detector(
        "Teams.exe",
        &[true, true, false, false, true, true, true],
        DetectConfig::default(),
    );
    d.poll(at(0));
    d.poll(at(5));
    d.dismiss(7);
    d.poll(at(6));
    d.poll(at(26));
    assert!(d.poll(at(603)).is_empty());
    assert!(d.poll(at(605)).is_empty());
    assert!(started(&d.poll(at(608))));
}

#[test]
fn failed_snapshots_reset_unconfirmed_start_evidence() {
    let frame = || {
        Ok(Snapshot {
            processes: vec![observation("Teams.exe", true)],
            warnings: vec![],
        })
    };
    let mut d = CallDetector::new(
        Fake(VecDeque::from([
            frame(),
            Err(anyhow!("failed")),
            frame(),
            frame(),
        ])),
        DetectConfig::default(),
    );
    assert!(d.poll(at(0)).is_empty());
    assert!(d.poll(at(4)).is_empty());
    assert_eq!(d.take_warnings().len(), 1);
    assert!(d.poll(at(9)).is_empty());
    assert!(started(&d.poll(at(14))));
}

#[test]
fn stopped_recording_reoffers_a_continuing_call_after_cooldown() {
    let mut d = detector("Teams.exe", &[true; 5], DetectConfig::default());
    d.poll(at(0));
    assert!(started(&d.poll(at(5))));
    d.poll(at(100));
    d.recording_stopped();
    assert!(d.poll(at(699)).is_empty());
    assert!(started(&d.poll(at(700))));
}

#[test]
fn browser_toggle_preserves_active_call_and_dismissal() {
    let mut d = detector(
        "chrome.exe",
        &[true, true, true, true, true, false, false],
        DetectConfig::default(),
    );
    d.poll(at(0));
    assert!(matches!(
        d.poll(at(5)).as_slice(),
        [DetectEvent::CallStarted { .. }]
    ));
    d.dismiss(7);
    d.set_detect_browsers(false);
    assert!(d.poll(at(50)).is_empty());
    d.set_detect_browsers(true);
    assert!(d.poll(at(604)).is_empty());
    assert!(matches!(
        d.poll(at(605)).as_slice(),
        [DetectEvent::CallStarted { .. }]
    ));
    d.set_detect_browsers(false);
    assert!(d.poll(at(606)).is_empty());
    assert!(matches!(
        d.poll(at(626)).as_slice(),
        [DetectEvent::CallEnded { .. }]
    ));
}
