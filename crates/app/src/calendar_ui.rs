use crate::theme::{self, ButtonStyle};
use chrono::{DateTime, Utc};
use eframe::egui;
use notetaker_core::calendar::CalendarEvent;

pub fn call_link(event: &CalendarEvent) -> Option<String> {
    let url = event.link.call_url.as_deref()?;
    notetaker_core::calendar::call_url(url).filter(|allowed| allowed == url)
}
pub fn action_label(event: &CalendarEvent) -> &'static str {
    if event.link.call_url.is_some() {
        "Войти и записать"
    } else {
        "Начать запись"
    }
}
pub fn eligible(event: &CalendarEvent, now: DateTime<Utc>) -> bool {
    !event.all_day
        && event.end > now
        && event.start <= now + chrono::Duration::minutes(5)
        && (event.link.attendees.len() >= 2 || call_link(event).is_some())
}
#[derive(Default, PartialEq, Debug)]
pub enum Action {
    #[default]
    None,
    Start,
    Dismiss,
}
pub fn reminder(ui: &mut egui::Ui, event: &CalendarEvent, enabled: bool) -> Action {
    let mut action = Action::None;
    theme::bar(theme::Palette::of(ui)).show(ui, |ui| {
        ui.add(egui::Label::new(egui::RichText::new(&event.link.title).strong()).truncate());
        let minutes = (event.start - Utc::now()).num_seconds();
        let time = if minutes > 0 {
            format!("через {} мин", (minutes + 59) / 60)
        } else {
            format!("идёт {} мин", -minutes / 60)
        };
        ui.weak(format!(
            "{time} · Участников: {}",
            event.link.attendees.len()
        ));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    enabled,
                    theme::styled_button(
                        theme::Palette::of(ui),
                        action_label(event),
                        ButtonStyle::Primary,
                    ),
                )
                .clicked()
            {
                action = Action::Start;
            }
            if theme::button(ui, "Позже", ButtonStyle::Secondary).clicked() {
                action = Action::Dismiss;
            }
        });
    });
    action
}

// Vendor instructions checked on 2026-11-15. These are help pages, never subscription URLs.
pub fn setup_help(ui: &mut egui::Ui) {
    theme::disclosure("Где взять ссылку").show(ui, |ui| {
        for (name, steps, source) in [
            ("Яндекс Календарь", "1. В календаре слева откройте настройки нужного календаря.\n2. Откройте «Экспорт».\n3. Скопируйте ссылку iCal.", "https://yandex.ru/support/yandex-360/customers/calendar/web/ru/data-exchange/export"),
            ("Google Calendar", "1. На компьютере откройте настройки и выберите свой календарь.\n2. Откройте «Интеграция календаря».\n3. Скопируйте «Закрытый адрес в формате iCal». Если его нет, обратитесь к администратору.", "https://support.google.com/calendar/answer/37648?hl=ru"),
            ("Outlook", "1. В веб-календаре откройте «Параметры» и «Общие календари».\n2. В «Публикации календаря» выберите календарь и доступные сведения, нажмите «Опубликовать».\n3. Скопируйте ссылку ICS.", "https://support.microsoft.com/ru-ru/outlook/share-your-calendar-in-outlook-on-the-web"),
        ] {
            ui.strong(name); ui.small(steps); ui.hyperlink_to("Справка",source);
        }
        ui.weak("Ссылка даёт доступ к календарю. Не публикуйте её. Outlook публикует календарь для владельцев ссылки; это может быть запрещено организацией.");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reminder_keeps_focus_and_no_link_has_record_action() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let now = Utc::now();
        let event = CalendarEvent {
            link: notetaker_core::calendar::EventLink {
                uid: "event".into(),
                title: "План".into(),
                attendees: vec![],
                call_url: None,
            },
            start: now - chrono::Duration::minutes(2),
            end: now + chrono::Duration::minutes(10),
            all_day: false,
            organizer: None,
        };
        let field = egui::Id::unique("focus-owner");
        let mut value = String::new();
        let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            ui.add(theme::text_field(&mut value).id(field))
                .request_focus();
            reminder(ui, &event, true);
        });
        crate::test_ui::text_rect(&output.shapes, "Начать запись");
        assert!(ctx.memory(|m| m.has_focus(field)));
        output.drop_without_applying_deltas();
        let mut chosen = Action::None;
        crate::test_ui::click(
            &ctx,
            &mut |ui| {
                let action = reminder(ui, &event, true);
                if action != Action::None {
                    chosen = action;
                }
            },
            "Начать запись",
        );
        assert_eq!(chosen, Action::Start);
    }
}
