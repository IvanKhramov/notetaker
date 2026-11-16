use super::*;
use crate::{Channel, ExportFormat, MeetingMeta, Segment, Source, SpeakerId, SpeakerNameSource};

fn fixture() -> (
    tempfile::TempDir,
    Library,
    Meeting,
    Transcript,
    VoiceLibrary,
) {
    let root = tempfile::tempdir().unwrap();
    let lib = Library::open(root.path()).unwrap();
    let meeting = lib
        .create_meeting(MeetingMeta::new("Проверка голосов", Utc::now()))
        .unwrap();
    let mut t = Transcript::default();
    t.segments.push(Segment {
        id: uuid::Uuid::now_v7(),
        speaker: SpeakerId("s1".into()),
        channel: Channel::System,
        start_ms: 0,
        end_ms: 8000,
        text: "Обсудим нейтральный план проекта".into(),
        source: Source::Final,
        edited: false,
        lang: None,
    });
    let mut bank = VoiceLibrary::default();
    bank.enroll(
        VoiceKey::Stable("ktalk-a"),
        "Анна Петрова",
        false,
        &vec![1.0, 0.0],
    )
    .unwrap();
    write_json(&lib.private_state_dir().join("voices.json"), &bank).unwrap();
    lib.write_meeting_voices(&meeting, BTreeMap::from([("s1".into(), vec![0.8, 0.6])]))
        .unwrap();
    bank.recognize(
        &mut t,
        &BTreeMap::from([("s1".into(), vec![0.8, 0.6])]),
        true,
    );
    meeting.write_transcript(&t).unwrap();
    (root, lib, meeting, t, bank)
}

#[test]
fn automatic_names_survive_only_in_the_optional_field_and_all_views_resolve_them() {
    let (_root, lib, meeting, t, _) = fixture();
    assert!(t.speaker_names.is_empty());
    assert!(t.speaker_voice_ids.is_empty());
    assert!(matches!(
        t.speaker_identity("s1").unwrap().source,
        SpeakerNameSource::Voice {
            similarity_milli: 800
        }
    ));
    assert!(t.named_text().contains("Анна Петрова"));
    for format in ExportFormat::ALL {
        assert!(t.export(format).unwrap().contains("Анна Петрова"));
    }
    assert_eq!(t.speaker_timeline().speakers[0].name, "Анна Петрова");
    lib.reindex_incremental().unwrap();
    assert_eq!(lib.search("Петрова", 10).unwrap().len(), 1);
    let people = lib.people(false, "", "").unwrap();
    let person = people.iter().find(|p| p.name == "Анна Петрова").unwrap();
    assert!(lib.meeting_summaries(false).unwrap()[0].has_person(person));
    assert_eq!(
        lib.person_page(person.clone(), false)
            .unwrap()
            .meetings
            .len(),
        1
    );

    // Simulate a 0.32 writer retaining its known v1 fields and editing the text.
    let mut old = serde_json::to_value(&t).unwrap();
    old.as_object_mut().unwrap().retain(|key, _| {
        [
            "version",
            "segments",
            "bookmarks",
            "speaker_names",
            "speaker_suggestions",
            "speaker_voice_ids",
            "self_speaker",
            "ktalk_sources",
        ]
        .contains(&key.as_str())
    });
    old["segments"][0]["text"] = "Уточнённый нейтральный план".into();
    old["segments"][0]["edited"] = true.into();
    let rewritten = crate::migrate_transcript(old).unwrap();
    assert!(rewritten.speaker_names.is_empty());
    assert!(rewritten.voice_recognitions.is_empty());
    assert_eq!(rewritten.speaker_name("s1"), "s1");
    meeting.write_transcript(&rewritten).unwrap();
    lib.recognize_meeting(&meeting, true).unwrap();
    let restored = meeting.load_transcript().unwrap();
    assert_eq!(restored.value.speaker_name("s1"), "Анна Петрова");
    assert!(restored.value.speaker_names.is_empty());
    assert_eq!(restored.value.version, 1);
    lib.recognize_meeting(&meeting, true).unwrap();
    assert_eq!(
        meeting.load_transcript().unwrap().revision,
        restored.revision
    );
    assert_eq!(lib.read_voices().unwrap().voices[0].embeddings.len(), 1);
    // An old writer's unused own-name metadata must not reserve an actual speaker.
    let mut older = rewritten;
    older
        .speaker_names
        .insert("me".into(), "Анна Петрова".into());
    meeting.write_transcript(&older).unwrap();
    lib.recognize_meeting(&meeting, true).unwrap();
    assert_eq!(
        meeting.read_transcript().unwrap().speaker_name("s1"),
        "Анна Петрова"
    );
}

#[test]
fn rejected_imported_me_becomes_ordinary_and_stays_unnamed_after_reopening() {
    let (_root, lib, meeting, mut t, _) = fixture();
    t.segments[0].speaker.0 = "me".into();
    let mark = t.voice_recognitions.remove("s1").unwrap();
    t.voice_recognitions.insert("me".into(), mark);
    assert!(!t.default_self_name("Oleg Sidorov"));
    meeting.write_transcript(&t).unwrap();
    lib.write_meeting_voices(&meeting, BTreeMap::from([("me".into(), vec![0.8, 0.6])]))
        .unwrap();
    lib.edit_speakers(
        &meeting,
        &t,
        SpeakerEdit::RejectVoice {
            speaker: "me".into(),
        },
        true,
    )
    .unwrap();
    let mut reopened = meeting.read_transcript().unwrap();
    let id = reopened.segments[0].speaker.0.clone();
    assert_ne!(id, "me");
    assert!(reopened.self_speaker.is_none());
    assert!(reopened.voice_rejected.contains(&id));
    assert!(!reopened.default_self_name("Oleg Sidorov"));
    assert!(reopened.speaker_identity(&id).is_none());
    lib.recognize_meeting(&meeting, true).unwrap();
    let reopened = meeting.read_transcript().unwrap();
    assert!(reopened.speaker_identity(&id).is_none());
    assert!(!reopened.named_text().contains("Анна Петрова"));
    assert!(reopened.speaker_suggestions.contains_key(&id));
    assert!(!reopened.speaker_voice_ids.contains_key(&id));
}

#[test]
fn global_spelling_change_preserves_automatic_source_and_cannot_train_from_it() {
    let (_root, lib, other, automatic, _) = fixture();
    let meeting = lib
        .create_meeting(MeetingMeta::new("Подтверждённая встреча", Utc::now()))
        .unwrap();
    let mut confirmed = automatic.clone();
    confirmed.rename_speaker("s1", "Анна Петрова");
    meeting.write_transcript(&confirmed).unwrap();
    lib.edit_speakers(
        &meeting,
        &confirmed,
        SpeakerEdit::Rename {
            speaker: "s1".into(),
            name: "Анна".into(),
            is_me: false,
            everywhere: true,
        },
        true,
    )
    .unwrap();
    let after = other.read_transcript().unwrap();
    assert_eq!(after.speaker_name("s1"), "Анна");
    assert!(after.speaker_names.is_empty());
    assert_eq!(
        after.voice_recognitions["s1"].similarity_milli,
        automatic.voice_recognitions["s1"].similarity_milli
    );
    let path = lib.private_state_dir().join("voices.json");
    let before = fs::read(&path).unwrap();
    lib.remember_speakers(&other, &after).unwrap();
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn counter_compares_identities_instead_of_spelling_and_skips_unknown_links() {
    let (_root, lib, meeting, before, _) = fixture();
    let mut after = before.clone();
    after.segments[0].speaker.0 = "ktalk-a".into();
    after.rename_speaker("ktalk-a", "Анна");
    after
        .ktalk_sources
        .insert("ktalk-a".into(), "recording-a".into());
    lib.review_voice_names(&meeting, &before, &after).unwrap();
    lib.review_voice_names(&meeting, &before, &after).unwrap();
    assert_eq!(lib.read_voices().unwrap().recognition_counts(), (0, 1));
    after.segments[0].speaker.0 = "ktalk-b".into();
    after.rename_speaker("ktalk-b", "Анна Петрова");
    after
        .ktalk_sources
        .insert("ktalk-b".into(), "recording-b".into());
    lib.review_voice_names(&meeting, &before, &after).unwrap();
    assert_eq!(lib.read_voices().unwrap().recognition_counts(), (1, 2));
    assert!(
        lib.learn_recording_voices(
            "bad",
            vec![VoiceSample {
                speaker: "ktalk-owner".into(),
                name: "Oleg Sidorov".into(),
                embedding: vec![0.0, 1.0],
                own: true,
                recording_id: Some("bad".into())
            }],
            || false
        )
        .is_err()
    );
}

#[test]
fn confirming_an_own_name_does_not_train_own_voice_without_this_is_me() {
    let (_root, lib, meeting, t, mut bank) = fixture();
    bank.voices[0].is_me = true;
    let path = lib.private_state_dir().join("voices.json");
    write_json(&path, &bank).unwrap();
    let before = fs::read(&path).unwrap();
    lib.edit_speakers(
        &meeting,
        &t,
        SpeakerEdit::Rename {
            speaker: "s1".into(),
            name: "Анна Петрова".into(),
            is_me: false,
            everywhere: false,
        },
        true,
    )
    .unwrap();
    let confirmed = meeting.read_transcript().unwrap();
    assert!(confirmed.voice_recognitions.is_empty());
    assert!(confirmed.self_speaker.is_none());
    lib.remember_speakers(&meeting, &confirmed).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    lib.edit_speakers(
        &meeting,
        &confirmed,
        SpeakerEdit::Rename {
            speaker: "s1".into(),
            name: "Анна Петрова".into(),
            is_me: true,
            everywhere: false,
        },
        true,
    )
    .unwrap();
    assert_eq!(lib.read_voices().unwrap().voices[0].embeddings.len(), 2);
}
