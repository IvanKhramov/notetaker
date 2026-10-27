use std::collections::BTreeSet;

use crate::{Observation, Snapshot};

#[derive(Clone, Debug)]
pub enum RegistryApp {
    /// Full executable path, with Windows path separators replaced by '#'.
    NonPackaged(String),
    /// Package family name, not executable name or package full name.
    Packaged(String),
}

#[derive(Clone, Debug)]
pub struct RegistryUsage {
    pub app: RegistryApp,
    pub last_used_start: Option<u64>,
    pub last_used_stop: Option<u64>,
}

impl RegistryUsage {
    /// ConsentStore FILETIMEs: zero stop is usual during capture. Missing or
    /// malformed timestamps are unknown, never positive evidence of capture.
    pub fn is_active(&self) -> bool {
        matches!((self.last_used_start, self.last_used_stop), (Some(start), Some(stop)) if start > stop)
    }
}

/// Both REG_QWORD and legacy REG_BINARY timestamps contain 8 little-endian bytes.
pub fn registry_timestamp(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub started_at: u64,
    pub exe: String,
    pub path: Option<String>,
    pub package_family: Option<String>,
}

/// Match registry identity exactly. Where several processes share it, capture
/// sessions disambiguate the microphone owner. Collapse same-exe/package subprocesses
/// (Chromium/Firefox audio services) to their running application root.
pub fn registry_snapshot(
    usage: &[RegistryUsage],
    processes: &[Process],
    capture_pids: &BTreeSet<u32>,
    output_pids: &BTreeSet<u32>,
) -> Snapshot {
    let root = |process: &Process| {
        let mut current = process;
        let mut seen = BTreeSet::new();
        while seen.insert(current.pid) {
            let Some(parent) = processes.iter().find(|p| {
                Some(p.pid) == current.parent_pid
                    && (p.exe.eq_ignore_ascii_case(&current.exe)
                        || p.package_family.as_ref().is_some_and(|family| {
                            current
                                .package_family
                                .as_ref()
                                .is_some_and(|f| family.eq_ignore_ascii_case(f))
                        }))
            }) else {
                break;
            };
            current = parent;
        }
        current.pid
    };
    let mut microphone = BTreeSet::new();
    for entry in usage.iter().filter(|entry| entry.is_active()) {
        let candidates: Vec<_> = processes
            .iter()
            .filter(|p| match &entry.app {
                RegistryApp::NonPackaged(encoded) => p
                    .path
                    .as_ref()
                    .is_some_and(|path| path.eq_ignore_ascii_case(&encoded.replace('#', "\\"))),
                RegistryApp::Packaged(family) => p
                    .package_family
                    .as_ref()
                    .is_some_and(|f| f.eq_ignore_ascii_case(family)),
            })
            .collect();
        let owners: Vec<_> = candidates
            .iter()
            .filter(|p| capture_pids.contains(&p.pid))
            .collect();
        if !owners.is_empty() {
            microphone.extend(owners.iter().map(|p| root(p)));
        } else {
            let roots: BTreeSet<_> = candidates.iter().map(|p| root(p)).collect();
            if roots.len() == 1 {
                microphone.extend(roots);
            }
        }
    }
    let output: BTreeSet<_> = processes
        .iter()
        .filter(|p| output_pids.contains(&p.pid))
        .map(root)
        .collect();
    Snapshot {
        processes: processes
            .iter()
            .filter(|p| root(p) == p.pid)
            .map(|p| Observation {
                pid: p.pid,
                started_at: p.started_at,
                exe: p.exe.clone(),
                microphone: microphone.contains(&p.pid),
                output_active: output.contains(&p.pid),
                system: false,
                description: None,
                call: None,
            })
            .collect(),
        warnings: vec![],
    }
}

/// Include descendants with different executable names (for example WebView2).
/// Creation times prevent a reused parent PID from swallowing an unrelated process.
pub fn process_tree_pids(pid: u32, processes: &[Process]) -> BTreeSet<u32> {
    let mut tree = BTreeSet::from([pid]);
    loop {
        let before = tree.len();
        for process in processes {
            if processes.iter().any(|parent| {
                Some(parent.pid) == process.parent_pid
                    && tree.contains(&parent.pid)
                    && parent.started_at <= process.started_at
            }) {
                tree.insert(process.pid);
            }
        }
        if tree.len() == before {
            return tree;
        }
    }
}
