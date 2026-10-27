use notetaker_core::{Bookmark, ExportFormat, Library, MeetingMeta, Transcript};
use uuid::Uuid;

#[test]
fn bookmarks_are_optional_and_survive_atomic_updates_and_final_replacement() {
    let old: Transcript = serde_json::from_str(r#"{"version":1,"segments":[]}"#).unwrap();
    assert!(old.bookmarks.is_empty());
    assert!(!serde_json::to_string(&old).unwrap().contains("bookmarks"));
    let dir = tempfile::tempdir().unwrap();
    let meeting = Library::open(dir.path())
        .unwrap()
        .create_meeting(MeetingMeta::new("test", chrono::Utc::now()))
        .unwrap();
    let bookmark = Bookmark {
        id: Uuid::now_v7(),
        at_ms: 42_000,
        label: Some("Решение".into()),
    };
    meeting
        .update_transcript(|t| t.bookmarks.push(bookmark.clone()))
        .unwrap();
    // The same update primitive used by the final ASR pass changes segments only.
    meeting.update_transcript(|t| t.segments.clear()).unwrap();
    assert_eq!(
        meeting.read_transcript().unwrap().bookmarks,
        vec![bookmark.clone()]
    );
    meeting
        .update_transcript(|t| t.bookmarks[0].label = Some("Новое имя".into()))
        .unwrap();
    assert_eq!(
        meeting.read_transcript().unwrap().bookmarks[0]
            .label
            .as_deref(),
        Some("Новое имя")
    );
    meeting
        .update_transcript(|t| t.bookmarks.retain(|b| b.id != bookmark.id))
        .unwrap();
    assert!(meeting.read_transcript().unwrap().bookmarks.is_empty());
}

#[test]
fn bookmark_exports_have_json_array_markdown_list_and_safe_vtt_csv() {
    let text = Transcript {
        bookmarks: vec![Bookmark {
            id: Uuid::now_v7(),
            at_ms: 42_000,
            label: Some("<b>Решение</b>\nNOTE\n\n\"quote\"".into()),
        }],
        ..Default::default()
    };
    let md = text.export(ExportFormat::Md).unwrap();
    assert!(md.contains("## Важные моменты\n\n- [00:00:42.000] &lt;b&gt;"));
    let json: serde_json::Value =
        serde_json::from_str(&text.export(ExportFormat::Json).unwrap()).unwrap();
    assert_eq!(json["bookmarks"][0]["at_ms"], 42_000);
    assert_eq!(json["bookmarks"][0]["id"], text.bookmarks[0].id.to_string());
    let vtt = text.export(ExportFormat::Vtt).unwrap();
    assert!(vtt.contains("NOTE Закладка 00:00:42.000 &lt;b&gt;Решение&lt;/b&gt; NOTE"));
    let csv = text.export(ExportFormat::Csv).unwrap();
    assert!(csv.contains("\"Закладка\""));
    assert!(csv.contains("\"\"quote\"\""));
    // SRT has no metadata/comment mechanism; leave subtitle cue semantics intact.
    assert_eq!(text.export(ExportFormat::Srt).unwrap(), "");
}
