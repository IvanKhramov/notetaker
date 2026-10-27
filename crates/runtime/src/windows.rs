//! Small Win32 boundary. Handles and COM initialization are owned here.
#![allow(unsafe_code)]
use anyhow::{Result, ensure};
use std::{os::windows::ffi::OsStrExt, path::Path, time::Duration};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER,
            GetLastError, HANDLE, WAIT_OBJECT_0,
        },
        System::{
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
                RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW,
                RegSetValueExW,
            },
            Threading::{
                CreateEventW, CreateMutexW, OpenEventW, OpenMutexW, OpenProcess,
                PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
            },
        },
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow, SW_SHOWNORMAL},
        },
    },
    core::PCWSTR,
};

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn object_name(kind: &str) -> Vec<u16> {
    // Fixed FNV-1a, independent of Rust version/random seeds. Local namespace
    // scopes it to the interactive session; profile scopes it to the user.
    let profile = std::env::var_os("USERPROFILE").unwrap_or_default();
    let hash = profile
        .to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
    wide(format!("Local\\notetaker-{hash:016x}-{kind}"))
}

pub struct Instance {
    _mutex: Handle,
    _event: Handle,
    _quit: Handle,
}
impl Instance {
    /// The event is created before the mutex, so even a second launch during
    /// GUI initialization can queue a restore request without a startup race.
    pub fn acquire() -> Result<Option<Self>> {
        unsafe {
            let event = Handle(CreateEventW(
                None,
                false,
                false,
                PCWSTR(object_name("show").as_ptr()),
            )?);
            let quit = Handle(CreateEventW(
                None,
                false,
                false,
                PCWSTR(object_name("quit").as_ptr()),
            )?);
            let name = object_name("instance");
            let mutex = CreateMutexW(None, false, PCWSTR(name.as_ptr()))?;
            // Read immediately, before even freeing a temporary name buffer.
            let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
            let mutex = Handle(mutex);
            if already_running {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                SetEvent(event.0)?;
                return Ok(None);
            }
            Ok(Some(Self {
                _mutex: mutex,
                _event: event,
                _quit: quit,
            }))
        }
    }
    pub fn show_requested() -> bool {
        Self::requested("show")
    }
    pub fn quit_requested() -> bool {
        Self::requested("quit")
    }
    fn requested(kind: &str) -> bool {
        unsafe {
            OpenEventW(
                SYNCHRONIZATION_SYNCHRONIZE,
                false,
                PCWSTR(object_name(kind).as_ptr()),
            )
            .map(|handle| {
                let handle = Handle(handle);
                WaitForSingleObject(handle.0, 0) == WAIT_OBJECT_0
            })
            .unwrap_or(false)
        }
    }
    pub fn refuse_quit() {
        unsafe {
            if let Ok(event) = CreateEventW(
                None,
                false,
                false,
                PCWSTR(object_name("quit-denied").as_ptr()),
            ) {
                let event = Handle(event);
                let _ = SetEvent(event.0);
            }
        }
    }
    /// Exit codes: 0 stopped/absent, 2 busy/timeout/error. Installers must not
    /// proceed on nonzero. Never start a GUI instance for a --quit request.
    pub fn request_quit() -> Result<()> {
        unsafe {
            if !Self::running()? {
                return Ok(());
            }
            let denied = Handle(CreateEventW(
                None,
                false,
                false,
                PCWSTR(object_name("quit-denied").as_ptr()),
            )?);
            let _ = WaitForSingleObject(denied.0, 0); // clear an earlier denial
            let event = Handle(CreateEventW(
                None,
                false,
                false,
                PCWSTR(object_name("quit").as_ptr()),
            )?);
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            SetEvent(event.0)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(120);
            loop {
                ensure!(
                    WaitForSingleObject(denied.0, 0) != WAIT_OBJECT_0,
                    "Приложение занято записью или чистовым проходом"
                );
                if !Self::running()? {
                    return Ok(());
                }
                ensure!(
                    std::time::Instant::now() < deadline,
                    "Приложение не завершилось"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    fn running() -> Result<bool> {
        unsafe {
            match OpenMutexW(
                SYNCHRONIZATION_SYNCHRONIZE,
                false,
                PCWSTR(object_name("instance").as_ptr()),
            ) {
                Ok(handle) => {
                    drop(Handle(handle));
                    Ok(true)
                }
                Err(_) if GetLastError() == ERROR_FILE_NOT_FOUND => Ok(false),
                Err(error) => Err(error.into()),
            }
        }
    }
}

pub fn wait_for_exit(pid: u32, timeout: Duration) -> Result<()> {
    unsafe {
        let handle = match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(handle) => Handle(handle),
            Err(_) if GetLastError() == ERROR_INVALID_PARAMETER => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            WaitForSingleObject(handle.0, timeout.as_millis() as u32) == WAIT_OBJECT_0,
            "Приложение не завершилось, обновление отменено"
        );
    }
    Ok(())
}

/// GUI-subsystem processes have no console stderr. Install a pipe for Rust's
/// stderr/logging output and drain it through the same content filter as ASR.
pub fn capture_app_stderr() -> Result<()> {
    use std::{fs::File, os::windows::io::FromRawHandle, sync::OnceLock};
    use windows::Win32::System::{
        Console::{STD_ERROR_HANDLE, SetStdHandle},
        Pipes::CreatePipe,
    };
    static STDERR: OnceLock<File> = OnceLock::new();
    if STDERR.get().is_some() {
        return Ok(());
    }
    unsafe {
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        CreatePipe(&mut read, &mut write, None, 0)?;
        let reader = File::from_raw_handle(read.0);
        let writer = File::from_raw_handle(write.0);
        SetStdHandle(STD_ERROR_HANDLE, write)?;
        let _ = STDERR.set(writer);
        crate::diagnostics::capture_app_stderr(reader);
    }
    Ok(())
}

pub fn open(path: &Path) -> Result<()> {
    shell_open(path.as_os_str())
}
pub fn open_url(url: &str) -> Result<()> {
    shell_open(std::ffi::OsStr::new(url))
}
fn shell_open(value: &std::ffi::OsStr) -> Result<()> {
    unsafe {
        let result = ShellExecuteW(
            None,
            windows::core::w!("open"),
            PCWSTR(wide(value).as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
        ensure!(result.0 as isize > 32, "Не удалось открыть файл или ссылку");
    }
    Ok(())
}
pub fn reveal(path: &Path) -> Result<()> {
    std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()?;
    Ok(())
}

const RUN: PCWSTR = windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = windows::core::w!("notetaker");
pub fn autostart() -> bool {
    let mut size = 0;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
        .is_ok()
    }
}
pub fn set_autostart(enabled: bool) -> Result<()> {
    let bytes = if enabled {
        wide(format!("\"{}\"", crate::paths::executable()?.display()))
            .iter()
            .flat_map(|c| c.to_le_bytes())
            .collect::<Vec<u8>>()
    } else {
        Vec::new()
    };
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let result = if enabled {
            RegSetValueExW(key, VALUE, None, REG_SZ, Some(&bytes))
        } else {
            RegDeleteValueW(key, VALUE)
        };
        let _ = RegCloseKey(key);
        if result != ERROR_FILE_NOT_FOUND {
            result.ok()?;
        }
    }
    Ok(())
}
