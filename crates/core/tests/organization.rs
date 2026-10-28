use chrono::{TimeZone, Utc};
use notetaker_core::{
    Library, MeetingMeta, Mode,
    folders::{CalendarRule, FolderSettings, matches_folder},
    sharing::{MergeChoice, likely_synced},
};

#[test]
fn inheritance_rules_series_selection_defaults_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path()).unwrap();
    let mut folders = library.load_folders().unwrap();
    folders.value.insert(
        "client".into(),
        FolderSettings {
            mode: Some(Mode::Hybrid),
            template: Some("standup".into()),
            rules: vec![CalendarRule {
                title_contains: "Weekly".into(),
                attendee_domain: "client.ru".into(),
            }],
            ..Default::default()
        },
    );
    library
        .save_folders(&folders.value, folders.revision)
        .unwrap();
    let mut meta = MeetingMeta::new(
        "Weekly",
        Utc.with_ymd_and_hms(2026, 10, 1, 10, 0, 0).unwrap(),
    );
    meta.calendar_event = Some(notetaker_core::calendar::EventLink {
        uid: "series".into(),
        title: "Weekly".into(),
        attendees: vec![notetaker_core::calendar::Attendee {
            name: "Иван".into(),
            email: "ivan@CLIENT.RU".into(),
        }],
        call_url: None,
    });
    library.inherit_folder(&mut meta, Some("selected")).unwrap();
    assert_eq!(meta.folder.as_deref(), Some("client"));
    assert_eq!(meta.mode, Mode::Hybrid);
    assert_eq!(meta.template.as_deref(), Some("standup"));
    library.create_meeting(meta.clone()).unwrap();
    meta.id = uuid::Uuid::now_v7();
    meta.folder = None;
    meta.started_at += chrono::Duration::days(1);
    meta.calendar_event.as_mut().unwrap().title = "Other".into();
    library.inherit_folder(&mut meta, Some("selected")).unwrap();
    assert_eq!(meta.folder.as_deref(), Some("client"));
    meta.calendar_event = None;
    meta.folder = None;
    library.inherit_folder(&mut meta, Some("selected")).unwrap();
    assert_eq!(meta.folder.as_deref(), Some("selected"));
    meta.folder = Some("client/project".into());
    assert!(matches_folder(&meta, Some("client")));
    assert!(!matches_folder(&meta, Some("cli")));
    assert!(!matches_folder(&meta, Some("")));
    assert!(matches_folder(&meta, None));
}
#[test]
fn shared_paths_revisions_folder_moves_and_biometric_paths() {
    let dir = tempfile::tempdir().unwrap();
    let l = Library::open(dir.path()).unwrap();
    assert_eq!(l.index_path(), l.root().join(".notetaker/index.sqlite"));
    assert!(!l.private_state_dir().starts_with(l.root()));
    l.set_shared(true).unwrap();
    assert_eq!(l.index_path(), l.local_state_dir().join("index.sqlite"));
    assert!(!l.index_path().starts_with(l.root()));
    assert_eq!(
        Library::open(dir.path()).unwrap().local_state_dir(),
        l.local_state_dir()
    );
    assert!(likely_synced(std::path::Path::new(
        r"C:\Users\me\Yandex.Disk\meetings"
    )));
    assert!(likely_synced(std::path::Path::new(
        "/home/me/OneDrive - company/meetings"
    )));
    let loaded = l.load_folders().unwrap();
    l.save_folders(&loaded.value, loaded.revision).unwrap();
    assert!(matches!(
        l.save_folders(&loaded.value, loaded.revision),
        Err(notetaker_core::Error::Conflict { .. })
    ));
    let mut m = MeetingMeta::new("Test", Utc::now());
    m.folder = Some("client/project".into());
    let meeting = l.create_meeting(m).unwrap();
    let path = meeting.path().to_owned();
    l.rename_folder("client", Some("customer")).unwrap();
    assert_eq!(
        meeting.read_meta().unwrap().folder.as_deref(),
        Some("customer/project")
    );
    l.rename_folder("customer", None).unwrap();
    assert!(meeting.read_meta().unwrap().folder.is_none());
    assert_eq!(meeting.path(), path);
}
#[test]
fn sync_copy_detection_and_each_merge_choice() {
    for (choice, expected) in [
        (MergeChoice::Mine, "mine"),
        (MergeChoice::Theirs, "theirs"),
        (
            MergeChoice::Both,
            "mine\n\n## Копия от синхронизации\n\ntheirs",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let l = Library::open(dir.path()).unwrap();
        let m = l
            .create_meeting(MeetingMeta::new("Test", Utc::now()))
            .unwrap();
        m.write_notes("mine").unwrap();
        std::fs::write(m.path().join("notes (1).md"), "theirs").unwrap();
        let conflicts = m.sync_conflicts().unwrap();
        assert_eq!(conflicts.len(), 1);
        m.merge_sync_conflict(&conflicts[0], choice).unwrap();
        assert_eq!(m.read_notes().unwrap(), expected);
        assert!(m.sync_conflicts().unwrap().is_empty());
        std::fs::write(m.path().join("enhanced-PC.md"), "x").unwrap();
        std::fs::write(m.path().join("notes (PC conflicted copy).md"), "x").unwrap();
        assert_eq!(m.sync_conflicts().unwrap().len(), 2);
    }
}
#[test]
fn task_issue_links_are_indexed_without_changing_owner_or_due() {
    let dir = tempfile::tempdir().unwrap();
    let l = Library::open(dir.path()).unwrap();
    let m = l
        .create_meeting(MeetingMeta::new("Task", Utc::now()))
        .unwrap();
    m.write_enhanced("- [ ] Сделать - Иван - завтра\n").unwrap();
    l.reindex_incremental().unwrap();
    let task = l.list_tasks(None, None, false).unwrap().remove(0);
    m.link_task_issue(&task, "https://tracker.test/1").unwrap();
    l.reindex_incremental().unwrap();
    let sent = l.list_tasks(None, None, false).unwrap().remove(0);
    assert_eq!(sent.owner, "Иван");
    assert_eq!(sent.due_text, "завтра");
    assert_eq!(sent.issue_url.as_deref(), Some("https://tracker.test/1"));
    m.toggle_task(&sent, true).unwrap();
    assert!(
        m.read_enhanced()
            .unwrap()
            .contains("[Трекер](https://tracker.test/1)")
    );
}

#[test]
fn folder_filters_cover_tasks_people_series_and_digest() {
    let dir = tempfile::tempdir().unwrap();
    let l = Library::open(dir.path()).unwrap();
    for (folder, name) in [("client", "Анна"), ("other", "Иван")] {
        let mut meta =
            MeetingMeta::new(name, Utc.with_ymd_and_hms(2026, 10, 26, 10, 0, 0).unwrap());
        meta.folder = Some(folder.into());
        meta.mode = Mode::Hybrid;
        meta.calendar_event = Some(notetaker_core::calendar::EventLink {
            uid: "same-series".into(),
            title: "Weekly".into(),
            attendees: vec![notetaker_core::calendar::Attendee {
                name: name.into(),
                email: format!("{folder}@example.org"),
            }],
            call_url: None,
        });
        let m = l.create_meeting(meta).unwrap();
        m.write_enhanced(&format!("- [ ] Проверить - {name} - завтра\n"))
            .unwrap();
    }
    l.reindex_incremental().unwrap();
    let tasks = l
        .list_tasks_in_folder(None, None, false, Some("client"))
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].owner, "Анна");
    let people = l.people_in_folder(false, "", "", Some("client")).unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].name, "Анна");
    let series = l.series_in_folder(false, Some("client")).unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].meetings.len(), 1);
    let page = l
        .person_page_in_folder(people[0].clone(), false, Some("client"))
        .unwrap();
    assert_eq!(page.meetings.len(), 1);
    assert_eq!(page.owned_tasks.len(), 1);
    let digest = l
        .digest(
            notetaker_core::DigestPeriod {
                start: chrono::NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(),
                end: chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            },
            &notetaker_core::DigestFilter {
                folder: Some("client".into()),
                ..Default::default()
            },
            &Utc,
            false,
        )
        .unwrap();
    assert_eq!(digest.meetings.len(), 1);
    assert_eq!(digest.new_tasks.len(), 1);
}
#[test]
fn shared_people_and_vocabulary_preserve_external_versions() {
    let dir = tempfile::tempdir().unwrap();
    let l = Library::open(dir.path()).unwrap();
    let first = l.read_people_edits().unwrap();
    let second = l.read_people_edits().unwrap();
    l.write_people_edits(&first).unwrap();
    assert!(matches!(
        l.write_people_edits(&second),
        Err(notetaker_core::Error::Conflict { .. })
    ));
    let loaded = l.load_shared_text("vocabulary.txt").unwrap();
    l.save_shared_text("vocabulary.txt", "Deckhouse", loaded.revision)
        .unwrap();
    assert!(
        l.save_shared_text("vocabulary.txt", "Kubernetes", loaded.revision)
            .is_err()
    );
    assert_eq!(
        l.load_shared_text("vocabulary.txt").unwrap().value,
        "Deckhouse"
    );
}
#[test]
fn legacy_voice_state_moves_out_before_sharing_and_is_not_imported_from_shared_disk() {
    let dir = tempfile::tempdir().unwrap();
    let l = Library::open(dir.path()).unwrap();
    std::fs::create_dir_all(l.root().join(".notetaker")).unwrap();
    std::fs::write(
        l.root().join(".notetaker/voices.json"),
        serde_json::to_vec(&notetaker_core::VoiceLibrary::default()).unwrap(),
    )
    .unwrap();
    l.set_shared(true).unwrap();
    assert!(!l.root().join(".notetaker/voices.json").exists());
    assert!(l.private_state_dir().join("voices.json").exists());
    std::fs::write(
        l.root().join(".notetaker/voices.json"),
        b"another machine's private data",
    )
    .unwrap();
    assert!(l.read_voices().unwrap().voices.is_empty());
}
#[test]
fn stale_sync_comparison_never_overwrites_an_external_edit() {
    let dir = tempfile::tempdir().unwrap();
    let l = Library::open(dir.path()).unwrap();
    let m = l
        .create_meeting(MeetingMeta::new("Test", Utc::now()))
        .unwrap();
    m.write_notes("mine").unwrap();
    std::fs::write(m.path().join("notes-PC.md"), "theirs").unwrap();
    let conflict = m.sync_conflicts().unwrap().remove(0);
    m.write_notes("new external edit").unwrap();
    assert!(
        m.merge_sync_conflict(&conflict, MergeChoice::Theirs)
            .is_err()
    );
    assert_eq!(m.read_notes().unwrap(), "new external edit");
    assert!(m.path().join("notes-PC.md").exists());
}
