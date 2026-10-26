use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use tempfile::NamedTempFile;

use crate::{MeetingMeta, Result, Transcript, migrate_transcript};

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

    /// Remove immediate audio files (including symlinks), without following directories.
    pub fn sweep_audio(&self, now: DateTime<Utc>) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        for meeting in self.list_meetings()? {
            let meta = meeting.read_meta()?;
            if !meta.audio_expires_at.is_some_and(|expires| expires <= now) {
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

    pub fn read_meta(&self) -> Result<MeetingMeta> {
        Ok(serde_yaml::from_str(&fs::read_to_string(
            self.path.join("meta.yaml"),
        )?)?)
    }

    pub fn write_meta(&self, meta: &MeetingMeta) -> Result<()> {
        atomic_write(
            &self.path.join("meta.yaml"),
            serde_yaml::to_string(meta)?.as_bytes(),
        )
    }

    pub fn read_notes(&self) -> Result<String> {
        read_optional(&self.path.join("notes.md"))
    }
    pub fn write_notes(&self, notes: &str) -> Result<()> {
        atomic_write(&self.path.join("notes.md"), notes.as_bytes())
    }
    pub fn read_enhanced(&self) -> Result<String> {
        read_optional(&self.path.join("enhanced.md"))
    }
    pub fn write_enhanced(&self, enhanced: &str) -> Result<()> {
        atomic_write(&self.path.join("enhanced.md"), enhanced.as_bytes())
    }

    pub fn read_transcript(&self) -> Result<Transcript> {
        match fs::read_to_string(self.path.join("transcript.json")) {
            Ok(text) => migrate_transcript(serde_json::from_str(&text)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Transcript::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn write_transcript(&self, transcript: &Transcript) -> Result<()> {
        if transcript.version != crate::SCHEMA_VERSION {
            return Err(crate::Error::UnsupportedVersion(transcript.version));
        }
        atomic_write(
            &self.path.join("transcript.json"),
            &serde_json::to_vec_pretty(transcript)?,
        )
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

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
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
