use super::super::client::{Chunk, Participant, Track, Word};
use super::*;
use notetaker_core::{Channel, MeetingMeta, Segment, Source, SpeakerId};
use uuid::Uuid;
fn start() -> DateTime<Utc> {
    "2026-01-01T12:00:00Z".parse().unwrap()
}
fn recording(id: &str, delta: i64, duration: u32) -> Recording {
    Recording {
        id: id.into(),
        title: format!("Встреча {id}"),
        created_date: start() + chrono::Duration::seconds(delta),
        duration,
        status: "complete".into(),
    }
}
fn owner() -> User {
    User {
        key: "owner".into(),
        firstname: "Oleg".into(),
        surname: "Sidorov".into(),
    }
}
fn participant(index: usize) -> Participant {
    if index == 0 {
        Participant {
            is_anonymous: false,
            anonymous_id: None,
            anonymous_name: None,
            user_info: Some(User {
                key: "anna".into(),
                firstname: "Анна".into(),
                surname: "Петрова".into(),
            }),
        }
    } else {
        Participant {
            is_anonymous: true,
            anonymous_id: Some("guest".into()),
            anonymous_name: Some("Oleg Sidorov (комп)".into()),
            user_info: None,
        }
    }
}
fn fixture(offset: i64) -> (Local, Transcript) {
    let mut local = Local::default();
    let mut remote = Transcript {
        status: "complete".into(),
        tracks: (0..3)
            .map(|i| Track {
                speaker: participant(i),
                chunks: vec![Chunk { words: vec![] }],
            })
            .collect(),
    };
    // A single long chunk per participant; third track is silent. Irregular gaps
    // prevent periodic aliases. Word boundaries have deterministic timing jitter.
    for (i, t) in [
        120_000, 127_300, 139_900, 156_100, 178_600, 211_700, 250_200,
    ]
    .into_iter()
    .enumerate()
    {
        let text = format!("Обсуждаем вопрос номер {i} сегодня вместе");
        local.segments.push(Segment {
            id: Uuid::now_v7(),
            speaker: SpeakerId("s1".into()),
            channel: Channel::System,
            start_ms: t as u64,
            end_ms: (t + 3000) as u64,
            text: text.clone(),
            source: Source::Final,
            edited: false,
            lang: None,
        });
        for (j, word) in text.split_whitespace().enumerate() {
            remote.tracks[i % 2].chunks[0].words.push(Word {
                text: word.into(),
                start_time_offset_in_millis: t + offset + j as i64 * 500 + 15,
                end_time_offset_in_millis: t + offset + (j + 1) as i64 * 500 - 15,
            });
        }
    }
    (local, remote)
}
#[test]
fn overlap_matching_ambiguity_none_and_complete_only() {
    assert_eq!(
        match_recording(&[recording("a", 30, 100)], start(), 100_000)
            .unwrap()
            .id,
        "a"
    );
    assert!(match_recording(&[recording("a", 51, 100)], start(), 100_000).is_err());
    assert!(
        match_recording(
            &[recording("a", 0, 100), recording("b", 5, 100)],
            start(),
            100_000
        )
        .unwrap_err()
        .to_string()
        .contains("Неоднозначное")
    );
    assert_eq!(
        match_recording(
            &[recording("a", 0, 100), recording("b", 20, 100)],
            start(),
            100_000
        )
        .unwrap()
        .id,
        "a"
    );
    let mut r = recording("a", 0, 100);
    r.status = "processing".into();
    assert!(match_recording(&[r], start(), 100_000).is_err());
    assert!(match_recording(&[], start(), 100_000).is_err());
    assert!(
        match_recording(
            &[recording("long", 600, 900), recording("short", 0, 300)],
            start(),
            1_000_000
        )
        .is_err()
    );
}
#[test]
fn aligns_positive_and_negative_offsets_and_rejects_unrelated_text() {
    for offset in [37_000, -95_000] {
        let (local, remote) = fixture(offset);
        let p = plan(&local, start(), recording("a", 0, 400), &remote, &owner()).unwrap();
        assert!((p.offset_ms - offset).abs() <= 100, "{}", p.offset_ms);
        assert!(p.validation_score > 0.95);
        let mut unrelated = local;
        for s in &mut unrelated.segments {
            s.text = "Совершенно чужой текст без совпадений".into();
        }
        assert_eq!(
            plan(
                &unrelated,
                start(),
                recording("a", 0, 400),
                &remote,
                &owner()
            )
            .err()
            .unwrap()
            .to_string(),
            "не удалось надёжно сопоставить время"
        );
    }
}
#[test]
fn alternating_tracks_fine_words_guests_confirmed_and_manual_edits() {
    let (mut local, remote) = fixture(37_000);
    local.segments[2].speaker.0 = "confirmed".into();
    local
        .speaker_names
        .insert("confirmed".into(), "Моё имя".into());
    local.segments[3].edited = true;
    let p = plan(&local, start(), recording("a", 0, 400), &remote, &owner()).unwrap();
    assert_ne!(p.after.segments[0].speaker, p.after.segments[1].speaker);
    assert_eq!(
        p.after.speaker_name(&p.after.segments[0].speaker.0),
        "Анна Петрова"
    );
    assert_eq!(
        p.after.speaker_name(&p.after.segments[1].speaker.0),
        "Oleg Sidorov (комп)"
    );
    assert_eq!(p.after.segments[2], local.segments[2]);
    assert_eq!(p.after.segments[3], local.segments[3]);
    assert!(!p.after.ktalk_sources.is_empty());
}
#[test]
fn equal_split_unchanged_and_me_named_only_when_unconfirmed() {
    let (mut local, mut remote) = fixture(0);
    let words = remote.tracks[0].chunks[0].words[..6].to_vec();
    remote.tracks[0].chunks[0].words.drain(..6);
    remote.tracks[0].chunks[0]
        .words
        .extend(words[..3].iter().cloned());
    remote.tracks[1].chunks[0]
        .words
        .extend(words[3..].iter().cloned());
    local.segments[1].speaker.0 = "me".into();
    let p = plan(&local, start(), recording("a", 0, 400), &remote, &owner()).unwrap();
    assert_eq!(p.after.segments[0].speaker.0, "s1");
    assert_eq!(p.after.speaker_name("me"), "Oleg Sidorov");
    local.speaker_names.insert("me".into(), "Своё имя".into());
    let p = plan(&local, start(), recording("a", 0, 400), &remote, &owner()).unwrap();
    assert_eq!(p.after.speaker_name("me"), "Своё имя");
}
#[test]
fn old_file_read_no_rewrite_idempotent_revision_stale_summary_and_search() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path()).unwrap();
    let meeting = lib
        .create_meeting(MeetingMeta::new("Проверка", start()))
        .unwrap();
    let (local, remote) = fixture(37_000);
    let json = serde_json::to_vec_pretty(&local).unwrap();
    assert!(!String::from_utf8_lossy(&json).contains("ktalk_sources"));
    std::fs::write(meeting.path().join("transcript.json"), &json).unwrap();
    let loaded = meeting.load_transcript().unwrap();
    assert_eq!(
        std::fs::read(meeting.path().join("transcript.json")).unwrap(),
        json
    );
    let stamp = notetaker_core::SummaryStamp::new("", &local, "", "");
    let p = plan(
        &loaded.value,
        start(),
        recording("a", 0, 400),
        &remote,
        &owner(),
    )
    .unwrap();
    apply(&lib, &meeting, &p).unwrap();
    let saved = meeting.load_transcript().unwrap();
    assert!(stamp.stale("", &saved.value, ""));
    assert_eq!(lib.search("Петрова", 10).unwrap().len(), 1);
    let p2 = plan(
        &saved.value,
        start(),
        recording("a", 0, 400),
        &remote,
        &owner(),
    )
    .unwrap();
    assert_eq!(p2.before, p2.after);
    apply(&lib, &meeting, &p2).unwrap();
    assert_eq!(meeting.load_transcript().unwrap().revision, saved.revision);
    // A concurrent text edit invalidates a prepared attribution plan.
    meeting
        .update_transcript(|t| t.segments[0].text = "Правка".into())
        .unwrap();
    assert!(apply(&lib, &meeting, &p).is_err());
}

#[test]
fn text_threshold_accepts_half_tokens_and_normalizes_case_punctuation() {
    let (mut local, remote) = fixture(37_000);
    for segment in &mut local.segments {
        segment.text = "ОБСУЖДАЕМ, вопрос! номер. другой чужой пример".into();
    }
    let p = plan(&local, start(), recording("a", 0, 400), &remote, &owner()).unwrap();
    assert!((p.validation_score - 0.5).abs() < 0.001);
    for segment in &mut local.segments {
        segment.text = "Обсуждаем вопрос чужой иной новый пример".into();
    }
    assert!(plan(&local, start(), recording("a", 0, 400), &remote, &owner()).is_err());
}
