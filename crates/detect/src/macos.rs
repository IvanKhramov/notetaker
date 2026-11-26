//! HAL microphone owners plus process ancestry. No window titles or browser tabs.
use anyhow::Result;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

use crate::{Observation, Snapshot, SnapshotProvider};

pub struct MacosSnapshot;
impl SnapshotProvider for MacosSnapshot {
    fn snapshot(&mut self) -> Result<Snapshot> {
        let owners = notetaker_capture::microphone_processes()?;
        let mut system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        anyhow::ensure!(
            system
                .process(sysinfo::Pid::from_u32(std::process::id()))
                .is_some(),
            "process inventory is unavailable"
        );

        let mut microphone = std::collections::BTreeSet::new();
        for pid in owners {
            let owner = sysinfo::Pid::from_u32(pid);
            let mut selected = owner;
            let mut current = Some(owner);
            // Prefer the known app over its microphone-owning helper process.
            for _ in 0..16 {
                let Some(pid) = current else {
                    break;
                };
                let Some(process) = system.process(pid) else {
                    break;
                };
                if crate::KNOWN_APPS
                    .iter()
                    .any(|(exe, _, _)| process.name().to_string_lossy().eq_ignore_ascii_case(exe))
                {
                    selected = pid;
                }
                current = process.parent();
            }
            microphone.insert(selected);
        }
        // Executable paths only for microphone owners: a full scan is costly.
        let owners: Vec<_> = microphone.iter().copied().collect();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&owners),
            false,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        Ok(Snapshot {
            processes: system
                .processes()
                .values()
                .filter(|p| p.pid().as_u32() != std::process::id())
                .map(|p| Observation {
                    pid: p.pid().as_u32(),
                    started_at: p.start_time(),
                    exe: p.name().to_string_lossy().into_owned(),
                    microphone: microphone.contains(&p.pid()),
                    output_active: false,
                    system: p.pid().as_u32() <= 1 || apple_daemon(p.exe()),
                    description: None,
                    call: None,
                })
                .collect(),
            warnings: vec![],
        })
    }
}

/// Siri, dictation and other macOS services hold the microphone without a call.
/// Call apps live in /Applications or the user's folders, never in these.
fn apple_daemon(path: Option<&std::path::Path>) -> bool {
    path.is_some_and(|path| path.starts_with("/System/Library") || path.starts_with("/usr"))
}

#[cfg(test)]
mod tests {
    use super::apple_daemon;
    use std::path::Path;
    #[test]
    fn system_services_are_not_calls_but_apps_are() {
        assert!(apple_daemon(Some(Path::new(
            "/System/Library/PrivateFrameworks/CoreSpeech.framework/corespeechd"
        ))));
        assert!(apple_daemon(Some(Path::new("/usr/libexec/avconferenced"))));
        assert!(!apple_daemon(Some(Path::new(
            "/Applications/zoom.us.app/Contents/MacOS/zoom.us"
        ))));
        assert!(!apple_daemon(Some(Path::new(
            "/Users/a/Applications/Telegram.app"
        ))));
        assert!(!apple_daemon(Some(Path::new("/usrlocal/app"))));
        assert!(!apple_daemon(None));
    }
}
