use std::{collections::BTreeSet, ffi::c_void, io};

use anyhow::{Context, Result};
use notetaker_capture::Channel;
use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND, LPARAM},
        Storage::{
            FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
            Packaging::Appx::GetPackageFamilyName,
        },
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
        },
    },
    core::{BOOL, PCWSTR, PWSTR, w},
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
        let windows_dir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let windows_prefix = format!("{}\\", windows_dir.trim_end_matches('\\').to_lowercase());
        for observed in &mut snapshot.processes {
            let Some(process) = processes.iter().find(|p| p.pid == observed.pid) else {
                continue;
            };
            observed.system = process.pid == std::process::id()
                || process.pid <= 4
                || process
                    .path
                    .as_ref()
                    .is_some_and(|path| path.to_lowercase().starts_with(&windows_prefix));
            if !observed.microphone || observed.system || crate::denied_exe(&observed.exe) {
                continue;
            }
            if !crate::KNOWN_APPS
                .iter()
                .any(|(exe, _, _)| exe.eq_ignore_ascii_case(&observed.exe))
            {
                observed.description = process.path.as_deref().and_then(file_description);
            }
            let pids = crate::process_tree_pids(process.pid, &processes);
            match window_titles(&pids) {
                Ok(titles) => {
                    observed.call = crate::identify_call(titles.iter().map(String::as_str))
                }
                Err(error) => snapshot.warnings.push(format!("window titles: {error}")),
            }
        }
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

#[allow(unsafe_code)]
fn window_titles(pids: &BTreeSet<u32>) -> windows::core::Result<Vec<String>> {
    struct Titles<'a> {
        pids: &'a BTreeSet<u32>,
        titles: Vec<String>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: EnumWindows calls synchronously with our live, exclusively borrowed
        // Titles context. Windows supplies a valid window handle; failures yield 0.
        unsafe {
            let context = &mut *(data.0 as *mut Titles<'_>);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if context.pids.contains(&pid) && IsWindowVisible(hwnd).as_bool() {
                let mut text = [0u16; 2048];
                let len = GetWindowTextW(hwnd, &mut text);
                if len > 0 {
                    context
                        .titles
                        .push(String::from_utf16_lossy(&text[..len as usize]));
                }
            }
        }
        BOOL(1)
    }
    let mut context = Titles {
        pids,
        titles: vec![],
    };
    // SAFETY: visit does not retain this pointer; context outlives enumeration.
    unsafe {
        EnumWindows(
            Some(visit),
            LPARAM(&mut context as *mut Titles<'_> as isize),
        )?
    };
    Ok(context.titles)
}

#[allow(unsafe_code)]
fn file_description(path: &str) -> Option<String> {
    let path: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: path is NUL-terminated; the bounded, aligned version buffer stays live
    // throughout all VerQueryValue calls. query checks returned pointer/length bounds
    // and alignment before constructing a slice. No returned pointer escapes.
    unsafe {
        let len = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
        if !(1..=1024 * 1024).contains(&len) {
            return None;
        }
        let mut data = vec![0u64; (len as usize).div_ceil(8)];
        GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, len, data.as_mut_ptr().cast()).ok()?;
        let query = |key: PCWSTR, bytes: bool| -> Option<&[u16]> {
            let mut value: *mut c_void = std::ptr::null_mut();
            let mut size = 0;
            if !VerQueryValueW(data.as_ptr().cast(), key, &mut value, &mut size).as_bool() {
                return None;
            }
            let units = if bytes {
                size as usize / 2
            } else {
                size as usize
            };
            let start = data.as_ptr() as usize;
            let address = value as usize;
            let end = address.checked_add(units.checked_mul(2)?)?;
            if address < start || end > start + len as usize || !address.is_multiple_of(2) {
                return None;
            }
            Some(std::slice::from_raw_parts(value.cast(), units))
        };
        let translations = query(w!("\\VarFileInfo\\Translation"), true)?;
        for [language, codepage] in translations.as_chunks::<2>().0.iter().take(16) {
            let key: Vec<_> =
                format!(r"\StringFileInfo\{language:04x}{codepage:04x}\FileDescription")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
            if let Some(value) = query(PCWSTR(key.as_ptr()), false) {
                let end = value.iter().position(|&c| c == 0).unwrap_or(value.len());
                let description = String::from_utf16_lossy(&value[..end]).trim().to_owned();
                if !description.is_empty() {
                    return Some(description);
                }
            }
        }
        None
    }
}
