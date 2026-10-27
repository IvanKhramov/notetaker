//! Action items retain their source line; Markdown remains the source of truth.
use std::path::PathBuf;

use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::{Meeting, Result, Revision};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub meeting_id: uuid::Uuid,
    pub path: PathBuf,
    pub meeting_title: String,
    /// One-based line number in enhanced.md.
    pub line: usize,
    pub text: String,
    pub owner: String,
    pub due_text: String,
    pub due: Option<NaiveDate>,
    pub completed: bool,
    pub source: String,
    #[serde(skip)]
    pub revision: Revision,
}

impl Task {
    pub fn meeting(&self, library: &crate::Library) -> Result<Meeting> {
        let path = self.path.canonicalize()?;
        if path.parent() != Some(library.root()) {
            return Err(crate::Error::TaskEdit("Встреча вне библиотеки".into()));
        }
        let meeting = Meeting { path };
        meeting.validate_source_paths()?;
        if meeting.read_meta()?.id != self.meeting_id {
            return Err(crate::Error::TaskEdit("Встреча изменилась".into()));
        }
        Ok(meeting)
    }
}

pub fn person_matches(owner: &str, name: &str) -> bool {
    !name.trim().is_empty() && normalize_person(owner) == normalize_person(name)
}
fn normalize_person(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('@')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .replace('ё', "е")
}

/// Fenced and indented code are ignored; nested list items are accepted.
pub fn parse_tasks(text: &str, date: NaiveDate) -> Vec<Task> {
    let mut tasks = Vec::new();
    let parser = pulldown_cmark::Parser::new_ext(text, pulldown_cmark::Options::ENABLE_TASKLISTS);
    for (event, range) in parser.into_offset_iter() {
        let pulldown_cmark::Event::TaskListMarker(completed) = event else {
            continue;
        };
        let line_start = text[..range.start].rfind('\n').map_or(0, |index| index + 1);
        let line_end = text[range.end..]
            .find('\n')
            .map_or(text.len(), |index| range.end + index);
        let source = text[line_start..line_end].trim_end_matches('\r');
        let index = text[..line_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let body = text[range.end..line_end].trim();
        if body.is_empty() {
            continue;
        }
        // Accept a typographic dash in input without emitting it in our assets.
        let normalized = body.replace(['\u{2013}', '\u{2014}'], "-");
        let mut fields: Vec<_> = normalized.split(" - ").map(str::trim).collect();
        if fields.len() == 1 {
            fields = normalized.split(" | ").map(str::trim).collect();
        }
        if fields.len() == 1 {
            fields = normalized.split("; ").map(str::trim).collect();
        }
        let (title, owner, due_text) = if fields.len() >= 3 {
            let due = fields.pop().unwrap();
            let owner = fields.pop().unwrap();
            (
                fields.join(" - "),
                clean_field(owner, &["ответственный:", "владелец:", "owner:"]),
                clean_field(due, &["срок:", "deadline:", "due:"]),
            )
        } else if fields.len() == 2 {
            let last = fields[1];
            if resolve_due(last, date).is_some() || last.to_lowercase().starts_with("до ") {
                (
                    fields[0].into(),
                    String::new(),
                    clean_field(last, &["срок:", "due:"]),
                )
            } else {
                (
                    fields[0].into(),
                    clean_field(last, &["ответственный:", "owner:"]),
                    String::new(),
                )
            }
        } else {
            (body.into(), String::new(), String::new())
        };
        tasks.push(Task {
            meeting_id: uuid::Uuid::nil(),
            path: PathBuf::new(),
            meeting_title: String::new(),
            line: index + 1,
            text: title,
            owner,
            due: resolve_due(&due_text, date),
            due_text,
            completed,
            source: source.into(),
            revision: Revision::default(),
        });
    }
    tasks
}
fn clean_field(value: &str, prefixes: &[&str]) -> String {
    let unmarked = value.replace(['*', '`'], "");
    let value = unmarked.trim().trim_matches('_').trim();
    let value = value
        .split_once(':')
        .and_then(|(label, rest)| {
            let label = label.trim().trim_matches('_').trim().to_lowercase();
            prefixes
                .iter()
                .any(|prefix| prefix.trim_end_matches(':') == label)
                .then(|| rest.trim().trim_matches('_').trim())
        })
        .unwrap_or(value);
    if [
        "не указан",
        "не указана",
        "не указано",
        "нет",
        "n/a",
        "?",
        "-",
    ]
    .contains(&value.to_lowercase().as_str())
    {
        String::new()
    } else {
        value.into()
    }
}
pub fn resolve_due(value: &str, date: NaiveDate) -> Option<NaiveDate> {
    let lower = value.trim().to_lowercase();
    let value = lower.strip_prefix("до ").unwrap_or(&lower).trim();
    match value {
        "сегодня" => return Some(date),
        "завтра" => return date.checked_add_signed(Duration::days(1)),
        "послезавтра" => return date.checked_add_signed(Duration::days(2)),
        _ => {}
    }
    for format in ["%Y-%m-%d", "%d.%m.%Y", "%d/%m/%Y"] {
        if let Ok(parsed) = NaiveDate::parse_from_str(value, format) {
            return Some(parsed);
        }
    }
    if value.split('.').count() == 2 {
        let (day, month) = value.split_once('.')?;
        let parsed = NaiveDate::from_ymd_opt(date.year(), month.parse().ok()?, day.parse().ok()?)?;
        // A yearless date denotes the next occurrence from the meeting date.
        return if parsed < date {
            NaiveDate::from_ymd_opt(date.year() + 1, parsed.month(), parsed.day())
        } else {
            Some(parsed)
        };
    }
    let weekday = match value.trim_end_matches('.') {
        "пн" | "понедельника" | "понедельник" => 0,
        "вт" | "вторника" | "вторник" => 1,
        "ср" | "среды" | "среду" | "среда" => 2,
        "чт" | "четверга" | "четверг" => 3,
        "пт" | "пятницы" | "пятницу" | "пятница" => 4,
        "сб" | "субботы" | "субботу" | "суббота" => 5,
        "вс" | "воскресенья" | "воскресенье" => 6,
        _ => return None,
    };
    let days = (weekday - i64::from(date.weekday().num_days_from_monday())).rem_euclid(7);
    date.checked_add_signed(Duration::days(days))
}

impl Meeting {
    pub fn toggle_task(&self, task: &Task, completed: bool) -> Result<Revision> {
        if task.line == 0 || task.path != self.path {
            return Err(crate::Error::TaskEdit(
                "Задача относится к другой встрече".into(),
            ));
        }
        let loaded = self.load_enhanced()?;
        let mut lines: Vec<_> = loaded
            .value
            .split_inclusive('\n')
            .map(str::to_owned)
            .collect();
        let line = lines
            .get_mut(task.line.saturating_sub(1))
            .ok_or_else(|| crate::Error::TaskEdit("Строка задачи больше не существует".into()))?;
        if line.trim_end_matches(['\r', '\n']) != task.source {
            return Err(crate::Error::TaskEdit(
                "Задача изменилась. Обновите список".into(),
            ));
        }
        let offset = line
            .find("[ ]")
            .or_else(|| line.find("[x]"))
            .or_else(|| line.find("[X]"))
            .ok_or_else(|| crate::Error::TaskEdit("Нет флажка задачи".into()))?;
        line.replace_range(offset + 1..offset + 2, if completed { "x" } else { " " });
        self.save_enhanced(&lines.concat(), task.revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marked_labels_missing_fields_and_unicode_separators() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let text = "- [ ] Сделать \u{2014} **Ответственный:** Иван \u{2013} **Срок:** _завтра_\n- [ ] Проверить; owner: @Анна; due: до 30.10\n- [ ] Дело - не указан - не указан\n- [ ] Другое - завтра";
        let tasks = parse_tasks(text, date);
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0].owner, "Иван");
        assert_eq!(tasks[0].due, date.succ_opt());
        assert!(person_matches(&tasks[1].owner, "анна"));
        assert!(tasks[2].owner.is_empty());
        assert!(tasks[2].due.is_none());
        assert!(tasks[3].owner.is_empty());
        assert_eq!(tasks[3].due, date.succ_opt());
    }
    #[test]
    fn formats_dates_nesting_and_code() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let items = parse_tasks(
            "- [ ] Релиз - Иван - до пт\n  * [X] Проверить | Ответственный: Анна | Срок: завтра\n1. [ ] Тест - Игорь - до 30.10\n- [ ] Без срока\n```md\n- [ ] код\n```\n\n    - [ ] тоже код\n~~~\n- [ ] код\n~~~\n- [ ] Год - Я - 2027-01-01\n",
            date,
        );
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].due.unwrap().to_string(), "2026-10-30");
        assert_eq!(items[1].owner, "Анна");
        assert!(items[1].completed);
        assert_eq!(items[1].due.unwrap().to_string(), "2026-10-28");
        assert_eq!(items[2].due, items[0].due);
        assert!(items[3].due.is_none());
        assert_eq!(
            resolve_due("до 02.01", NaiveDate::from_ymd_opt(2026, 12, 31).unwrap())
                .unwrap()
                .year(),
            2027
        );
        assert!(resolve_due("на следующей неделе", date).is_none());
        assert!(person_matches("@Иван Ёлкин", "иван елкин"));
    }
    #[test]
    fn toggle_preserves_lines_and_detects_stale_revision() {
        let dir = tempfile::tempdir().unwrap();
        let lib = crate::Library::open(dir.path()).unwrap();
        let meeting = lib
            .create_meeting(crate::MeetingMeta::new("Test", chrono::Utc::now()))
            .unwrap();
        meeting
            .write_enhanced("# Итоги\r\n- [ ] Дело - Иван - завтра\r\n")
            .unwrap();
        lib.reindex_full().unwrap();
        let task = lib.list_tasks(None, None, false).unwrap().remove(0);
        meeting
            .write_enhanced("# Итоги\r\n- [ ] Дело - Иван - завтра\r\nВнешняя правка\r\n")
            .unwrap();
        assert!(matches!(
            meeting.toggle_task(&task, true),
            Err(crate::Error::Conflict { .. })
        ));
        assert!(meeting.read_enhanced().unwrap().contains("[ ] Дело"));
        lib.reindex_incremental().unwrap();
        let task = lib.list_tasks(None, None, false).unwrap().remove(0);
        meeting.toggle_task(&task, true).unwrap();
        assert!(meeting.read_enhanced().unwrap().contains("[x] Дело"));
        assert!(
            meeting
                .read_enhanced()
                .unwrap()
                .contains("Внешняя правка\r\n")
        );
        assert!(meeting.read_enhanced().unwrap().starts_with("# Итоги\r\n"));
    }
}
