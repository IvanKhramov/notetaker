use std::process::Command;

use chrono::Utc;
use notetaker_core::{
    Channel, ExportFormat, Library, MeetingMeta, Segment, Source, SpeakerId, Transcript,
};

#[test]
fn export_cli_matches_all_core_formats_and_rejects_unknown_format() {
    let directory = tempfile::tempdir().unwrap();
    let library = Library::open(directory.path()).unwrap();
    let meeting = library
        .create_meeting(MeetingMeta::new("Экспорт", Utc::now()))
        .unwrap();
    let mut transcript = Transcript::default();
    transcript.rename_speaker("s1", "Анна");
    transcript.segments.push(Segment {
        id: meeting.read_meta().unwrap().id,
        speaker: SpeakerId("s1".into()),
        channel: Channel::System,
        start_ms: 1234,
        end_ms: 5678,
        text: "Текст, с \"кавычками\"".into(),
        source: Source::Final,
        edited: false,
        lang: None,
    });
    meeting.write_transcript(&transcript).unwrap();
    for format in ExportFormat::ALL {
        let output = Command::new(env!("CARGO_BIN_EXE_notetaker"))
            .arg("export")
            .arg(meeting.path())
            .args(["--format", format.extension()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            transcript.export(format).unwrap()
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_notetaker"))
        .arg("export")
        .arg(meeting.path())
        .args(["--format", "xml"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
