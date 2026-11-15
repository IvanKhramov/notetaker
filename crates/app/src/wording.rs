//! Shared Russian counters and plain-text excerpts for UI previews.
pub const MEETINGS: [&str; 3] = ["встреча", "встречи", "встреч"];
pub const TASKS: [&str; 3] = ["задача", "задачи", "задач"];
pub const PARTICIPANTS: [&str; 3] = ["участник", "участника", "участников"];
pub const SPEAKERS: [&str; 3] = ["собеседник", "собеседника", "собеседников"];
pub const UTTERANCES: [&str; 3] = ["реплика", "реплики", "реплик"];
pub const TURNS: [&str; 3] = ["ход", "хода", "ходов"];
pub const MINUTES: [&str; 3] = ["минута", "минуты", "минут"];
pub const DAYS: [&str; 3] = ["день", "дня", "дней"];

pub fn plural(n: u64, forms: [&str; 3]) -> &str {
    if (11..=14).contains(&(n % 100)) {
        forms[2]
    } else {
        match n % 10 {
            1 => forms[0],
            2..=4 => forms[1],
            _ => forms[2],
        }
    }
}
pub fn count(n: u64, forms: [&str; 3]) -> String {
    format!("{n} {}", plural(n, forms))
}

pub use notetaker_core::plain_excerpt as excerpt;

pub fn audio_status(available: bool, expires: Option<chrono::DateTime<chrono::Utc>>) -> String {
    if !available {
        "Аудио удалено".into()
    } else if let Some(expires) = expires {
        format!(
            "Аудио хранится до {}",
            expires.with_timezone(&chrono::Local).format("%d.%m")
        )
    } else {
        "Аудио хранится бессрочно".into()
    }
}

pub fn short_excerpt(markdown: &str) -> String {
    excerpt(markdown)
        .lines()
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn russian_counter_table() {
        for (n, index) in [
            (0, 2),
            (1, 0),
            (2, 1),
            (4, 1),
            (5, 2),
            (11, 2),
            (12, 2),
            (14, 2),
            (21, 0),
            (22, 1),
            (25, 2),
            (101, 0),
            (111, 2),
        ] {
            for forms in [
                MEETINGS,
                TASKS,
                PARTICIPANTS,
                SPEAKERS,
                UTTERANCES,
                TURNS,
                MINUTES,
                DAYS,
            ] {
                assert_eq!(plural(n, forms), forms[index]);
                assert_eq!(count(n, forms), format!("{n} {}", forms[index]));
            }
        }
    }
    #[test]
    fn excerpts_remove_only_markdown_prefixes() {
        assert_eq!(
            excerpt("-\t[\t]\tПроверить\n1)\tДальше"),
            "Проверить\nДальше"
        );
        assert_eq!(
            short_excerpt("## Решения\n- Первый пункт\n## Дальше\n- Второй пункт"),
            "Решения Первый пункт Дальше"
        );
        assert_eq!(
            excerpt("## Итоги\n- Решение\n  1. Дальше\n> - [x] Сделано\r\n+ Ещё"),
            "Итоги\nРешение\nДальше\nСделано\nЕщё"
        );
        assert_eq!(
            excerpt("C#\n#тег\n1.5 версия\n-22 градуса\nhttps://example.invalid"),
            "C#\n#тег\n1.5 версия\n-22 градуса\nhttps://example.invalid"
        );
    }
    #[test]
    fn audio_availability_kept_expiring_deleted() {
        let expiry = "2026-11-22T12:00:00Z".parse().unwrap();
        assert_eq!(audio_status(true, None), "Аудио хранится бессрочно");
        assert_eq!(audio_status(true, Some(expiry)), "Аудио хранится до 22.11");
        assert_eq!(audio_status(false, Some(expiry)), "Аудио удалено");
        assert_eq!(audio_status(false, None), "Аудио удалено");
    }
}
