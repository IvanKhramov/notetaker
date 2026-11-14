//! Recording lifetime guards. Linux intentionally does not inhibit sleep.
use std::{io, path::Path};

pub const MIN_FREE_BYTES: u64 = 500 * 1024 * 1024;

#[cfg(unix)]
pub fn available_space(path: &Path) -> io::Result<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: both pointers are valid and statvfs initializes stats on success.
    #[allow(unsafe_code)]
    unsafe {
        if libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let stats = stats.assume_init();
        #[allow(clippy::unnecessary_cast)]
        Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
    }
}

#[cfg(windows)]
pub fn available_space(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::PCWSTR};
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0;
    // SAFETY: path is terminated and free is a valid output pointer.
    #[allow(unsafe_code)]
    unsafe {
        GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut free), None, None)
            .map_err(io::Error::other)?;
    }
    Ok(free)
}

/// Refuses only when the volume is known to be nearly full: a failed query
/// must not block a recording.
pub fn check_space(path: &Path) -> io::Result<()> {
    if available_space(path).is_ok_and(|free| free < MIN_FREE_BYTES) {
        return Err(io::Error::other(
            "Недостаточно места для записи. Освободите хотя бы 500 МБ в папке библиотеки.",
        ));
    }
    Ok(())
}

pub struct SleepGuard {
    #[cfg(target_os = "macos")]
    child: std::process::Child,
    // Windows execution state belongs to the thread that acquired it.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl SleepGuard {
    pub fn acquire() -> io::Result<Self> {
        #[cfg(windows)]
        {
            use windows::Win32::System::Power::{
                ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
            };
            // SAFETY: flags have no pointer arguments and apply to this thread.
            #[allow(unsafe_code)]
            if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) }.0 == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            #[cfg(target_os = "macos")]
            child: std::process::Command::new("/usr/bin/caffeinate")
                .args(["-i", "-w", &std::process::id().to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
            _thread: std::marker::PhantomData,
        })
    }
}
impl Drop for SleepGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: clears the current recording thread's execution requirement.
            #[allow(unsafe_code)]
            unsafe {
                windows::Win32::System::Power::SetThreadExecutionState(
                    windows::Win32::System::Power::ES_CONTINUOUS,
                );
            }
        }
        #[cfg(target_os = "macos")]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_space_query_does_not_block_recording() {
        let dir = tempfile::tempdir().unwrap();
        assert!(super::available_space(dir.path()).unwrap() > 0);
        assert!(super::available_space(&dir.path().join("missing")).is_err());
        assert!(super::check_space(&dir.path().join("missing")).is_ok());
    }
}
