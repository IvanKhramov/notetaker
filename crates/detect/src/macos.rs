//! HAL microphone owners plus process ancestry. No window titles or browser tabs.
use anyhow::Result;
use sysinfo::{ProcessRefreshKind, RefreshKind, System};

use crate::{Observation, Snapshot, SnapshotProvider};

pub struct MacosSnapshot;
impl SnapshotProvider for MacosSnapshot {
    fn snapshot(&mut self) -> Result<Snapshot> {
        let owners = notetaker_capture::microphone_processes()?;
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
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
                    system: p.pid().as_u32() <= 1,
                    description: None,
                    call: None,
                })
                .collect(),
            warnings: vec![],
        })
    }
}
