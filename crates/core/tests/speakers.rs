use std::collections::BTreeMap;

use chrono::Utc;
use notetaker_core::{
    Channel, ExportFormat, Library, MeetingMeta, Segment, Source, SpeakerEdit, SpeakerId,
    SummaryStamp, Transcript, format_timestamp,
};
use uuid::Uuid;

fn transcript() -> Transcript {
    let mut text = Transcript::default();
    for (speaker, start_ms, end_ms, content) in [
        ("s1", 0, 2000, "Раз"),
        ("s2", 1000, 3000, "Два"),
        ("s1", 4000, 5000, "Три"),
        ("s1", 5000, 7000, "Четыре"),
    ] {
        text.segments.push(Segment {
            id: Uuid::now_v7(),
            speaker: SpeakerId(speaker.into()),
            channel: Channel::System,
            start_ms,
            end_ms,
            text: content.into(),
            source: Source::Final,
            edited: false,
        });
    }
    text.rename_speaker("s1", "Иван");
    text.rename_speaker("s2", "Анна");
    text
}

#[test]
fn merge_undo_preserves_later_text_edits_and_rejects_other_speaker_edits() {
    let mut text = transcript();
    let original = text.clone();
    text.speaker_voice_ids.insert("s2".into(), "voice-2".into());
    let before = text.clone();
    let undo = text.merge_speakers("s2", "s1").unwrap();
    assert!(text.segments.iter().all(|s| s.speaker.0 == "s1"));
    assert_eq!(text.speaker_names.len(), 1);
    assert_eq!(text.speaker_voice_ids["s1"], "voice-2");
    text.segments[1].text = "Исправлено".into();
    text.undo_merge(&undo).unwrap();
    assert_eq!(text.speaker_names, before.speaker_names);
    assert_eq!(text.speaker_voice_ids, before.speaker_voice_ids);
    assert_eq!(text.segments[1].speaker.0, "s2");
    assert_eq!(text.segments[1].text, "Исправлено");
    assert!(text.segments[1].edited);
    let undo = text.merge_speakers("s2", "s1").unwrap();
    text.rename_speaker("s1", "Другой");
    assert!(text.undo_merge(&undo).is_err());
    assert!(text.merge_speakers("s1", "s1").is_err());
    let mut guessed = original.clone();
    guessed.speaker_names.remove("s1");
    guessed
        .speaker_suggestions
        .insert("s1".into(), "Неверная догадка".into());
    guessed
        .speaker_voice_ids
        .insert("s1".into(), "wrong".into());
    guessed
        .speaker_voice_ids
        .insert("s2".into(), "confirmed".into());
    guessed.merge_speakers("s2", "s1").unwrap();
    assert_eq!(guessed.speaker_name("s1"), "Анна");
    assert_eq!(guessed.speaker_voice_ids["s1"], "confirmed");
    assert!(!guessed.speaker_suggestions.contains_key("s1"));
    let stamp = SummaryStamp::new("", &original, "general", "summary");
    assert!(stamp.stale("", &text, "general"));
}

#[test]
fn reassign_selected_range_and_disjoint_ids_without_changing_timing_or_channels() {
    let mut text = transcript();
    let original = text.clone();
    let ids: Vec<_> = text.segments[1..=2].iter().map(|s| s.id).collect();
    text.reassign_segments(&ids, "s3").unwrap();
    assert_eq!(
        text.segments
            .iter()
            .map(|s| s.speaker.0.as_str())
            .collect::<Vec<_>>(),
        ["s1", "s3", "s3", "s1"]
    );
    text.reassign_segments(&[text.segments[0].id, text.segments[3].id], "s2")
        .unwrap();
    for (a, b) in text.segments.iter().zip(original.segments) {
        assert_eq!(
            (a.start_ms, a.end_ms, a.channel, a.text.as_str()),
            (b.start_ms, b.end_ms, b.channel, b.text.as_str())
        );
    }
    let before = text.clone();
    assert!(
        text.reassign_segments(&[text.segments[0].id, Uuid::now_v7()], "s4")
            .is_err()
    );
    assert_eq!(text, before);
    assert_eq!(text.new_speaker_id(), "s4");
    text.speaker_suggestions
        .insert("s4".into(), "Старый".into());
    text.speaker_voice_ids
        .insert("s5".into(), "voice-old".into());
    text.self_speaker = Some("s6".into());
    assert_eq!(text.new_speaker_id(), "s7");
}

#[test]
fn timing_counts_overlap_for_each_voice_and_joins_adjacent_chunks() {
    let mut text = transcript();
    let stats = text.speaker_timeline();
    assert_eq!(stats.duration_ms, 7000);
    let ivan = &stats.speakers[0];
    assert_eq!(
        (
            ivan.total_ms,
            ivan.turns,
            ivan.segment_count,
            ivan.longest_monologue_ms
        ),
        (5000, 2, 3, 3000)
    );
    assert!((ivan.share_percent - 500.0 / 7.0).abs() < 0.001);
    assert_eq!(stats.speakers[1].total_ms, 2000);
    text.segments.push(Segment {
        start_ms: 500,
        end_ms: 1500,
        ..text.segments[0].clone()
    });
    assert_eq!(text.speaker_timeline().speakers[0].total_ms, 5000);
    assert!(Transcript::default().speaker_timeline().speakers.is_empty());
}

#[test]
fn subtitle_markdown_csv_and_json_exports_are_deterministic_and_escape_content() {
    let mut text = transcript();
    text.segments.truncate(1);
    text.segments[0].start_ms = 3_723_004;
    text.segments[0].end_ms = 3_724_005;
    text.segments[0].text = "<b>Да & \"нет\"</b>,\n\n--> конец".into();
    text.rename_speaker("s1", "Иван, \"А\"");
    assert_eq!(format_timestamp(3_723_004, ','), "01:02:03,004");
    let srt = text.export(ExportFormat::Srt).unwrap();
    assert!(srt.starts_with("1\n01:02:03,004 --> 01:02:04,005\n"));
    assert!(srt.contains("&lt;b&gt;Да &amp; \"нет\"&lt;/b&gt;, --&gt; конец"));
    assert_eq!(srt.matches("\n\n").count(), 1);
    assert!(
        text.export(ExportFormat::Vtt)
            .unwrap()
            .starts_with("WEBVTT\n\n1\n01:02:03.004 --> 01:02:04.005\n")
    );
    let csv = text.export(ExportFormat::Csv).unwrap();
    assert!(csv.contains("\"Иван, \"\"А\"\"\",\"<b>Да & \"\"нет\"\"</b>,\n\n--> конец\"\r\n"));
    let json: serde_json::Value =
        serde_json::from_str(&text.export(ExportFormat::Json).unwrap()).unwrap();
    assert_eq!(json["speakers"][0]["total_ms"], 1001);
    assert_eq!(json["segments"][0]["text"], text.segments[0].text);
    assert!(
        text.export(ExportFormat::Md)
            .unwrap()
            .contains("[01:02:03.004 - 01:02:04.005] **Иван")
    );
}

#[test]
fn persisted_merge_undo_and_rename_propagate_by_voice_id_only() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let first = library
        .create_meeting(MeetingMeta::new("First", Utc::now()))
        .unwrap();
    let second = library
        .create_meeting(MeetingMeta::new("Second", Utc::now()))
        .unwrap();
    let third = library
        .create_meeting(MeetingMeta::new("Unconfirmed", Utc::now()))
        .unwrap();
    let text = transcript();
    first.write_transcript(&text).unwrap();
    library
        .write_meeting_voices(
            &first,
            BTreeMap::from([("s1".into(), vec![1.0, 0.0]), ("s2".into(), vec![0.0, 1.0])]),
        )
        .unwrap();
    library
        .edit_speakers(
            &first,
            &text,
            SpeakerEdit::Rename {
                speaker: "s1".into(),
                name: "Иван".into(),
                is_me: false,
                everywhere: false,
            },
            true,
        )
        .unwrap();
    let text = first.read_transcript().unwrap();
    let linked = text.speaker_voice_ids["s1"].clone();
    let mut other = transcript();
    other.speaker_voice_ids.insert("s2".into(), linked.clone());
    second.write_transcript(&other).unwrap();
    let mut unconfirmed = text.clone();
    unconfirmed.speaker_names.remove("s1");
    unconfirmed
        .speaker_suggestions
        .insert("s1".into(), "Иван".into());
    third.write_transcript(&unconfirmed).unwrap();
    let voices_before = library.read_voices().unwrap();
    let merged = library
        .edit_speakers(
            &first,
            &text,
            SpeakerEdit::Merge {
                from: "s2".into(),
                into: "s1".into(),
            },
            true,
        )
        .unwrap();
    assert_eq!(library.read_voices().unwrap().voices[0].embeddings.len(), 2);
    let current = first.read_transcript().unwrap();
    library
        .edit_speakers(
            &first,
            &current,
            SpeakerEdit::Undo(Box::new(merged.undo.unwrap())),
            true,
        )
        .unwrap();
    assert_eq!(first.read_transcript().unwrap(), text);
    assert_eq!(library.read_voices().unwrap(), voices_before);
    library
        .edit_speakers(
            &first,
            &text,
            SpeakerEdit::Rename {
                speaker: "s1".into(),
                name: "Пётр".into(),
                is_me: false,
                everywhere: true,
            },
            true,
        )
        .unwrap();
    let other = second.read_transcript().unwrap();
    assert_eq!(other.speaker_name("s2"), "Пётр");
    assert_eq!(other.speaker_name("s1"), "Иван");
    let unconfirmed = third.read_transcript().unwrap();
    assert!(!unconfirmed.speaker_names.contains_key("s1"));
    assert_eq!(unconfirmed.speaker_suggestions["s1"], "Пётр");
    assert!(!library.search("Пётр", 10).unwrap().is_empty());
    assert_eq!(
        library.read_voices().unwrap().voices[0].id.as_ref(),
        Some(&linked)
    );
}

#[test]
fn disabled_library_leaves_no_biometric_files_and_legacy_transcripts_load() {
    let old = r#"{"version":1,"segments":[]}"#;
    assert!(
        serde_json::from_str::<Transcript>(old)
            .unwrap()
            .speaker_voice_ids
            .is_empty()
    );
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let meeting = library
        .create_meeting(MeetingMeta::new("Off", Utc::now()))
        .unwrap();
    let text = transcript();
    meeting.write_transcript(&text).unwrap();
    library
        .edit_speakers(
            &meeting,
            &text,
            SpeakerEdit::Merge {
                from: "s2".into(),
                into: "s1".into(),
            },
            false,
        )
        .unwrap();
    assert!(!dir.path().join(".notetaker/voices.json").exists());
    assert!(!dir.path().join(".notetaker/speaker-embeddings").exists());
    assert!(
        library
            .edit_speakers(
                &meeting,
                &text,
                SpeakerEdit::Reassign {
                    segments: vec![text.segments[0].id],
                    speaker: "s2".into(),
                    name: None
                },
                false
            )
            .is_err()
    );
}

#[test]
fn correcting_a_wrong_suggestion_does_not_reuse_the_guessed_identity() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let meeting = library
        .create_meeting(MeetingMeta::new("Guess", Utc::now()))
        .unwrap();
    let text = transcript();
    meeting.write_transcript(&text).unwrap();
    library
        .write_meeting_voices(&meeting, BTreeMap::from([("s1".into(), vec![1.0, 0.0])]))
        .unwrap();
    let rename = |name: &str| SpeakerEdit::Rename {
        speaker: "s1".into(),
        name: name.into(),
        is_me: false,
        everywhere: false,
    };
    library
        .edit_speakers(&meeting, &text, rename("Иван"), true)
        .unwrap();
    let mut guessed = meeting.read_transcript().unwrap();
    let original_id = guessed.speaker_voice_ids["s1"].clone();
    guessed.speaker_names.remove("s1");
    guessed
        .speaker_suggestions
        .insert("s1".into(), "Иван".into());
    meeting.write_transcript(&guessed).unwrap();
    library
        .edit_speakers(&meeting, &guessed, rename("Мария"), true)
        .unwrap();
    let corrected = meeting.read_transcript().unwrap();
    assert_ne!(corrected.speaker_voice_ids["s1"], original_id);
    assert_eq!(corrected.speaker_name("s1"), "Мария");
    let voices = library.read_voices().unwrap();
    assert_eq!(voices.voices.len(), 2);
    assert_eq!(
        voices
            .voices
            .iter()
            .find(|v| v.id.as_ref() == Some(&original_id))
            .unwrap()
            .name,
        "Иван"
    );
}
