use std::{collections::BTreeSet, io};

use anyhow::{Context, Result};
use notetaker_capture::Channel;
use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
        Storage::Packaging::Appx::GetPackageFamilyName,
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    },
    core::PWSTR,
};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, REG_BINARY, REG_QWORD},
};

use crate::{
    Process, RegistryApp, RegistryUsage, Snapshot, SnapshotProvider, registry_snapshot,
    registry_timestamp,
};

const MICROPHONE: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// Read-only snapshot for the current interactive user. No microphone is opened.
#[derive(Default)]
pub struct WindowsSnapshot;

impl SnapshotProvider for WindowsSnapshot {
    fn snapshot(&mut self) -> Result<Snapshot> {
        let usage = read_usage().context("read microphone ConsentStore")?;
        let inventory = notetaker_capture::inventory().context("audio session inventory")?;
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet)),
        );
        let need_packages = usage
            .iter()
            .any(|u| u.is_active() && matches!(u.app, RegistryApp::Packaged(_)));
        let processes: Vec<_> = system
            .processes()
            .values()
            .map(|p| Process {
                pid: p.pid().as_u32(),
                parent_pid: p.parent().map(|p| p.as_u32()),
                started_at: p.start_time(),
                exe: p.name().to_string_lossy().into_owned(),
                path: p.exe().map(|path| path.to_string_lossy().into_owned()),
                package_family: need_packages
                    .then(|| package_family(p.pid().as_u32()))
                    .flatten(),
            })
            .collect();
        let active_pids = |channel| -> BTreeSet<u32> {
            inventory
                .sessions
                .iter()
                .filter(|session| {
                    session.active
                        && inventory
                            .devices
                            .iter()
                            .any(|d| d.id == session.device_id && d.channel == channel)
                })
                .map(|s| s.pid)
                .collect()
        };
        let mut snapshot = registry_snapshot(
            &usage,
            &processes,
            &active_pids(Channel::Mic),
            &active_pids(Channel::System),
        );
        snapshot.warnings = inventory.warnings;
        Ok(snapshot)
    }
}

fn read_usage() -> io::Result<Vec<RegistryUsage>> {
    let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(MICROPHONE) {
        Ok(key) => key,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error),
    };
    let mut entries = vec![];
    for name in key.enum_keys() {
        let name = name?;
        let child = key.open_subkey(&name)?;
        if name.eq_ignore_ascii_case("NonPackaged") {
            for path in child.enum_keys() {
                let path = path?;
                collect_usage(
                    &child.open_subkey(&path)?,
                    RegistryApp::NonPackaged(path),
                    0,
                    &mut entries,
                )?;
            }
        } else {
            collect_usage(&child, RegistryApp::Packaged(name), 0, &mut entries)?;
        }
    }
    Ok(entries)
}

fn collect_usage(
    key: &RegKey,
    app: RegistryApp,
    depth: usize,
    entries: &mut Vec<RegistryUsage>,
) -> io::Result<()> {
    entries.push(RegistryUsage {
        app: app.clone(),
        last_used_start: timestamp(key, "LastUsedTimeStart")?,
        last_used_stop: timestamp(key, "LastUsedTimeStop")?,
    });
    // Packaged apps may keep per-instance usage beneath the package family key.
    if depth < 8 {
        for name in key.enum_keys() {
            collect_usage(&key.open_subkey(name?)?, app.clone(), depth + 1, entries)?;
        }
    }
    Ok(())
}

fn timestamp(key: &RegKey, name: &str) -> io::Result<Option<u64>> {
    match key.get_raw_value(name) {
        Ok(value) if matches!(value.vtype, REG_QWORD | REG_BINARY) => {
            Ok(registry_timestamp(&value.bytes))
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[allow(unsafe_code)]
fn package_family(pid: u32) -> Option<String> {
    // SAFETY: read-only process query; the returned owned handle is closed below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut len = 0;
    // SAFETY: len is writable; a null buffer asks Windows for the required size.
    let status = unsafe { GetPackageFamilyName(handle, &mut len, None) };
    let result = if status == ERROR_INSUFFICIENT_BUFFER && (1..=1024).contains(&len) {
        let mut buffer = vec![0u16; len as usize];
        // SAFETY: buffer has len UTF-16 elements and lives through the call.
        let status =
            unsafe { GetPackageFamilyName(handle, &mut len, Some(PWSTR(buffer.as_mut_ptr()))) };
        (status == ERROR_SUCCESS).then(|| {
            let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            String::from_utf16_lossy(&buffer[..end])
        })
    } else {
        None
    };
    // SAFETY: this function owns the successfully opened handle; close exactly once.
    let _ = unsafe { CloseHandle(handle) };
    result
}
