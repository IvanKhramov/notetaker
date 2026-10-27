use crate::calendar::{Attendee, EventLink};
use crate::*;
use chrono::{TimeZone, Utc};
fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap()
}
fn add(
    library: &Library,
    title: &str,
    days: i64,
    uid: Option<&str>,
    name: &str,
    address: &str,
    voice: Option<&str>,
) -> Meeting {
    let mut meta = MeetingMeta::new(title, now() + chrono::Duration::days(days));
    if let Some(uid) = uid {
        meta.calendar_event = Some(EventLink {
            uid: uid.into(),
            title: title.into(),
            attendees: vec![Attendee {
                name: name.into(),
                email: address.into(),
            }],
            call_url: None,
        });
    }
    let meeting = library.create_meeting(meta).unwrap();
    let mut transcript = Transcript::default();
    if !name.is_empty() {
        transcript.rename_speaker("s1", name);
    }
    if let Some(voice) = voice {
        transcript
            .speaker_voice_ids
            .insert("s1".into(), voice.into());
    }
    meeting.write_transcript(&transcript).unwrap();
    meeting
}
#[test]
fn person_selection_email_speaker_voice_and_persisted_edits() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let first = add(&library, "A", -3, Some("event"), "Анна", "a@x", None);
    add(&library, "B", -2, None, "анна", "", Some("v"));
    add(&library, "C", -1, None, "", "", Some("v"));
    add(&library, "D", 0, None, "Иван", "", None);
    first
        .write_enhanced(
            "- [ ] Проверить - Анна - завтра | Попросил: Иван\n- [x] Готово - Анна - сегодня",
        )
        .unwrap();
    let people = library.people(false, "", "").unwrap();
    let anna = people.iter().find(|p| p.matches("a@x")).unwrap().clone();
    let page = library.person_page(anna.clone(), false).unwrap();
    assert_eq!(page.meetings.len(), 3);
    assert_eq!(page.owned_tasks.len(), 1);
    let ivan = people.iter().find(|p| p.matches("Иван")).unwrap().clone();
    assert_eq!(
        library
            .person_page(ivan, false)
            .unwrap()
            .requested_tasks
            .len(),
        1
    );
    let mut edits = library.read_people_edits().unwrap();
    edits.rename(&anna, "Анна Новая");
    edits.hidden.insert(anna.id.clone());
    library.write_people_edits(&edits).unwrap();
    library.reindex_full().unwrap();
    let people = library.people(false, "", "").unwrap();
    let anna = people.iter().find(|p| p.id == anna.id).unwrap();
    assert_eq!(anna.name, "Анна Новая");
    assert!(anna.hidden);
    assert!(anna.matches("анна"));
    add(&library, "Новое имя", 1, None, "Анна Новая", "", None);
    let people = library.people(false, "", "").unwrap();
    assert_eq!(people.iter().filter(|p| p.matches("Анна Новая")).count(), 1);
}
#[test]
fn recurring_uid_heuristic_and_current_open_task_carry_over() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let first = add(&library, "План", -4, Some("rrule"), "Иван", "i@x", None);
    first.write_enhanced("## Решения\nПроверить релиз\n## Задачи\n- [ ] Сделать - Иван - завтра\n- [x] Готово - Иван - вчера").unwrap();
    let next = add(
        &library,
        "Изменённое название",
        0,
        Some("rrule"),
        "Иван",
        "i@x",
        None,
    );
    add(&library, "Ретро", -2, None, "ИВАН", "", None);
    add(&library, "ретро", -1, None, "Иван", "", None);
    add(&library, "ретро", 0, None, "Анна", "", None);
    let series = library.series(false).unwrap();
    let recurring = series.iter().find(|s| s.id == "uid:rrule").unwrap();
    assert_eq!(recurring.meetings.len(), 2);
    assert!(
        series
            .iter()
            .any(|s| s.title == "ретро" && s.meetings.len() == 2)
    );
    let carry = library
        .carry_over(recurring, next.read_meta().unwrap().started_at, false)
        .unwrap()
        .unwrap();
    assert_eq!(carry.tasks.len(), 1);
    assert!(carry.decisions.contains("Проверить релиз"));
    assert!(!carry.decisions.contains("Задачи"));
    first.toggle_task(&carry.tasks[0], true).unwrap();
    library.reindex_incremental().unwrap();
    assert!(
        library
            .carry_over(recurring, now(), false)
            .unwrap()
            .unwrap()
            .tasks
            .is_empty()
    );
}
#[test]
fn digest_iso_year_local_dates_and_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let zone = calendar::timezone("Europe/Moscow").unwrap();
    let period = DigestPeriod::week(zone.with_ymd_and_hms(2021, 1, 1, 12, 0, 0).unwrap(), false);
    assert_eq!(period.filename(), "2020-W53.md");
    assert_eq!(
        DigestPeriod::week(zone.with_ymd_and_hms(2021, 1, 4, 0, 0, 0).unwrap(), true),
        period
    );
    let at = Utc.with_ymd_and_hms(2021, 1, 3, 21, 0, 0).unwrap();
    assert!(!period.contains(at, &zone));
    assert!(period.contains(at - chrono::Duration::seconds(1), &zone));
    library
        .create_meeting(MeetingMeta::new("Boundary", at))
        .unwrap();
    let mut digest = library
        .digest(period, &DigestFilter::default(), &zone, false)
        .unwrap();
    assert!(digest.meetings.is_empty());
    let path = library.save_digest(&digest).unwrap();
    digest.markdown = "updated".into();
    assert_eq!(library.save_digest(&digest).unwrap(), path);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "updated");
    assert_eq!(
        std::fs::read_dir(dir.path().join(".digests"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn ics_recurrences_keep_series_uid_and_names_do_not_guess() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let events = calendar::parse_ics("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:recurring\nDTSTART:20261026T120000Z\nDTEND:20261026T130000Z\nSUMMARY:План\nRRULE:FREQ=DAILY;COUNT=2\nATTENDEE;CN=Анна:mailto:a@example.org\nEND:VEVENT\nEND:VCALENDAR", now() - chrono::Duration::days(2), now() + chrono::Duration::days(1), calendar::timezone("UTC").unwrap()).unwrap();
    assert_eq!(events.len(), 2);
    for event in events {
        let mut meta = MeetingMeta::new("План", event.start);
        meta.calendar_event = Some(event.link);
        library.create_meeting(meta).unwrap();
    }
    let series = library.series(false).unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].meetings.len(), 2);
    assert!(library.series(true).unwrap().is_empty());
}
#[test]
fn person_timing_unions_segments_across_explicitly_merged_speakers() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let meeting = add(&library, "Разговор", 0, None, "Анна", "", Some("voice"));
    let mut transcript = meeting.read_transcript().unwrap();
    transcript.rename_speaker("s2", "Анна");
    for (speaker, start_ms, end_ms) in [("s1", 0, 1000), ("s2", 500, 1500)] {
        transcript.segments.push(Segment {
            id: uuid::Uuid::now_v7(),
            speaker: SpeakerId(speaker.into()),
            channel: Channel::System,
            start_ms,
            end_ms,
            text: "Да".into(),
            source: Source::Final,
            edited: false,
        });
    }
    meeting.write_transcript(&transcript).unwrap();
    let person = library.people(false, "", "").unwrap().pop().unwrap();
    let page = library.person_page(person, false).unwrap();
    assert_eq!(page.talk_ms, 1500);
    assert_eq!(page.segment_count, 2);
    assert_eq!(page.turns, 1);
}

#[test]
fn user_aliases_attach_to_the_existing_email_identity() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    add(
        &library,
        "Календарь",
        -1,
        Some("uid"),
        "Александр",
        "a@example.org",
        None,
    );
    add(&library, "Разговор", 0, None, "Саша", "", Some("v1"));
    let people = library.people(false, "Александр", "Саша").unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].id, "email:a@example.org");
    assert!(people[0].matches("Саша"));
    assert_eq!(
        library
            .person_page(people[0].clone(), false)
            .unwrap()
            .meetings
            .len(),
        2
    );
}

#[test]
fn carry_insertion_is_explicit_revision_checked_and_preserves_local_policy() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let previous = add(&library, "План", -1, Some("series"), "Анна", "a@x", None);
    previous
        .write_enhanced("## Решения\nLOCAL DECISION\n- [ ] LOCAL TASK - Анна - завтра")
        .unwrap();
    let next = add(&library, "План", 0, Some("series"), "Анна", "a@x", None);
    let mut meta = next.read_meta().unwrap();
    meta.mode = Mode::Hybrid;
    next.write_meta(&meta).unwrap();
    next.write_notes("Мои заметки").unwrap();
    let revision = next.load_notes().unwrap().revision;
    let series = library.series(false).unwrap().pop().unwrap();
    assert!(library.carry_over(&series, now(), false).unwrap().is_some());
    assert_eq!(next.read_notes().unwrap(), "Мои заметки");
    assert_eq!(next.read_meta().unwrap().mode, Mode::Hybrid);
    next.write_notes("Внешняя правка").unwrap();
    assert!(
        library
            .insert_carry_over(&next, previous.read_meta().unwrap().id, revision)
            .is_err()
    );
    assert_eq!(next.read_meta().unwrap().mode, Mode::Hybrid);
    library
        .insert_carry_over(
            &next,
            previous.read_meta().unwrap().id,
            next.load_notes().unwrap().revision,
        )
        .unwrap();
    assert_eq!(next.read_meta().unwrap().mode, Mode::Local);
    assert!(next.read_notes().unwrap().contains("LOCAL TASK"));
    assert!(next.read_notes().unwrap().ends_with("Внешняя правка"));
}

#[test]
fn user_edits_follow_a_new_email_and_can_be_unhidden() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    add(&library, "Без календаря", -1, None, "Анна", "", None);
    let person = library.people(false, "", "").unwrap().pop().unwrap();
    let mut edits = library.read_people_edits().unwrap();
    edits.rename(&person, "Анна Новая");
    edits.set_hidden(&person, true);
    library.write_people_edits(&edits).unwrap();
    add(
        &library,
        "С календарём",
        0,
        Some("uid"),
        "Анна",
        "a@x",
        None,
    );
    let people = library.people(false, "", "").unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].name, "Анна Новая");
    assert!(people[0].hidden);
    assert_eq!(people[0].id, "email:a@x");
    edits.set_hidden(&people[0], false);
    library.write_people_edits(&edits).unwrap();
    assert!(!library.people(false, "", "").unwrap()[0].hidden);
}
