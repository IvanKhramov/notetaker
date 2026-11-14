use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{MeetingMeta, Result, Transcript, migrate_transcript};

/// Content revision, including whether the file existed when loaded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Revision(Option<[u8; 32]>);
impl Revision {
    fn of(bytes: Option<&[u8]>) -> Self {
        Self(bytes.map(|bytes| Sha256::digest(bytes).into()))
    }
}

#[derive(Debug, Clone)]
pub struct Versioned<T> {
    pub value: T,
    pub revision: Revision,
}

// Handles obtained through different Library instances must use the same lock.
// Weak entries avoid keeping a mutex alive after its last operation completes.
pub(crate) fn meeting_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}

fn lock_unsupported(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(windows)]
    if error.raw_os_error() == Some(1) {
        // ERROR_INVALID_FUNCTION
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error().is_some_and(|code| {
        [libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS, libc::ENOLCK].contains(&code)
    }) {
        return true;
    }
    false
}

// Keep this inode permanently: removing/replacing it would split lock holders.
// Only known content names/extensions are indexed, exported or reconciled.
pub(crate) fn lock_meeting_files(path: &Path) -> Result<Option<fs::File>> {
    lock_meeting_files_for(path, std::time::Duration::from_secs(5))
}

fn lock_meeting_files_for(path: &Path, timeout: std::time::Duration) -> Result<Option<fs::File>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".notetaker.lock"))?;
    let started = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "meeting lock timed out after {timeout:?}: {}",
                        path.display()
                    ),
                )
                .into());
            }
            Err(fs::TryLockError::Error(error)) if lock_unsupported(&error) => {
                tracing::warn!(path = %path.display(), %error,
                    "file locking unsupported; using in-process meeting lock only");
                // CLI has no tracing subscriber; the GUI captures stderr in its log.
                eprintln!("warning: file locking unsupported; using in-process meeting lock only");
                return Ok(None);
            }
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

/// A library root. All returned meeting paths are absolute.
#[derive(Debug, Clone)]
pub struct Library {
    pub(crate) root: PathBuf,
}

/// A meeting directory; metadata is read afresh to respect external edits.
#[derive(Debug, Clone)]
pub struct Meeting {
    pub(crate) path: PathBuf,
}

impl Library {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Meeting folders whose `meta.yaml` exists but cannot be read, for example after
    /// a sync conflict. `list_meetings` skips them, so the caller reports them.
    pub fn unreadable_meetings(&self) -> Result<Vec<PathBuf>> {
        let mut unreadable = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".notetaker" || name == ".trash" || !entry.file_type()?.is_dir() {
                continue;
            }
            let meeting = Meeting { path: entry.path() };
            if meeting.path.join("meta.yaml").is_file() && meeting.read_meta().is_err() {
                unreadable.push(meeting.path);
            }
        }
        unreadable.sort();
        Ok(unreadable)
    }

    /// Moves a meeting out of the library into `<library>/.trash`. Nothing is erased:
    /// the folder stays there, with its audio, until someone removes it by hand.
    pub fn trash_meeting(&self, meeting: &Meeting) -> Result<PathBuf> {
        let name = meeting
            .path
            .file_name()
            .filter(|_| meeting.path.parent() == Some(self.root.as_path()))
            .ok_or_else(|| std::io::Error::other("meeting is not in this library"))?;
        let trash = self.root.join(".trash");
        fs::create_dir_all(&trash)?;
        let mut target = trash.join(name);
        if target.exists() {
            let mut unique = name.to_owned();
            unique.push(format!("-{}", Utc::now().timestamp_millis()));
            target = trash.join(unique);
        }
        fs::rename(&meeting.path, &target)?;
        Ok(target)
    }

    /// Metadata is published last so incomplete creations are skipped by listing.
    pub fn create_meeting(&self, meta: MeetingMeta) -> Result<Meeting> {
        let base = format!(
            "{}_{}",
            meta.started_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d_%H%M"),
            slug(&meta.title)
        );
        let mut suffix = 0u64;
        let path = loop {
            let name = if suffix == 0 {
                base.clone()
            } else {
                format!("{base}-{suffix}")
            };
            let path = self.root.join(name);
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
                Err(e) => return Err(e.into()),
            }
        };
        let meeting = Meeting { path };
        fs::create_dir(meeting.path.join("audio"))?;
        meeting.write_notes("")?;
        meeting.write_enhanced("")?;
        meeting.write_transcript(&Transcript::default())?;
        meeting.write_meta(&meta)?;
        Ok(meeting)
    }

    /// Skip incomplete/corrupt meeting directories, warning with their path.
    pub fn list_meetings(&self) -> Result<Vec<Meeting>> {
        let mut meetings = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".notetaker" || name == ".trash" || !entry.file_type()?.is_dir() {
                continue;
            }
            let meeting = Meeting { path: entry.path() };
            match meeting.read_meta() {
                Ok(_) => meetings.push(meeting),
                Err(error) => {
                    tracing::warn!(path = %meeting.path.display(), %error, "skipping invalid meeting")
                }
            }
        }
        meetings.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(meetings)
    }

    /// Remove expired audio, failed final-pass scratch and old attachment trash.
    /// Attachment retention is independent of audio and active recordings.
    /// Audio in meetings still being captured must never be swept.
    pub fn sweep_audio(&self, now: DateTime<Utc>) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        for meeting in self.list_meetings()? {
            removed.extend(meeting.purge_attachment_trash(now)?);
            let meta = meeting.read_meta()?;
            if meta.ended_at.is_none()
                || !meta.audio_expires_at.is_some_and(|expires| expires <= now)
            {
                continue;
            }
            let audio = meeting.path.join("audio");
            let metadata = match fs::symlink_metadata(&audio) {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            for entry in fs::read_dir(audio)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    if entry.file_name() == ".work" {
                        fs::remove_dir_all(entry.path())?;
                        removed.push(entry.path());
                    }
                    continue;
                }
                let path = entry.path();
                match fs::remove_file(&path) {
                    Ok(()) => removed.push(path),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        removed.sort();
        Ok(removed)
    }
}

impl Meeting {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve each existing source inside this meeting, including symlinks and
    /// Windows reparse points. Return the resolved path so reads use that target.
    pub(crate) fn source_path(&self, name: &str) -> Result<PathBuf> {
        let path = self.path.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(path),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let resolved = path.canonicalize()?;
        if !resolved.starts_with(self.path.canonicalize()?) || !resolved.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "meeting source must stay inside its meeting folder",
            )
            .into());
        }
        Ok(resolved)
    }

    pub fn validate_source_paths(&self) -> Result<()> {
        for name in ["meta.yaml", "notes.md", "enhanced.md", "transcript.json"] {
            self.source_path(name)?;
        }
        Ok(())
    }

    pub fn read_meta(&self) -> Result<MeetingMeta> {
        Ok(serde_yaml::from_str(&fs::read_to_string(
            self.source_path("meta.yaml")?,
        )?)?)
    }

    pub fn update_meta(&self, update: impl FnOnce(&mut MeetingMeta)) -> Result<()> {
        self.update_meta_with_timeout(std::time::Duration::from_secs(5), update)
    }

    /// Longer bounded contention budget for recording finalization.
    pub fn update_meta_with_timeout(
        &self,
        timeout: std::time::Duration,
        update: impl FnOnce(&mut MeetingMeta),
    ) -> Result<()> {
        let started = std::time::Instant::now();
        let lock = meeting_lock(&self.path);
        let _guard = loop {
            match lock.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if started.elapsed() < timeout => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "meeting metadata mutex timed out",
                    )
                    .into());
                }
            }
        };
        let _file_guard =
            lock_meeting_files_for(&self.path, timeout.saturating_sub(started.elapsed()))?;
        let loaded = load_text(&self.source_path("meta.yaml")?)?;
        let mut meta: MeetingMeta = serde_yaml::from_str(&loaded.value)?;
        update(&mut meta);
        checked_write(
            &self.path.join("meta.yaml"),
            serde_yaml::to_string(&meta)?.as_bytes(),
            loaded.revision,
        )?;
        Ok(())
    }

    pub fn write_meta(&self, meta: &MeetingMeta) -> Result<()> {
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = lock_meeting_files(&self.path)?;
        atomic_write(
            &self.path.join("meta.yaml"),
            serde_yaml::to_string(meta)?.as_bytes(),
        )
    }

    pub fn read_notes(&self) -> Result<String> {
        read_optional(&self.source_path("notes.md")?)
    }
    pub fn write_notes(&self, notes: &str) -> Result<()> {
        let prepared = PreparedWrite::new(&self.path.join("notes.md"), notes.as_bytes())?;
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = crate::storage::lock_meeting_files(&self.path)?;
        prepared.persist(&self.path.join("notes.md"))?;
        self.clean_attachments_after_save();
        Ok(())
    }
    pub fn read_enhanced(&self) -> Result<String> {
        read_optional(&self.source_path("enhanced.md")?)
    }
    pub fn write_enhanced(&self, enhanced: &str) -> Result<()> {
        let prepared = PreparedWrite::new(&self.path.join("enhanced.md"), enhanced.as_bytes())?;
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = crate::storage::lock_meeting_files(&self.path)?;
        prepared.persist(&self.path.join("enhanced.md"))?;
        self.clean_attachments_after_save();
        Ok(())
    }

    pub fn read_transcript(&self) -> Result<Transcript> {
        match fs::read_to_string(self.source_path("transcript.json")?) {
            Ok(text) => migrate_transcript(serde_json::from_str(&text)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Transcript::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn write_transcript(&self, transcript: &Transcript) -> Result<()> {
        if transcript.version != crate::SCHEMA_VERSION {
            return Err(crate::Error::UnsupportedVersion(transcript.version));
        }
        let path = self.path.join("transcript.json");
        let prepared = PreparedWrite::new(&path, &serde_json::to_vec_pretty(transcript)?)?;
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = lock_meeting_files(&self.path)?;
        prepared.persist(&path)
    }

    pub fn load_notes(&self) -> Result<Versioned<String>> {
        load_text(&self.source_path("notes.md")?)
    }
    pub fn load_enhanced(&self) -> Result<Versioned<String>> {
        load_text(&self.source_path("enhanced.md")?)
    }
    pub fn load_transcript(&self) -> Result<Versioned<Transcript>> {
        let loaded = load_text(&self.source_path("transcript.json")?)?;
        Ok(Versioned {
            value: if loaded.revision.0.is_none() {
                Transcript::default()
            } else {
                migrate_transcript(serde_json::from_str(&loaded.value)?)?
            },
            revision: loaded.revision,
        })
    }
    pub fn save_notes(&self, text: &str, expected: Revision) -> Result<Revision> {
        self.save_checked("notes.md", text.as_bytes(), expected)
    }
    pub fn save_enhanced(&self, text: &str, expected: Revision) -> Result<Revision> {
        self.save_checked("enhanced.md", text.as_bytes(), expected)
    }
    pub fn save_transcript(&self, transcript: &Transcript, expected: Revision) -> Result<Revision> {
        if transcript.version != crate::SCHEMA_VERSION {
            return Err(crate::Error::UnsupportedVersion(transcript.version));
        }
        self.save_checked(
            "transcript.json",
            &serde_json::to_vec_pretty(transcript)?,
            expected,
        )
    }
    fn save_checked(&self, name: &str, bytes: &[u8], expected: Revision) -> Result<Revision> {
        let prepared = PreparedWrite::new(&self.path.join(name), bytes)?;
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = crate::storage::lock_meeting_files(&self.path)?;
        let revision = prepared.checked_persist(&self.path.join(name), bytes, expected)?;
        if matches!(name, "notes.md" | "enhanced.md") {
            self.clean_attachments_after_save();
        }
        Ok(revision)
    }
    /// Serialize local transcript edits; concurrent process edits are revision-checked.
    /// Stage large transcript writes without holding the meeting file lock.
    pub fn update_transcript(&self, update: impl FnOnce(&mut Transcript)) -> Result<()> {
        let transcript_lock = meeting_lock(&self.path.join("transcript.json"));
        let _transcript_guard = transcript_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut loaded = {
            let lock = meeting_lock(&self.path);
            let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let _file_guard = lock_meeting_files(&self.path)?;
            self.load_transcript()?
        };
        update(&mut loaded.value);
        self.save_transcript(&loaded.value, loaded.revision)?;
        Ok(())
    }

    pub fn mic_audio_path(&self) -> PathBuf {
        self.path.join("audio/mic.opus")
    }
    pub fn system_audio_path(&self) -> PathBuf {
        self.path.join("audio/system.opus")
    }
}

fn read_optional(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) struct PreparedWrite(NamedTempFile);
impl PreparedWrite {
    pub(crate) fn new(path: &Path, bytes: &[u8]) -> Result<Self> {
        let mut temporary =
            NamedTempFile::new_in(path.parent().expect("meeting file has a parent"))?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        Ok(Self(temporary))
    }
    pub(crate) fn checked_persist(
        self,
        path: &Path,
        bytes: &[u8],
        expected: Revision,
    ) -> Result<Revision> {
        if Revision::of(read_bytes(path)?.as_deref()) != expected {
            // Preserve conflict handling, but never flush the large file under the lock.
            let local = path.with_file_name(format!(
                "{}.conflict-{}-{}.{}",
                path.file_stem().unwrap().to_string_lossy(),
                DateTime::<Utc>::from(std::time::SystemTime::now()).format("%Y%m%dT%H%M%S%.3fZ"),
                uuid::Uuid::now_v7(),
                path.extension().unwrap().to_string_lossy()
            ));
            self.persist(&local)?;
            return Err(crate::Error::Conflict {
                path: path.to_owned(),
                local,
            });
        }
        self.persist(path)?;
        Ok(Revision::of(Some(bytes)))
    }
    pub(crate) fn persist(self, path: &Path) -> Result<()> {
        let mut temporary = self.0;
        // Sync clients and antivirus briefly hold the target open on Windows; the
        // rename then fails with PermissionDenied and succeeds a moment later.
        let mut attempt = 0;
        loop {
            match temporary.persist(path) {
                Ok(_) => return Ok(()),
                Err(error)
                    if error.error.kind() == std::io::ErrorKind::PermissionDenied
                        && attempt < 4 =>
                {
                    attempt += 1;
                    temporary = error.file;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(error) => return Err(error.error.into()),
            }
        }
    }
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    PreparedWrite::new(path, bytes)?.persist(path)
}

fn slug(title: &str) -> String {
    let mut slug = String::new();
    for ch in deunicode::deunicode(title).chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 64 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "meeting".into()
    } else {
        slug.into()
    }
}

pub(crate) fn load_text(path: &Path) -> Result<Versioned<String>> {
    let bytes = read_bytes(path)?;
    let revision = Revision::of(bytes.as_deref());
    let value = String::from_utf8(bytes.unwrap_or_default())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Versioned { value, revision })
}

pub(crate) fn checked_write(path: &Path, bytes: &[u8], expected: Revision) -> Result<Revision> {
    PreparedWrite::new(path, bytes)?.checked_persist(path, bytes, expected)
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn independent_lock_holder_preserves_rename() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Original", Utc::now()))
            .unwrap();
        // This holder bypasses meeting_lock, like a separate GUI process.
        let holder = lock_meeting_files(meeting.path()).unwrap().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (updated_tx, updated_rx) = std::sync::mpsc::channel();
        let other = meeting.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = other.update_meta(|m| {
                updated_tx.send(()).unwrap();
                m.ended_at = Some(m.started_at);
            });
            result.unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            updated_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
        let mut renamed = meeting.read_meta().unwrap();
        renamed.title = "Renamed".into();
        atomic_write(
            &meeting.path.join("meta.yaml"),
            serde_yaml::to_string(&renamed).unwrap().as_bytes(),
        )
        .unwrap();
        drop(holder);
        worker.join().unwrap();
        let saved = meeting.read_meta().unwrap();
        assert_eq!(saved.title, "Renamed");
        assert_eq!(saved.ended_at, Some(saved.started_at));
        assert_eq!(library.list_meetings().unwrap().len(), 1);
        assert!(meeting.sync_conflicts().unwrap().is_empty());
    }

    #[test]
    fn independent_lock_holder_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let _holder = lock_meeting_files(dir.path()).unwrap().unwrap();
        let error =
            lock_meeting_files_for(dir.path(), std::time::Duration::from_millis(50)).unwrap_err();
        assert!(
            matches!(error, crate::Error::Io(ref e) if e.kind() == std::io::ErrorKind::TimedOut)
        );
    }
}

#[cfg(test)]
mod unsupported_lock_tests {
    use super::lock_unsupported;
    use std::io::{Error, ErrorKind};

    #[test]
    fn unsupported_kind_is_recognized_but_contention_is_not() {
        assert!(lock_unsupported(&Error::from(ErrorKind::Unsupported)));
        for kind in [
            ErrorKind::WouldBlock,
            ErrorKind::PermissionDenied,
            ErrorKind::TimedOut,
        ] {
            assert!(!lock_unsupported(&Error::from(kind)));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_invalid_function_means_unsupported_locking() {
        assert!(lock_unsupported(&Error::from_raw_os_error(1)));
        for code in [5, 32, 33] {
            assert!(!lock_unsupported(&Error::from_raw_os_error(code)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_unsupported_and_nfs_lock_errors_are_recognized() {
        for code in [libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS, libc::ENOLCK] {
            assert!(lock_unsupported(&Error::from_raw_os_error(code)));
        }
        for code in [libc::EACCES, libc::EAGAIN, libc::EINTR] {
            assert!(!lock_unsupported(&Error::from_raw_os_error(code)));
        }
    }
}
