//! Remove source Markdown prefixes without interpreting the resulting text again.
use pulldown_cmark::{Event, Options, Parser, Tag};
use std::{collections::BTreeMap, ops::Range};

pub fn plain_excerpt(source: &str) -> String {
    let mut spans: Vec<Range<usize>> = Vec::new();
    let mut items = BTreeMap::new();
    for (event, range) in Parser::new_ext(source, Options::ENABLE_TASKLISTS).into_offset_iter() {
        let start = range.start
            + source[range.start..]
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
        let tail = &source[start..];
        match event {
            Event::Start(Tag::Item) => {
                let line = source[..start].rfind('\n').map_or(0, |index| index + 1);
                let std::collections::btree_map::Entry::Vacant(item) = items.entry(line) else {
                    continue;
                };
                let bullet = tail.starts_with(['-', '*', '+']);
                let marker = if bullet {
                    1
                } else {
                    tail.bytes().take_while(u8::is_ascii_digit).count() + 1
                };
                let end = start
                    + marker
                    + tail[marker..]
                        .bytes()
                        .take_while(|byte| matches!(byte, b' ' | b'\t'))
                        .count();
                item.insert(end);
                // A number stays: `2026. год запуска` is a list item to the
                // parser and a sentence to the reader.
                if bullet {
                    spans.push(start..end);
                }
            }
            Event::TaskListMarker(_) => {
                let line = source[..start].rfind('\n').map_or(0, |index| index + 1);
                if items
                    .get(&line)
                    .is_some_and(|&end| source[end..start].trim().is_empty())
                {
                    // The gap after the box goes with it, or a numbered item
                    // keeps two spaces.
                    let gap = source[range.end..]
                        .bytes()
                        .take_while(|byte| matches!(byte, b' ' | b'\t'))
                        .count();
                    spans.push(range.start..range.end + gap);
                }
            }
            Event::Start(Tag::Heading { .. }) => {
                let hashes = tail.bytes().take_while(|byte| *byte == b'#').count();
                if (1..=6).contains(&hashes) && tail.as_bytes().get(hashes) == Some(&b' ') {
                    spans.push(start..start + hashes + 1);
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                let mut offset = range.start;
                for line in source[range].split_inclusive('\n') {
                    let leading = line.len() - line.trim_start_matches([' ', '\t']).len();
                    if line.as_bytes().get(leading) == Some(&b'>') {
                        spans.push(offset + leading..offset + leading + 1);
                    }
                    offset += line.len();
                }
            }
            _ => {}
        }
    }
    spans.sort_by_key(|span| span.start);
    let mut spans = spans.iter().peekable();
    let mut text = String::new();
    for (index, ch) in source.char_indices() {
        while spans.peek().is_some_and(|span| span.end <= index) {
            spans.next();
        }
        if !spans.peek().is_some_and(|span| span.contains(&index)) {
            text.push(ch);
        }
    }
    text.lines().map(str::trim).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meeting_excerpt_is_clean_before_lines_are_joined() {
        let meeting = crate::MeetingSummary {
            meta: crate::MeetingMeta::new("Встреча", chrono::Utc::now()),
            path: Default::default(),
            summary: "## Итоги\n- Решение\n## Дальше".into(),
            open_tasks: 0,
            series_id: None,
            participants: Default::default(),
            transcript: Default::default(),
        };
        assert_eq!(meeting.excerpt(), "Итоги Решение Дальше");
    }
    #[test]
    fn excerpt_uses_original_markdown_context_once() {
        for (source, expected) in [
            (
                "## Итоги\n- Решение\n  1. Дальше\n> - [x] Сделано",
                "Итоги\nРешение\n1. Дальше\nСделано",
            ),
            ("1. 2026. год запуска", "1. 2026. год запуска"),
            ("2026. год запуска", "2026. год запуска"),
            ("Итог\n\n2026. год запуска\n", "Итог\n\n2026. год запуска"),
            ("1. [ ] Задача", "1. Задача"),
            ("План\n2026. год запуска", "План\n2026. год запуска"),
            (
                "#123\n#тег\n-5 градусов\nC#\n1.5 версия",
                "#123\n#тег\n-5 градусов\nC#\n1.5 версия",
            ),
            ("1. Первый\n2. Второй", "1. Первый\n2. Второй"),
            ("* Первый\n+ Второй\n- [ ] Третий", "Первый\nВторой\nТретий"),
            ("[x] Обычный текст", "[x] Обычный текст"),
            ("####### Не заголовок", "####### Не заголовок"),
            (
                "```md\n- [ ] Код\n## Код\n```",
                "```md\n- [ ] Код\n## Код\n```",
            ),
        ] {
            assert_eq!(plain_excerpt(source), expected, "source={source:?}");
        }
    }
}
