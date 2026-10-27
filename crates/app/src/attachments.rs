use std::{
    io::{Cursor, Read},
    ops::Range,
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use eframe::egui;
use image::{ImageEncoder, ImageFormat, codecs::png::PngEncoder};
use notetaker_core::Meeting;

pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

pub enum ImageInput {
    Clipboard(Arc<egui::ColorImage>),
    File(PathBuf),
}

pub struct InsertAt {
    pub notes: String,
    pub cursor: Option<Range<usize>>,
}

pub fn import(meeting: &Meeting, input: ImageInput) -> Result<String> {
    let (bytes, extension) = match input {
        ImageInput::Clipboard(image) => {
            // The 20 MB limit applies to the encoded PNG: a full 4K screenshot is
            // ~33 MB as raw RGBA but a few MB as PNG.
            ensure!(
                image.width() <= 16384 && image.height() <= 16384,
                "Изображение слишком большое"
            );
            let width = u32::try_from(image.width())?;
            let height = u32::try_from(image.height())?;
            ensure!(
                width > 0
                    && height > 0
                    && image.width().checked_mul(image.height()) == Some(image.pixels.len()),
                "Некорректный размер изображения"
            );
            let rgba: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_srgba_unmultiplied())
                .collect();
            let mut bytes = Vec::new();
            PngEncoder::new(&mut bytes).write_image(
                &rgba,
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )?;
            ensure!(bytes.len() <= MAX_IMAGE_BYTES, "Изображение больше 20 МБ");
            (bytes, "png")
        }
        ImageInput::File(path) => {
            let file = std::fs::File::open(&path).context("Не удалось открыть изображение")?;
            ensure!(
                file.metadata()?.len() <= MAX_IMAGE_BYTES as u64,
                "Изображение больше 20 МБ"
            );
            let mut bytes = Vec::new();
            file.take(MAX_IMAGE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= MAX_IMAGE_BYTES, "Изображение больше 20 МБ");
            let format =
                image::guess_format(&bytes).context("Поддерживаются изображения PNG и JPEG")?;
            let extension = match format {
                ImageFormat::Png => "png",
                ImageFormat::Jpeg => "jpg",
                _ => anyhow::bail!("Поддерживаются изображения PNG и JPEG"),
            };
            let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(80 * 1024 * 1024);
            limits.max_image_width = Some(16384);
            limits.max_image_height = Some(16384);
            reader.limits(limits);
            reader
                .decode()
                .context("Не удалось прочитать изображение")?;
            (bytes, extension)
        }
    };
    meeting
        .add_attachment(&bytes, extension)
        .context("Не удалось сохранить изображение")
}

pub fn insert(notes: &mut String, anchor: InsertAt, images: &[String]) -> usize {
    let end = notes.len();
    let range = if *notes == anchor.notes {
        anchor
            .cursor
            .and_then(|range| {
                let start = notes
                    .char_indices()
                    .nth(range.start)
                    .map_or(end, |(i, _)| i);
                let end = notes.char_indices().nth(range.end).map_or(end, |(i, _)| i);
                (start <= end).then_some(start..end)
            })
            .unwrap_or(end..end)
    } else {
        end..end
    };
    let mut markdown = String::new();
    if range.start > 0 && !notes[..range.start].ends_with('\n') {
        markdown.push('\n');
    }
    for image in images {
        markdown.push_str(&format!("![]({image})\n"));
    }
    let cursor = notes[..range.start].chars().count() + markdown.chars().count();
    notes.replace_range(range, &markdown);
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{Library, MeetingMeta};

    #[test]
    fn inserts_at_unicode_selection_and_appends_when_editor_changed() {
        let mut notes = "До 🚀 после".to_owned();
        let anchor = InsertAt {
            notes: notes.clone(),
            cursor: Some(3..4),
        };
        let cursor = insert(&mut notes, anchor, &["attachments/a.png".into()]);
        assert_eq!(notes, "До \n![](attachments/a.png)\n после");
        assert_eq!(cursor, "До \n![](attachments/a.png)\n".chars().count());
        let anchor = InsertAt {
            notes: "old".into(),
            cursor: Some(0..0),
        };
        insert(&mut notes, anchor, &["attachments/b.jpg".into()]);
        assert!(notes.ends_with("после\n![](attachments/b.jpg)\n"));
    }

    #[test]
    fn clipboard_png_roundtrip_and_file_validation() {
        let temp = tempfile::tempdir().unwrap();
        let meeting = Library::open(temp.path())
            .unwrap()
            .create_meeting(MeetingMeta::new("Images", chrono::Utc::now()))
            .unwrap();
        let color = egui::Color32::from_rgba_unmultiplied(120, 80, 40, 128);
        let input = egui::ColorImage::filled([2, 3], color);
        let relative = import(&meeting, ImageInput::Clipboard(Arc::new(input))).unwrap();
        let decoded = image::open(meeting.path().join(&relative))
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 3));
        assert_eq!(decoded.get_pixel(0, 0).0, color.to_srgba_unmultiplied());
        let copy = import(&meeting, ImageInput::File(meeting.path().join(relative))).unwrap();
        assert!(copy.ends_with(".png"));
        // A 4K screenshot is over 20 MB as raw RGBA but small as PNG.
        let screenshot = egui::ColorImage::filled([3840, 2160], egui::Color32::WHITE);
        import(&meeting, ImageInput::Clipboard(Arc::new(screenshot))).unwrap();
        let oversized = egui::ColorImage::filled([16385, 1], egui::Color32::WHITE);
        assert!(
            import(&meeting, ImageInput::Clipboard(Arc::new(oversized)))
                .unwrap_err()
                .to_string()
                .contains("слишком большое")
        );
        let jpeg = temp.path().join("photo.JPEG");
        image::RgbImage::new(2, 3)
            .save_with_format(&jpeg, ImageFormat::Jpeg)
            .unwrap();
        let relative = import(&meeting, ImageInput::File(jpeg)).unwrap();
        assert!(relative.ends_with(".jpg"));
        let invalid = temp.path().join("invalid.png");
        std::fs::write(&invalid, b"not an image").unwrap();
        assert!(import(&meeting, ImageInput::File(invalid)).is_err());
        let large = temp.path().join("large.png");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES as u64 + 1)
            .unwrap();
        assert!(
            import(&meeting, ImageInput::File(large))
                .unwrap_err()
                .to_string()
                .contains("20 МБ")
        );
    }
}
