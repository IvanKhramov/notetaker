//! Shared files are revision checked; disposable and biometric state stays local.
use crate::{
    Library, Meeting, Result, Revision, Versioned,
    storage::{checked_write, load_text, meeting_lock},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibrarySettings {
    pub shared: bool,
}

pub fn likely_synced(path: &Path) -> bool {
    let path = path.to_string_lossy().replace('\\', "/").to_lowercase();
    [
        "yandex.disk",
        "yandexdisk",
        "яндекс.диск",
        "onedrive",
        "dropbox",
        "google drive",
        "googledrive",
    ]
    .iter()
    .any(|name| path.split('/').any(|part| part.contains(name)))
}
impl Library {
    pub fn load_library_settings(&self) -> Result<Versioned<LibrarySettings>> {
        let loaded = self.load_shared_text("library.json")?;
        Ok(Versioned {
            value: if loaded.value.is_empty() {
                LibrarySettings::default()
            } else {
                serde_json::from_str(&loaded.value)?
            },
            revision: loaded.revision,
        })
    }
    pub fn set_shared(&self, shared: bool) -> Result<()> {
        let loaded = self.load_library_settings()?;
        if !loaded.value.shared {
            self.relocate_private_state()?;
        }
        self.save_shared_text(
            "library.json",
            &serde_json::to_string_pretty(&LibrarySettings { shared })?,
            loaded.revision,
        )?;
        if loaded.value.shared != shared {
            // A previous stay in this mode may have left semantic sources here.
            // Do not reuse them: the background semantic job rebuilds from files.
            self.open_index()?.execute_batch(
                "BEGIN IMMEDIATE; DELETE FROM semantic_chunks; DELETE FROM semantic_sources; COMMIT;",
            )?;
        }
        // Rebuild instead of copying a SQLite database that may be open elsewhere.
        self.reindex_full()
    }
    pub fn is_shared(&self) -> bool {
        // A damaged settings file must not put private caches back into sync.
        self.load_library_settings()
            .map_or(true, |s| s.value.shared)
    }
    /// One helper for the SQLite index and future disposable caches.
    pub fn local_state_dir(&self) -> PathBuf {
        if self.is_shared() {
            self.private_state_dir()
        } else {
            self.root().join(".notetaker")
        }
    }
    /// Biometrics always use this directory, even for a private library.
    pub fn private_state_dir(&self) -> PathBuf {
        let base = directories::ProjectDirs::from("", "", "notetaker")
            .map(|d| d.data_local_dir().to_owned())
            .unwrap_or_else(|| std::env::temp_dir().join("notetaker"));
        base.join("libraries").join(format!(
            "{:x}",
            Sha256::digest(self.root().to_string_lossy().as_bytes())
        ))
    }
    /// Move legacy biometrics before a private library is marked shared. Already
    /// shared libraries never import another member's voice enrollment.
    pub(crate) fn relocate_private_state(&self) -> Result<()> {
        if self.is_shared() {
            return Ok(());
        }
        for name in ["voices.json", "speaker-embeddings"] {
            let from = self.root().join(".notetaker").join(name);
            if !from.exists() {
                continue;
            }
            let to = self.private_state_dir().join(name);
            fs::create_dir_all(self.private_state_dir())?;
            if from.is_file() {
                move_legacy_file(&from, &to)?;
            } else if from.is_dir() {
                fs::create_dir_all(&to)?;
                for entry in fs::read_dir(&from)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        move_legacy_file(&entry.path(), &to.join(entry.file_name()))?;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn load_shared_text(&self, name: &str) -> Result<Versioned<String>> {
        shared_name(name)?;
        load_text(&self.root().join(".notetaker").join(name))
    }
    pub fn save_shared_text(&self, name: &str, text: &str, expected: Revision) -> Result<Revision> {
        shared_name(name)?;
        let path = self.root().join(".notetaker").join(name);
        fs::create_dir_all(path.parent().expect("state parent"))?;
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        checked_write(&path, text.as_bytes(), expected)
    }
}
fn move_legacy_file(from: &Path, to: &Path) -> Result<()> {
    // Never replace an existing enrollment. Archive both versions locally.
    let destination = if to.exists() {
        to.with_extension(format!("legacy-{}.json", uuid::Uuid::now_v7()))
    } else {
        to.to_owned()
    };
    fs::copy(from, &destination)?;
    // Windows FlushFileBuffers needs write access; a read-only handle fails.
    fs::OpenOptions::new()
        .write(true)
        .open(&destination)?
        .sync_all()?;
    fs::remove_file(from)?;
    Ok(())
}
fn shared_name(name: &str) -> Result<()> {
    if !matches!(
        name,
        "library.json" | "folders.json" | "people.json" | "vocabulary.txt"
    ) {
        return Err(crate::Error::TaskEdit("Недопустимый общий файл".into()));
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct SyncConflict {
    pub source: String,
    pub copy: String,
    pub mine: Versioned<String>,
    pub theirs: Versioned<String>,
}
#[derive(Clone, Copy, Debug)]
pub enum MergeChoice {
    Mine,
    Theirs,
    Both,
}
impl Meeting {
    pub fn sync_conflicts(&self) -> Result<Vec<SyncConflict>> {
        let mut conflicts = Vec::new();
        for entry in fs::read_dir(self.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            for stem in ["notes", "enhanced"] {
                if lower.ends_with(".md")
                    && [
                        format!("{stem} ("),
                        format!("{stem}-"),
                        format!("{stem}.conflict"),
                        format!("{stem} (conflicted"),
                        format!("{stem} -"),
                    ]
                    .iter()
                    .any(|prefix| lower.starts_with(prefix))
                {
                    let source = format!("{stem}.md");
                    conflicts.push(SyncConflict {
                        mine: load_text(&self.source_path(&source)?)?,
                        theirs: load_text(&self.source_path(&name)?)?,
                        source,
                        copy: name.clone(),
                    });
                }
            }
        }
        conflicts.sort_by(|a, b| a.copy.cmp(&b.copy));
        Ok(conflicts)
    }
    pub fn merge_sync_conflict(&self, conflict: &SyncConflict, choice: MergeChoice) -> Result<()> {
        // Only act on a current detected conflict, never a supplied arbitrary path.
        let current = self
            .sync_conflicts()?
            .into_iter()
            .find(|c| c.source == conflict.source && c.copy == conflict.copy)
            .ok_or_else(|| crate::Error::TaskEdit("Копия больше не найдена".into()))?;
        if current.mine.revision != conflict.mine.revision
            || current.theirs.revision != conflict.theirs.revision
        {
            return Err(crate::Error::TaskEdit(
                "Файлы изменились; обновите сравнение".into(),
            ));
        }
        let text = match choice {
            MergeChoice::Mine => conflict.mine.value.clone(),
            MergeChoice::Theirs => conflict.theirs.value.clone(),
            MergeChoice::Both => format!(
                "{}\n\n## Копия от синхронизации\n\n{}",
                conflict.mine.value, conflict.theirs.value
            ),
        };
        if conflict.source == "notes.md" {
            self.save_notes(&text, conflict.mine.revision)?;
        } else {
            self.save_enhanced(&text, conflict.mine.revision)?;
        }
        // Keep an archive instead of deleting the other machine's file.
        fs::rename(
            self.source_path(&conflict.copy)?,
            self.path().join(format!(
                "{}.resolved-{}",
                conflict.copy,
                uuid::Uuid::now_v7()
            )),
        )?;
        Ok(())
    }
}
