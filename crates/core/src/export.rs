use crate::{Result, Transcript};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Md,
    Srt,
    Vtt,
    Csv,
    Json,
}
impl ExportFormat {
    pub const ALL: [Self; 5] = [Self::Md, Self::Srt, Self::Vtt, Self::Csv, Self::Json];
    pub fn extension(self) -> &'static str {
        match self {
            Self::Md => "md",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
}
impl std::str::FromStr for ExportFormat {
    type Err = String;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|f| f.extension() == value)
            .ok_or_else(|| "Формат: md|srt|vtt|csv|json".into())
    }
}

pub fn format_timestamp(ms: u64, separator: char) -> String {
    format!(
        "{:02}:{:02}:{:02}{separator}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
fn subtitle_text(text: &str) -> String {
    // Encode markup and flatten lines so text cannot introduce cues or VTT directives.
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn markdown_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(['\r', '\n'], " ")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('`', "\\`")
}
fn csv(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

impl Transcript {
    pub fn export(&self, format: ExportFormat) -> Result<String> {
        let timeline = self.speaker_timeline();
        let mut output = match format {
            ExportFormat::Vtt => "WEBVTT\n\n".into(),
            ExportFormat::Csv => "start,end,speaker,text\r\n".into(),
            ExportFormat::Json => return Ok(serde_json::to_string_pretty(&timeline)? + "\n"),
            _ => String::new(),
        };
        for (index, segment) in timeline.segments.iter().enumerate() {
            let start = format_timestamp(segment.start_ms, '.');
            let end = format_timestamp(segment.end_ms, '.');
            match format {
                ExportFormat::Md => output.push_str(&format!(
                    "[{start} - {end}] **{}**: {}\n\n",
                    markdown_text(&segment.name),
                    markdown_text(&segment.text)
                )),
                ExportFormat::Srt | ExportFormat::Vtt => {
                    let separator = if format == ExportFormat::Srt {
                        ','
                    } else {
                        '.'
                    };
                    output.push_str(&format!(
                        "{}\n{} --> {}\n{}: {}\n\n",
                        index + 1,
                        format_timestamp(segment.start_ms, separator),
                        format_timestamp(segment.end_ms, separator),
                        subtitle_text(&segment.name),
                        subtitle_text(&segment.text)
                    ));
                }
                ExportFormat::Csv => output.push_str(&format!(
                    "{start},{end},{},{}\r\n",
                    csv(&segment.name),
                    csv(&segment.text)
                )),
                ExportFormat::Json => unreachable!(),
            }
        }
        Ok(output)
    }
}

impl crate::Meeting {
    pub fn export_summary(&self, text: &str) -> Result<std::path::PathBuf> {
        self.write_export("summary.md", text)
    }
    pub fn export_transcript(
        &self,
        transcript: &Transcript,
        format: ExportFormat,
    ) -> Result<std::path::PathBuf> {
        self.write_export(
            &format!("transcript.{}", format.extension()),
            &transcript.export(format)?,
        )
    }
    fn write_export(&self, suffix: &str, text: &str) -> Result<std::path::PathBuf> {
        // The folder name is immutable even when the meeting title changes.
        let folder = self
            .path()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let slug = folder.splitn(3, '_').nth(2).unwrap_or(&folder);
        let path = self.path().join(format!("{slug}.{suffix}"));
        crate::storage::atomic_write(&path, text.as_bytes())?;
        Ok(path)
    }
}

#[cfg(test)]
mod file_tests {
    #[test]
    fn export_replaces_the_same_file_per_format() {
        let dir = tempfile::tempdir().unwrap();
        let library = crate::Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(crate::MeetingMeta::new("Example", chrono::Utc::now()))
            .unwrap();
        let first = meeting.export_summary("old").unwrap();
        assert_eq!(first, meeting.export_summary("new").unwrap());
        assert_eq!(std::fs::read_to_string(first).unwrap(), "new");
        let transcript = crate::Transcript::default();
        let srt = meeting
            .export_transcript(&transcript, crate::ExportFormat::Srt)
            .unwrap();
        assert_eq!(
            srt,
            meeting
                .export_transcript(&transcript, crate::ExportFormat::Srt)
                .unwrap()
        );
        assert!(
            srt.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".transcript.srt")
        );
    }
}
