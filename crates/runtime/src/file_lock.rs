//! Persistent lock inodes shared by all writers of an on-disk resource.
use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    path::Path,
    time::{Duration, Instant},
};

pub fn exclusive(path: &Path, timeout: Duration) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Истекло время ожидания блокировки {}", path.display()),
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}
