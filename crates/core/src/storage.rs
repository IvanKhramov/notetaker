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

    /// Metadata is published last so incomplete creations are skipped by listing.
    pub fn create_meeting(&self, meta: MeetingMeta) -> Result<Meeting> {
        let base = format!(
            "{}_{}",
            meta.started_at.format("%Y-%m-%d_%H%M"),
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
            if entry.file_name() == ".notetaker" || !entry.file_type()?.is_dir() {
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

    pub fn write_meta(&self, meta: &MeetingMeta) -> Result<()> {
        atomic_write(
            &self.path.join("meta.yaml"),
            serde_yaml::to_string(meta)?.as_bytes(),
        )
    }

    pub fn read_notes(&self) -> Result<String> {
        read_optional(&self.source_path("notes.md")?)
    }
    pub fn write_notes(&self, notes: &str) -> Result<()> {
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        atomic_write(&self.path.join("notes.md"), notes.as_bytes())?;
        self.clean_attachments_after_save();
        Ok(())
    }
    pub fn read_enhanced(&self) -> Result<String> {
        read_optional(&self.source_path("enhanced.md")?)
    }
    pub fn write_enhanced(&self, enhanced: &str) -> Result<()> {
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        atomic_write(&self.path.join("enhanced.md"), enhanced.as_bytes())?;
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
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if transcript.version != crate::SCHEMA_VERSION {
            return Err(crate::Error::UnsupportedVersion(transcript.version));
        }
        atomic_write(
            &self.path.join("transcript.json"),
            &serde_json::to_vec_pretty(transcript)?,
        )
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
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let revision = checked_write(&self.path.join(name), bytes, expected)?;
        if matches!(name, "notes.md" | "enhanced.md") {
            self.clean_attachments_after_save();
        }
        Ok(revision)
    }
    /// Serialize read-modify-write across every handle in this process. External
    /// edits made during the closure are detected by the content revision too.
    pub fn update_transcript(&self, update: impl FnOnce(&mut Transcript)) -> Result<()> {
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut loaded = self.load_transcript()?;
        update(&mut loaded.value);
        if loaded.value.version != crate::SCHEMA_VERSION {
            return Err(crate::Error::UnsupportedVersion(loaded.value.version));
        }
        checked_write(
            &self.path.join("transcript.json"),
            &serde_json::to_vec_pretty(&loaded.value)?,
            loaded.revision,
        )?;
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

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temporary = NamedTempFile::new_in(path.parent().expect("meeting file has a parent"))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
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

fn load_text(path: &Path) -> Result<Versioned<String>> {
    let bytes = read_bytes(path)?;
    let revision = Revision::of(bytes.as_deref());
    let value = String::from_utf8(bytes.unwrap_or_default())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Versioned { value, revision })
}

fn checked_write(path: &Path, bytes: &[u8], expected: Revision) -> Result<Revision> {
    let current = Revision::of(read_bytes(path)?.as_deref());
    if current != expected {
        // UUID disambiguates concurrent saves within the same millisecond.
        let stem = path
            .file_stem()
            .expect("meeting file has a stem")
            .to_string_lossy();
        let ext = path
            .extension()
            .expect("meeting file has an extension")
            .to_string_lossy();
        let local = path.with_file_name(format!(
            "{stem}.conflict-{}-{}.{ext}",
            DateTime::<Utc>::from(std::time::SystemTime::now()).format("%Y%m%dT%H%M%S%.3fZ"),
            uuid::Uuid::now_v7()
        ));
        atomic_write(&local, bytes)?;
        return Err(crate::Error::Conflict {
            path: path.to_owned(),
            local,
        });
    }
    atomic_write(path, bytes)?;
    Ok(Revision::of(Some(bytes)))
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
