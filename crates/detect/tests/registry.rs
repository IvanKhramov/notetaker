use notetaker_detect::{
    Process, RegistryApp, RegistryUsage, registry_snapshot, registry_timestamp,
};
use std::collections::BTreeSet;

fn process(pid: u32) -> Process {
    Process {
        pid,
        parent_pid: None,
        started_at: 1,
        exe: "ms-teams.exe".into(),
        path: Some(r"C:\Apps\ms-teams.exe".into()),
        package_family: Some("MSTeams_8wekyb3d8bbwe".into()),
    }
}
fn entry(app: RegistryApp) -> RegistryUsage {
    RegistryUsage {
        app,
        last_used_start: Some(200),
        last_used_stop: Some(100),
    }
}

#[test]
fn timestamps_are_little_endian_and_missing_or_malformed_data_is_not_active() {
    assert_eq!(registry_timestamp(&200_u64.to_le_bytes()), Some(200));
    assert_eq!(registry_timestamp(&[0; 4]), None);
    assert_eq!(registry_timestamp(&[0; 9]), None);
    for (start, stop, active) in [
        (Some(200), Some(0), true),
        (Some(200), Some(100), true),
        (Some(200), Some(200), false),
        (Some(100), Some(200), false),
        (None, Some(0), false),
        (Some(200), None, false),
        (Some(0), Some(0), false),
    ] {
        let mut usage = entry(RegistryApp::Packaged("test".into()));
        usage.last_used_start = start;
        usage.last_used_stop = stop;
        assert_eq!(usage.is_active(), active);
    }
}

#[test]
fn matches_full_nonpackaged_path_or_exact_package_family() {
    for app in [
        RegistryApp::NonPackaged("c:#apps#MS-TEAMS.EXE".into()),
        RegistryApp::Packaged("msteams_8wekyb3d8bbwe".into()),
    ] {
        let snapshot = registry_snapshot(
            &[entry(app)],
            &[process(1)],
            &BTreeSet::new(),
            &BTreeSet::from([1]),
        );
        assert!(snapshot.processes[0].microphone);
        assert!(snapshot.processes[0].output_active);
    }
    let wrong = entry(RegistryApp::NonPackaged("C:#Other#ms-teams.exe".into()));
    assert!(
        !registry_snapshot(&[wrong], &[process(1)], &BTreeSet::new(), &BTreeSet::new()).processes
            [0]
        .microphone
    );
}

#[test]
fn capture_sessions_disambiguate_owners_and_subprocesses_collapse_to_root() {
    let mut child = process(2);
    child.parent_pid = Some(1);
    let usage = entry(RegistryApp::Packaged("MSTeams_8wekyb3d8bbwe".into()));
    let snapshot = registry_snapshot(
        std::slice::from_ref(&usage),
        &[process(1), child],
        &BTreeSet::from([2]),
        &BTreeSet::from([2]),
    );
    assert_eq!(snapshot.processes.len(), 1);
    assert_eq!(snapshot.processes[0].pid, 1);
    assert!(snapshot.processes[0].microphone && snapshot.processes[0].output_active);
    let snapshot = registry_snapshot(
        std::slice::from_ref(&usage),
        &[process(1), process(2)],
        &BTreeSet::from([2]),
        &BTreeSet::new(),
    );
    assert!(!snapshot.processes[0].microphone);
    assert!(snapshot.processes[1].microphone);
    let snapshot = registry_snapshot(
        &[usage],
        &[process(1), process(2)],
        &BTreeSet::new(),
        &BTreeSet::new(),
    );
    assert!(
        snapshot.processes.iter().all(|p| !p.microphone),
        "ambiguous registry identity cannot establish a PID"
    );
}

#[test]
fn packaged_webview_audio_is_attributed_to_the_teams_parent() {
    let mut webview = process(2);
    webview.parent_pid = Some(1);
    webview.exe = "msedgewebview2.exe".into();
    webview.path = Some(r"C:\WebView2\msedgewebview2.exe".into());
    let snapshot = registry_snapshot(
        &[entry(RegistryApp::Packaged("MSTeams_8wekyb3d8bbwe".into()))],
        &[process(1), webview],
        &BTreeSet::from([2]),
        &BTreeSet::from([2]),
    );
    assert_eq!(snapshot.processes.len(), 1);
    assert_eq!(snapshot.processes[0].exe, "ms-teams.exe");
    assert!(snapshot.processes[0].microphone && snapshot.processes[0].output_active);
}
