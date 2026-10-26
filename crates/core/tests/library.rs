use std::{
    fs,
    time::{Duration, SystemTime},
};

use chrono::{TimeZone, Utc};
use notetaker_core::{
    Channel, Error, Library, MeetingMeta, Mode, SCHEMA_VERSION, Segment, Source, SpeakerId,
    Transcript, migrate_transcript,
};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture() -> (TempDir, Library) {
    let directory = tempfile::tempdir().unwrap();
    let library = Library::open(directory.path()).unwrap();
    (directory, library)
}

fn meta(title: &str) -> MeetingMeta {
    MeetingMeta::new(
        title,
        Utc.with_ymd_and_hms(2026, 10, 26, 15, 30, 0).unwrap(),
    )
}

fn transcript(text: &str) -> Transcript {
    Transcript {
        version: SCHEMA_VERSION,
        segments: vec![
            Segment {
                id: Uuid::now_v7(),
                speaker: SpeakerId("future-speaker-42".into()),
                channel: Channel::System,
                start_ms: 120,
                end_ms: 780,
                text: text.into(),
                source: Source::Final,
                edited: true,
            },
            Segment {
                id: Uuid::now_v7(),
                speaker: SpeakerId("me".into()),
                channel: Channel::Mic,
                start_ms: 800,
                end_ms: 1100,
                text: "live segment".into(),
                source: Source::Live,
                edited: false,
            },
        ],
    }
}

#[test]
fn roundtrip_and_atomic_replacement() {
    let (_directory, library) = fixture();
    let mut expected = meta("Обсуждение проекта");
    expected.ended_at = Some(expected.started_at + chrono::Duration::hours(1));
    expected.app = Some("Teams".into());
    expected.mode = Mode::Hybrid;
    expected.template = Some("standup".into());
    expected.audio_expires_at = Some(expected.started_at + chrono::Duration::days(7));
    assert_eq!(expected.id.get_version_num(), 7);
    let defaults = meta("Defaults");
    assert_eq!(
        defaults.audio_expires_at,
        Some(defaults.started_at + chrono::Duration::days(7))
    );
    let meeting = library.create_meeting(expected.clone()).unwrap();
    assert_eq!(meeting.read_meta().unwrap(), expected);
    assert_eq!(meeting.read_transcript().unwrap(), Transcript::default());
    assert_eq!(meeting.read_notes().unwrap(), "");
    assert_eq!(meeting.read_enhanced().unwrap(), "");
    meeting.write_notes("# Заметки\nТекст").unwrap();
    meeting.write_notes("# Updated notes\nПравки").unwrap();
    meeting.write_enhanced("# Summary\nИтоги").unwrap();
    let expected_transcript = transcript("Решение принято");
    meeting.write_transcript(&expected_transcript).unwrap();
    expected.title = "Новое название".into();
    meeting.write_meta(&expected).unwrap();
    let reopened = Library::open(library.root())
        .unwrap()
        .list_meetings()
        .unwrap()
        .remove(0);
    assert_eq!(reopened.read_meta().unwrap(), expected);
    assert_eq!(reopened.read_notes().unwrap(), "# Updated notes\nПравки");
    assert_eq!(reopened.read_enhanced().unwrap(), "# Summary\nИтоги");
    assert_eq!(reopened.read_transcript().unwrap(), expected_transcript);
    let yaml = fs::read_to_string(meeting.path().join("meta.yaml")).unwrap();
    assert!(yaml.contains("mode: hybrid"));
    assert!(yaml.contains("2026-10-26T15:30:00Z"));
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(meeting.path().join("transcript.json")).unwrap())
            .unwrap();
    assert_eq!(json["segments"][0]["speaker"], "future-speaker-42");
    assert_eq!(json["segments"][0]["channel"], "system");
    assert_eq!(json["segments"][1]["source"], "live");
    assert_eq!(
        meeting.mic_audio_path(),
        meeting.path().join("audio/mic.opus")
    );
    assert_eq!(
        meeting.system_audio_path(),
        meeting.path().join("audio/system.opus")
    );
    assert!(!meeting.mic_audio_path().exists());
    assert_eq!(fs::read_dir(meeting.path()).unwrap().count(), 5);
}

#[test]
fn slug_transliterates_and_resolves_collisions() {
    let (_directory, library) = fixture();
    let first = library.create_meeting(meta("Привет, мир!")).unwrap();
    let second = library.create_meeting(meta("Привет, мир!")).unwrap();
    assert_eq!(
        first.path().file_name().unwrap(),
        "2026-10-26_1530_privet-mir"
    );
    assert_eq!(
        second.path().file_name().unwrap(),
        "2026-10-26_1530_privet-mir-1"
    );
    let empty = library.create_meeting(meta("... / \\ ")).unwrap();
    assert_eq!(empty.path().file_name().unwrap(), "2026-10-26_1530_meeting");
    assert_eq!(library.list_meetings().unwrap().len(), 3);
}

#[test]
fn migration_rejects_unknown_versions_before_decoding_segments() {
    let original = transcript("schema v1");
    assert_eq!(
        migrate_transcript(serde_json::to_value(&original).unwrap()).unwrap(),
        original
    );
    for version in [0, SCHEMA_VERSION + 1] {
        assert!(
            matches!(migrate_transcript(serde_json::json!({"version": version, "segments": "future format"})), Err(Error::UnsupportedVersion(found)) if found == version)
        );
    }
    let (_directory, library) = fixture();
    let meeting = library.create_meeting(meta("Versioning")).unwrap();
    let mut future = original;
    future.version += 1;
    assert!(matches!(
        meeting.write_transcript(&future),
        Err(Error::UnsupportedVersion(_))
    ));
    fs::write(meeting.path().join("transcript.json"), "{\"version\":2}").unwrap();
    assert!(matches!(
        meeting.read_transcript(),
        Err(Error::UnsupportedVersion(2))
    ));
}

#[test]
fn search_indexes_all_sources_and_escapes_user_input() {
    let (_directory, library) = fixture();
    let meeting = library.create_meeting(meta("Архитектура roadmap")).unwrap();
    meeting.write_notes("Разработка release café").unwrap();
    meeting.write_enhanced("Результаты summary").unwrap();
    meeting
        .write_transcript(&transcript("Совещание deployment"))
        .unwrap();
    let other = library.create_meeting(meta("Unrelated")).unwrap();
    other.write_notes("Roadmap secondary").unwrap();
    library.reindex_full().unwrap();
    for query in [
        "архитектура",
        "разработка",
        "результаты",
        "совещание",
        "release",
        "summary",
        "deployment",
        "разраб",
        "depl",
        "cafe",
        "разработка release",
        "\"разраб\"*",
        "release); --",
    ] {
        let hits = library.search(query, 10).unwrap();
        assert_eq!(hits.len(), 1, "query: {query}");
        assert_eq!(hits[0].meeting_id, meeting.read_meta().unwrap().id);
        assert_eq!(hits[0].path, meeting.path());
        assert_eq!(hits[0].title, "Архитектура roadmap");
        assert!(hits[0].snippet.contains('['), "query: {query}");
    }
    assert_eq!(library.search("road", 1).unwrap().len(), 1);
    assert_eq!(library.search("road", 10).unwrap().len(), 2);
    for query in ["", "   \"*(): -", "release OR unrelated", "NOT", "missing"] {
        assert!(
            library.search(query, 10).unwrap().is_empty(),
            "query: {query}"
        );
    }
    assert!(library.search("release", 0).unwrap().is_empty());
}

#[test]
fn deleted_index_rebuilds_even_with_same_library_handle() {
    let (_directory, library) = fixture();
    library.create_meeting(meta("Rebuildable индекс")).unwrap();
    library.reindex_full().unwrap();
    let expected = library.search("индекс", 10).unwrap();
    assert_eq!(expected.len(), 1);
    fs::remove_file(library.index_path()).unwrap();
    library.reindex_full().unwrap();
    assert_eq!(library.search("индекс", 10).unwrap(), expected);
    fs::remove_file(library.index_path()).unwrap();
    library.reindex_incremental().unwrap();
    assert_eq!(library.search("индекс", 10).unwrap(), expected);
}

#[test]
fn incremental_tracks_each_file_and_removes_missing_meetings() {
    let (_directory, library) = fixture();
    let meeting = library.create_meeting(meta("Incremental")).unwrap();
    meeting.write_notes("before").unwrap();
    library.reindex_incremental().unwrap();
    library.reindex_incremental().unwrap();
    assert_eq!(library.search("before", 10).unwrap().len(), 1);
    // External edit with identical length and an explicitly advanced mtime.
    let notes = meeting.path().join("notes.md");
    fs::write(&notes, "after!").unwrap();
    fs::File::options()
        .write(true)
        .open(&notes)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    library.reindex_incremental().unwrap();
    assert!(library.search("before", 10).unwrap().is_empty());
    assert_eq!(library.search("after", 10).unwrap().len(), 1);
    meeting.write_enhanced("enhancedword").unwrap();
    meeting
        .write_transcript(&transcript("transcriptword"))
        .unwrap();
    let mut metadata = meeting.read_meta().unwrap();
    metadata.title = "renamedword".into();
    meeting.write_meta(&metadata).unwrap();
    library.reindex_incremental().unwrap();
    for query in ["enhancedword", "transcriptword", "renamedword"] {
        assert_eq!(library.search(query, 10).unwrap().len(), 1);
    }
    fs::remove_file(notes).unwrap();
    library.reindex_incremental().unwrap();
    assert!(library.search("after", 10).unwrap().is_empty());
    fs::remove_dir_all(meeting.path()).unwrap();
    library.reindex_incremental().unwrap();
    assert!(library.search("renamedword", 10).unwrap().is_empty());
}

#[test]
fn invalid_meetings_are_skipped_and_removed_from_index() {
    let (_directory, library) = fixture();
    let valid = library.create_meeting(meta("Healthy")).unwrap();
    let corrupt = library.create_meeting(meta("Corrupted")).unwrap();
    library.reindex_full().unwrap();
    fs::write(corrupt.path().join("meta.yaml"), "title: [broken yaml").unwrap();
    fs::create_dir(library.root().join("incomplete")).unwrap();
    let meetings = library.list_meetings().unwrap();
    assert_eq!(meetings.len(), 1);
    assert_eq!(meetings[0].path(), valid.path());
    library.reindex_incremental().unwrap();
    assert!(library.search("corrupted", 10).unwrap().is_empty());
    assert_eq!(library.search("healthy", 10).unwrap().len(), 1);
}

#[test]
fn sweep_removes_only_expired_audio_and_is_idempotent() {
    let (_directory, library) = fixture();
    let now = meta("clock").started_at;
    let mut expired = meta("Expired");
    expired.ended_at = Some(now);
    expired.audio_expires_at = Some(now);
    let expired = library.create_meeting(expired).unwrap();
    let mut future = meta("Future");
    future.ended_at = Some(now);
    future.audio_expires_at = Some(now + chrono::Duration::days(1));
    let future = library.create_meeting(future).unwrap();
    let mut forever = meta("Forever");
    forever.audio_expires_at = None;
    let forever = library.create_meeting(forever).unwrap();
    let mut missing = meta("Missing audio");
    missing.audio_expires_at = Some(now - chrono::Duration::days(1));
    let missing = library.create_meeting(missing).unwrap();
    fs::remove_dir(missing.path().join("audio")).unwrap();
    for meeting in [&expired, &future, &forever] {
        fs::write(meeting.mic_audio_path(), b"mic").unwrap();
        fs::write(meeting.system_audio_path(), b"system").unwrap();
    }
    fs::write(expired.path().join("audio/other.opus"), b"audio").unwrap();
    let mut expected = vec![
        expired.mic_audio_path(),
        expired.system_audio_path(),
        expired.path().join("audio/other.opus"),
    ];
    expected.sort();
    assert_eq!(library.sweep_audio(now).unwrap(), expected);
    assert!(library.sweep_audio(now).unwrap().is_empty());
    assert!(future.mic_audio_path().exists());
    assert!(forever.system_audio_path().exists());
    assert!(expired.path().join("meta.yaml").exists());
    assert!(expired.path().join("transcript.json").exists());
    library
        .sweep_audio(now + chrono::Duration::days(8))
        .unwrap();
    assert!(!future.mic_audio_path().exists());
    assert!(forever.mic_audio_path().exists());
}

#[test]
fn failed_rebuild_keeps_previous_index_transactionally() {
    let (_directory, library) = fixture();
    let meeting = library.create_meeting(meta("Original")).unwrap();
    library.reindex_full().unwrap();
    meeting.write_notes("newword").unwrap();
    fs::write(meeting.path().join("transcript.json"), "invalid json").unwrap();
    assert!(library.reindex_full().is_err());
    assert_eq!(library.search("original", 10).unwrap().len(), 1);
    assert!(library.search("newword", 10).unwrap().is_empty());
}

#[test]
fn stale_saves_preserve_external_edits_and_keep_local_conflict_copies() {
    let (_dir, library) = fixture();
    let meeting = library.create_meeting(meta("Conflicts")).unwrap();
    let notes = meeting.load_notes().unwrap();
    let enhanced = meeting.load_enhanced().unwrap();
    let transcript_revision = meeting.load_transcript().unwrap().revision;
    meeting.write_notes("external notes").unwrap();
    meeting.write_enhanced("external summary").unwrap();
    let external_transcript = transcript("external transcript");
    meeting.write_transcript(&external_transcript).unwrap();
    let local_transcript = transcript("local transcript");
    for (result, expected) in [
        (
            meeting.save_notes("local notes", notes.revision),
            b"local notes".to_vec(),
        ),
        (
            meeting.save_enhanced("local summary", enhanced.revision),
            b"local summary".to_vec(),
        ),
        (
            meeting.save_transcript(&local_transcript, transcript_revision),
            serde_json::to_vec_pretty(&local_transcript).unwrap(),
        ),
    ] {
        let Error::Conflict { path, local } = result.unwrap_err() else {
            panic!("expected Conflict")
        };
        assert_eq!(fs::read(&local).unwrap(), expected);
        assert_eq!(path.extension(), local.extension());
        assert!(
            local
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains(".conflict-")
        );
    }
    assert_eq!(meeting.read_notes().unwrap(), "external notes");
    assert_eq!(meeting.read_enhanced().unwrap(), "external summary");
    assert_eq!(meeting.read_transcript().unwrap(), external_transcript);
    // Hashes detect edits even if length and mtime have been preserved.
    let loaded = meeting.load_notes().unwrap();
    let path = meeting.path().join("notes.md");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, "external NOTES").unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert!(matches!(
        meeting.save_notes("mine", loaded.revision),
        Err(Error::Conflict { .. })
    ));
    let loaded = meeting.load_notes().unwrap();
    let revision = meeting.save_notes("fresh", loaded.revision).unwrap();
    assert_eq!(meeting.load_notes().unwrap().revision, revision);
}

#[test]
fn transcript_updates_serialize_across_separately_opened_meeting_handles() {
    let (_dir, library) = fixture();
    let meeting = library.create_meeting(meta("Concurrent")).unwrap();
    let mut threads = Vec::new();
    for _ in 0..4 {
        let handle = Library::open(library.root())
            .unwrap()
            .list_meetings()
            .unwrap()
            .remove(0);
        threads.push(std::thread::spawn(move || {
            for _ in 0..20 {
                handle
                    .update_transcript(|value| {
                        std::thread::yield_now();
                        value.segments.push(transcript("append").segments.remove(0));
                    })
                    .unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(meeting.read_transcript().unwrap().segments.len(), 80);
    let external = transcript("external");
    let error = meeting
        .update_transcript(|local| {
            local.segments.clear();
            fs::write(
                meeting.path().join("transcript.json"),
                serde_json::to_vec(&external).unwrap(),
            )
            .unwrap();
        })
        .unwrap_err();
    assert!(matches!(error, Error::Conflict { .. }));
    assert_eq!(meeting.read_transcript().unwrap(), external);
}

#[test]
fn sweep_includes_failed_final_work_but_never_active_recordings() {
    let (_dir, library) = fixture();
    let now = meta("clock").started_at;
    let mut expired = meta("Failed final");
    expired.ended_at = Some(now);
    expired.audio_expires_at = Some(now);
    let expired = library.create_meeting(expired).unwrap();
    let mut active = meta("Active");
    active.audio_expires_at = Some(now);
    let active = library.create_meeting(active).unwrap();
    for meeting in [&expired, &active] {
        fs::create_dir(meeting.path().join("audio/.work")).unwrap();
        fs::write(meeting.path().join("audio/.work/mic.wav"), b"pcm").unwrap();
        fs::write(meeting.mic_audio_path(), b"opus").unwrap();
    }
    library.sweep_audio(now).unwrap();
    assert!(!expired.path().join("audio/.work").exists());
    assert!(!expired.mic_audio_path().exists());
    assert!(active.path().join("audio/.work/mic.wav").exists());
    assert!(active.mic_audio_path().exists());
    assert!(library.sweep_audio(now).unwrap().is_empty());
}
