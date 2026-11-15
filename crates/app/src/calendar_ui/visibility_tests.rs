use super::*;
use chrono::TimeZone;
fn now() -> DateTime<Utc> {
    chrono::Local
        .with_ymd_and_hms(2026, 11, 15, 12, 0, 0)
        .earliest()
        .unwrap()
        .with_timezone(&Utc)
}
fn event(title: &str, start: DateTime<Utc>) -> CalendarEvent {
    CalendarEvent {
        link: notetaker_core::calendar::EventLink {
            uid: title.into(),
            title: title.into(),
            attendees: vec![],
            call_url: Some("https://zoom.us/neutral".into()),
        },
        start,
        end: start + chrono::Duration::minutes(30),
        all_day: false,
        organizer: None,
    }
}
fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    fn visit(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
        match shape {
            egui::epaint::Shape::Text(t) => out.push(t.galley.text().into()),
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    visit(s, out)
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes {
        visit(&s.shape, &mut out);
    }
    out
}
fn model(events: &[CalendarEvent]) -> Home<'_> {
    Home {
        events,
        reports: &[],
        error: None,
        loading: false,
        now: now(),
        can_record: true,
    }
}
#[test]
fn home_loading_empty_today_tomorrow_in_progress_and_record_action() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut expanded = false;
    let mut out = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
        home(
            ui,
            Home {
                loading: true,
                ..model(&[])
            },
            &mut expanded,
        );
    });
    out.textures_delta.clear();
    assert!(texts(&out.shapes).contains(&"Загружаем календарь…".into()));
    let mut out = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
        home(ui, model(&[]), &mut expanded);
    });
    out.textures_delta.clear();
    assert!(texts(&out.shapes).contains(&"Сегодня и завтра встреч нет".into()));
    let events = vec![
        event("Прошедшая", now() - chrono::Duration::hours(2)),
        event("Текущая", now() - chrono::Duration::minutes(5)),
        event("Позже", now() + chrono::Duration::hours(2)),
        event("Завтрашняя", now() + chrono::Duration::days(1)),
    ];
    let mut out = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
        home(ui, model(&events), &mut expanded);
    });
    out.textures_delta.clear();
    let text = texts(&out.shapes);
    for value in [
        "Календарь",
        "Сегодня",
        "Завтра",
        "Текущая",
        "идёт",
        "Войти и записать",
        "Завтрашняя",
    ] {
        assert!(text.contains(&value.into()), "{value}: {text:?}");
    }
    assert!(!text.contains(&"Прошедшая".into()));
    assert_eq!(
        text.iter()
            .filter(|t| t.as_str() == "Войти и записать")
            .count(),
        1
    );
}
#[test]
fn home_error_settings_link_and_more_expand() {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut expanded = false;
    let mut settings = false;
    crate::test_ui::click(
        &ctx,
        &mut |ui| {
            if matches!(
                home(
                    ui,
                    Home {
                        error: Some("Не удалось загрузить календарь"),
                        ..model(&[])
                    },
                    &mut expanded
                ),
                HomeAction::Settings
            ) {
                settings = true;
            }
        },
        "Открыть настройки календаря",
    );
    assert!(settings);
    let events: Vec<_> = (0..8)
        .map(|i| {
            event(
                &format!("Событие {i}"),
                now() + chrono::Duration::minutes(10 + i * 20),
            )
        })
        .collect();
    let mut out = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
        home(ui, model(&events), &mut expanded);
    });
    out.textures_delta.clear();
    assert!(texts(&out.shapes).contains(&"ещё 2".into()));
    assert!(!texts(&out.shapes).contains(&"Событие 7".into()));
    crate::test_ui::click(
        &ctx,
        &mut |ui| {
            home(ui, model(&events), &mut expanded);
        },
        "ещё 2",
    );
    assert!(expanded);
    let mut out = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
        home(ui, model(&events), &mut expanded);
    });
    out.textures_delta.clear();
    assert!(texts(&out.shapes).contains(&"Событие 7".into()));
}
#[test]
fn settings_success_skips_titles_and_failure_keep_refresh_available() {
    use notetaker_core::calendar::{SkipReason, SkippedEvent};
    let report = notetaker_llm::calendar::SubscriptionStatus {
        id: "calendar-test".into(),
        events: vec![event("Обзор", now())],
        updated_at: Some(now()),
        skipped: vec![
            SkippedEvent {
                title: "Частый повтор".into(),
                reason: SkipReason::Recurrence,
            },
            SkippedEvent {
                title: "Другой повтор".into(),
                reason: SkipReason::Recurrence,
            },
            SkippedEvent {
                title: "Другая зона".into(),
                reason: SkipReason::Timezone,
            },
        ],
        error: None,
    };
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let mut out = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
        subscription_status(ui, Some(&report), None, false)
    });
    out.textures_delta.clear();
    let text = texts(&out.shapes);
    assert!(text.iter().any(|s| s.starts_with("Обновлено в 12:00")));
    assert!(text.contains(
        &"Пропущено событий: 3 (неподдерживаемое повторение: 2, часовой пояс: 1)".into()
    ));
    crate::test_ui::click(
        &ctx,
        &mut |ui| subscription_status(ui, Some(&report), None, false),
        "Пропущенные события",
    );
    // Open/close animation needs a later frame before checking the content.
    let mut out = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
        subscription_status(ui, Some(&report), None, false)
    });
    out.textures_delta.clear();
    assert!(texts(&out.shapes).contains(&"Частый повтор".into()));
    assert!(refresh_enabled(true, false));
    assert!(!refresh_enabled(true, true));
    assert!(!refresh_enabled(false, false));
    let (mut state, commands, _) = crate::test_ui::state();
    state
        .settings
        .calendars
        .push(notetaker_llm::calendar::Subscription {
            id: "calendar-test".into(),
            name: "Команда".into(),
        });
    state.settings_draft = state.settings.clone();
    state.calendar_error = Some("Не удалось загрузить календарь".into());
    let mut panel = crate::settings::Panel::default();
    panel.section = crate::settings::Section::Calendars;
    let mut maintenance = crate::maintenance::Maintenance::default();
    let mut organization = crate::organization::Panel::default();
    crate::test_ui::click(
        &ctx,
        &mut |ui| {
            panel.body(ui, &mut state, &mut maintenance, &mut organization);
        },
        "Обновить календари",
    );
    assert!(state.calendar_busy);
    assert!(
        commands
            .try_iter()
            .any(|c| matches!(c, crate::worker::Command::Calendar { .. }))
    );
}
