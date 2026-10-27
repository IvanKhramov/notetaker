use std::{collections::BTreeSet, fs, ops::Range, path::PathBuf};

use chrono::{DateTime, Duration, Utc};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::{
    Meeting, Result,
    storage::{atomic_write, meeting_lock},
};

/// Markdown image source ranges and resolved destinations, excluding code blocks.
pub fn markdown_images(text: &str) -> Vec<(Range<usize>, String)> {
    let mut images = Vec::new();
    let mut image = None;
    let mut depth = 0;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                if depth == 0 {
                    image = Some((range.start, dest_url.into_string()));
                }
                depth += 1;
            }
            Event::End(TagEnd::Image) => {
                depth -= 1;
                if depth == 0
                    && let Some((start, destination)) = image.take()
                {
                    images.push((start..range.end, destination));
                }
            }
            _ => {}
        }
    }
    images
}

/// Keep image and ordinary attachment links, including reference-style Markdown.
pub fn attachment_references(notes: &str) -> BTreeSet<String> {
    Parser::new(notes)
        .filter_map(|event| {
            let destination = match event {
                Event::Start(Tag::Image { dest_url, .. } | Tag::Link { dest_url, .. }) => dest_url,
                _ => return None,
            };
            let destination = destination.split(['#', '?']).next()?;
            let destination = percent_encoding::percent_decode_str(destination)
                .decode_utf8()
                .ok()?;
            let destination = destination.strip_prefix("./").unwrap_or(&destination);
            let name = destination.strip_prefix("attachments/")?;
            if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) {
                return None;
            }
            Some(format!("attachments/{name}"))
        })
        .collect()
}

/// Only local attachments can be loaded as images, never arbitrary paths/URIs.
pub fn attachment_image_path(meeting: &std::path::Path, destination: &str) -> Option<PathBuf> {
    let destination = destination.split(['#', '?']).next()?;
    let decoded = percent_encoding::percent_decode_str(destination)
        .decode_utf8()
        .ok()?;
    let relative = decoded.strip_prefix("./").unwrap_or(&decoded);
    let name = relative.strip_prefix("attachments/")?;
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', ':']) {
        return None;
    }
    let meeting = meeting.canonicalize().ok()?;
    let folder = meeting.join("attachments").canonicalize().ok()?;
    if folder.parent() != Some(meeting.as_path()) {
        return None;
    }
    let image = folder.join(name).canonicalize().ok()?;
    (image.starts_with(&folder) && image.is_file()).then_some(image)
}

fn directory(path: &std::path::Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.is_dir() && !meta.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

impl Meeting {
    /// The caller validates the image contents before publishing an attachment.
    pub fn add_attachment(&self, bytes: &[u8], extension: &str) -> Result<String> {
        if !matches!(extension, "png" | "jpg") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported attachment format",
            )
            .into());
        }
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let folder = self.path.join("attachments");
        fs::create_dir_all(&folder)?;
        if !directory(&folder)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "attachments must be a directory",
            )
            .into());
        }
        let relative = format!("attachments/{}.{}", uuid::Uuid::now_v7(), extension);
        atomic_write(&self.path.join(&relative), bytes)?;
        Ok(relative)
    }

    pub(crate) fn clean_attachments_after_save(&self) {
        // The note revision is committed even if ancillary cleanup fails.
        if let Err(error) =
            self.reconcile_attachments(DateTime::<Utc>::from(std::time::SystemTime::now()))
        {
            tracing::warn!(path = %self.path.display(), %error, "attachment cleanup failed");
        }
    }

    fn markdown_attachment_references(&self) -> Result<BTreeSet<String>> {
        let mut references = BTreeSet::new();
        // All Markdown we write lives directly in the meeting: notes, enhanced,
        // exported summaries/chats and conflict copies must preserve attachments.
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let name = entry.file_name();
            if entry.path().extension().is_some_and(|ext| ext == "md") {
                let path = self.source_path(&name.to_string_lossy())?;
                references.extend(attachment_references(&fs::read_to_string(path)?));
            }
        }
        Ok(references)
    }

    fn reconcile_attachments(&self, now: DateTime<Utc>) -> Result<()> {
        let folder = self.path.join("attachments");
        if !directory(&folder)? {
            return Ok(());
        }
        let references = self.markdown_attachment_references()?;
        let trash = folder.join(".trash");
        let has_trash = directory(&trash)?;
        // Undo after an autosave can reintroduce a reference that was just trashed.
        if has_trash {
            let mut entries = fs::read_dir(&trash)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries.into_iter().rev() {
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let name = entry.file_name();
                let Some((_, original)) = name.to_str().and_then(|name| name.split_once('-'))
                else {
                    continue;
                };
                if references.contains(&format!("attachments/{original}"))
                    && !folder.join(original).exists()
                {
                    fs::rename(entry.path(), folder.join(original))?;
                }
            }
        }
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            if references.contains(&format!("attachments/{}", name.to_string_lossy())) {
                continue;
            }
            if !has_trash {
                fs::create_dir_all(&trash)?;
                if !directory(&trash)? {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "attachment trash must be a directory",
                    )
                    .into());
                }
            }
            fs::rename(
                entry.path(),
                trash.join(format!(
                    "{}-{}",
                    now.timestamp_millis(),
                    name.to_string_lossy()
                )),
            )?;
        }
        Ok(())
    }

    pub(crate) fn purge_attachment_trash(&self, now: DateTime<Utc>) -> Result<Vec<PathBuf>> {
        let lock = meeting_lock(&self.path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let folder = self.path.join("attachments");
        let trash = folder.join(".trash");
        if !directory(&folder)? || !directory(&trash)? {
            return Ok(Vec::new());
        }
        // Re-read current Markdown under the meeting lock, even when no notes
        // were saved since an external edit or export reintroduced a reference.
        let references = self.markdown_attachment_references()?;
        let mut removed = Vec::new();
        let cutoff = (now - Duration::days(30)).timestamp_millis();
        for entry in fs::read_dir(trash)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some((timestamp, original)) = name.to_str().and_then(|name| name.split_once('-'))
            else {
                continue;
            };
            if references.contains(&format!("attachments/{original}")) {
                if !folder.join(original).exists() {
                    fs::rename(entry.path(), folder.join(original))?;
                }
                continue;
            }
            let expired = timestamp
                .parse::<i64>()
                .is_ok_and(|timestamp| timestamp < cutoff);
            if expired {
                fs::remove_file(entry.path())?;
                removed.push(entry.path());
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Library, MeetingMeta};

    #[test]
    fn nested_image_alt_text_does_not_hide_the_outer_destination() {
        let text = "![outer ![inner](attachments/a.png)](file://attacker/share/i.png)";
        let images = markdown_images(text);
        assert_eq!(
            images,
            vec![(0..text.len(), "file://attacker/share/i.png".into())]
        );
    }

    #[test]
    fn image_paths_allow_only_existing_contained_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("attachments");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a b.png"), b"image").unwrap();
        assert!(attachment_image_path(temp.path(), "./attachments/a%20b.png").is_some());
        for destination in [
            "../../x.png",
            "file://attacker/share/i.png",
            "//attacker/share/i.png",
            r"\\attacker\share\i.png",
            "https://example.org/i.png",
            "http://example.org/i.png",
            "attachments/missing.png",
            "attachments/../notes.md",
            "attachments/%2e%2e%2fx.png",
        ] {
            assert!(
                attachment_image_path(temp.path(), destination).is_none(),
                "{destination}"
            );
        }
        #[cfg(unix)]
        {
            fs::write(temp.path().join("outside.png"), b"secret").unwrap();
            std::os::unix::fs::symlink(temp.path().join("outside.png"), folder.join("linked.png"))
                .unwrap();
            assert!(attachment_image_path(temp.path(), "attachments/linked.png").is_none());
        }
    }

    #[test]
    fn references_follow_markdown_and_ignore_code_and_unsafe_paths() {
        let notes = r#"![](attachments/a.png) ![a](./attachments/b.jpg "title")
![ref][photo] [download](attachments/c.png)

[photo]: attachments/d.png
`![](attachments/code.png)`
```md
![](attachments/fenced.png)
```
![](https://example.org/attachments/e.png) ![](attachments/../notes.md)
![](attachments/.trash/a.png) ![](attachments/sub/e.png)
![](<attachments/e%20f.png>) ![](attachments/a.png#fragment)
![](attachments/hash%23tag.png) ![](attachments/query%3Fmark.png)
"#;
        assert_eq!(
            attachment_references(notes),
            [
                "a.png",
                "b.jpg",
                "c.png",
                "d.png",
                "e f.png",
                "hash#tag.png",
                "query?mark.png"
            ]
            .map(|name| format!("attachments/{name}"))
            .into()
        );
        let images = markdown_images("до ![alt](attachments/a.png) после");
        assert_eq!(
            &"до ![alt](attachments/a.png) после"[images[0].0.clone()],
            "![alt](attachments/a.png)"
        );
    }

    #[test]
    fn enhanced_and_export_references_survive_cleanup_and_expired_trash() {
        let now = DateTime::<Utc>::from(std::time::SystemTime::now());
        let temp = tempfile::tempdir().unwrap();
        let library = Library::open(temp.path()).unwrap();
        for name in ["enhanced.md", "summary-export.md", "chat-export.md"] {
            let meeting = library.create_meeting(MeetingMeta::new(name, now)).unwrap();
            let image = meeting.add_attachment(b"image", "png").unwrap();
            let markdown = format!("![]({image})");
            fs::write(meeting.path.join(name), &markdown).unwrap();
            meeting.write_notes("").unwrap();
            assert!(meeting.path.join(&image).exists(), "{name}");
            fs::write(meeting.path.join(name), "").unwrap();
            meeting.write_notes("").unwrap();
            assert!(!meeting.path.join(&image).exists());
            // An external editor/export can restore a reference without autosave.
            fs::write(meeting.path.join(name), markdown).unwrap();
            assert!(
                meeting
                    .purge_attachment_trash(now + Duration::days(31))
                    .unwrap()
                    .is_empty()
            );
            assert!(meeting.path.join(&image).exists(), "{name}");
            fs::write(meeting.path.join(name), "").unwrap();
            meeting.write_notes("").unwrap();
            assert_eq!(
                meeting
                    .purge_attachment_trash(now + Duration::days(31))
                    .unwrap()
                    .len(),
                1
            );
        }
    }

    #[test]
    fn saves_trash_restore_and_sweep_by_deletion_time_not_audio_expiry() {
        let now = DateTime::<Utc>::from(std::time::SystemTime::now());
        let temp = tempfile::tempdir().unwrap();
        let library = Library::open(temp.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Images", now))
            .unwrap();
        let relative = meeting.add_attachment(b"image", "png").unwrap();
        let notes = format!("![]({relative})");
        let revision = meeting.load_notes().unwrap().revision;
        let revision = meeting.save_notes(&notes, revision).unwrap();
        assert!(meeting.path.join(&relative).exists());
        library.reindex_full().unwrap();
        assert_eq!(library.list_meetings().unwrap().len(), 1);
        let revision = meeting.save_notes("", revision).unwrap();
        assert!(!meeting.path.join(&relative).exists());
        let trash = meeting.path.join("attachments/.trash");
        assert_eq!(fs::read_dir(&trash).unwrap().count(), 1);
        assert!(
            library
                .sweep_audio(now + Duration::days(29))
                .unwrap()
                .is_empty()
        );
        meeting.save_notes(&notes, revision).unwrap();
        assert!(meeting.path.join(&relative).exists());
        assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);
        assert!(
            library
                .sweep_audio(now + Duration::days(40))
                .unwrap()
                .is_empty()
        );
        meeting.write_notes("").unwrap();
        library.reindex_full().unwrap();
        assert_eq!(
            library.sweep_audio(now + Duration::days(31)).unwrap().len(),
            1
        );
        assert!(
            library
                .sweep_audio(now + Duration::days(31))
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn attachments_and_trash_symlinks_are_not_followed() {
        let now = DateTime::<Utc>::from(std::time::SystemTime::now());
        let temp = tempfile::tempdir().unwrap();
        let library = Library::open(temp.path()).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let keep = outside.join("0-keep.png");
        fs::write(&keep, b"keep").unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Symlink", now))
            .unwrap();
        std::os::unix::fs::symlink(&outside, meeting.path.join("attachments")).unwrap();
        assert!(meeting.add_attachment(b"image", "png").is_err());
        assert!(library.sweep_audio(now).unwrap().is_empty());
        let meeting = library
            .create_meeting(MeetingMeta::new("Trash symlink", now))
            .unwrap();
        let image = meeting.add_attachment(b"image", "png").unwrap();
        std::os::unix::fs::symlink(&outside, meeting.path.join("attachments/.trash")).unwrap();
        meeting.write_notes("").unwrap();
        assert!(library.sweep_audio(now).unwrap().is_empty());
        assert!(meeting.path.join(image).exists());
        assert_eq!(fs::read(keep).unwrap(), b"keep");
    }

    #[test]
    fn conflicted_save_preserves_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let library = Library::open(temp.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new(
                "Images",
                DateTime::<Utc>::from(std::time::SystemTime::now()),
            ))
            .unwrap();
        let stale = meeting.load_notes().unwrap().revision;
        let image = meeting.add_attachment(b"image", "jpg").unwrap();
        meeting.write_notes(&format!("![]({image})")).unwrap();
        assert!(meeting.save_notes("", stale).is_err());
        assert!(meeting.path.join(image).exists());
    }
}
