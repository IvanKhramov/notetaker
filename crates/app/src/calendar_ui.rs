use crate::theme::{self, ButtonStyle};
use chrono::{DateTime, Utc};
use eframe::egui;
use notetaker_core::calendar::CalendarEvent;

pub fn call_link(event: &CalendarEvent) -> Option<String> {
    let url = event.link.call_url.as_deref()?;
    notetaker_core::calendar::is_allowed_call_link(url).then(|| url.to_owned())
}
pub fn action_label(event: &CalendarEvent) -> &'static str {
    if call_link(event).is_some() {
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
    let frame = egui::Frame::popup(ui.style());
    let p = theme::Palette::of(ui);
    frame.show(ui, |ui| {
        ui.set_width(
            ui.available_width()
                .min(340.0 - frame.total_margin().sum().x),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(&event.link.title).font(theme::medium(theme::BODY)),
            )
            .truncate(),
        )
        .on_hover_text(&event.link.title);
        let minutes = (event.start - Utc::now()).num_seconds();
        let time = if minutes > 0 {
            format!("через {} мин", (minutes + 59) / 60)
        } else {
            format!("идёт {} мин", -minutes / 60)
        };
        ui.label(
            egui::RichText::new(format!(
                "{time} · {}",
                crate::wording::count(
                    event.link.attendees.len() as u64,
                    crate::wording::PARTICIPANTS
                )
            ))
            .font(theme::medium(theme::BUTTON))
            .color(p.muted),
        );
        theme::footer(ui, |ui| {
            // The footer lays out from right to left, so Later follows the primary action visually.
            if theme::button(ui, "Позже", ButtonStyle::Ghost).clicked() {
                action = Action::Dismiss;
            }
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
        });
    });
    action
}

// Vendor instructions checked on 2026-11-15. These are help pages, never subscription URLs.
pub fn setup_help(ui: &mut egui::Ui) {
    theme::disclosure("Где взять ссылку").show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (index, (name, steps, source)) in [
            ("Яндекс Календарь", "1. В календаре слева откройте настройки нужного календаря.\n2. Откройте «Экспорт».\n3. Скопируйте ссылку iCal.", "https://yandex.ru/support/yandex-360/customers/calendar/web/ru/data-exchange/export"),
            ("Google Calendar", "1. На компьютере откройте настройки и выберите свой календарь.\n2. Откройте «Интеграция календаря».\n3. Скопируйте «Закрытый адрес в формате iCal». Если его нет, обратитесь к администратору.", "https://support.google.com/calendar/answer/37648?hl=ru"),
            ("Outlook", "1. В веб-календаре откройте «Параметры» и «Общие календари».\n2. В «Публикации календаря» выберите календарь и доступные сведения, нажмите «Опубликовать».\n3. Скопируйте ссылку ICS.", "https://support.microsoft.com/ru-ru/outlook/share-your-calendar-in-outlook-on-the-web"),
        ].into_iter().enumerate() {
            if index > 0 { ui.add_space(theme::SPACE[2]); }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = theme::SPACE[0];
                let p = theme::Palette::of(ui);
                ui.label(egui::RichText::new(name).font(theme::medium(theme::SMALL)));
                ui.indent(("calendar-help-steps", name), |ui| {
                    for step in steps.lines() {
                        let (number, text) = step.split_once(". ").expect("static numbered instructions");
                        ui.horizontal_top(|ui| {
                            ui.label(egui::RichText::new(format!("{number}.")).font(theme::medium(theme::BUTTON)).color(p.muted));
                            ui.add(egui::Label::new(egui::RichText::new(text).font(theme::medium(theme::BUTTON)).color(p.muted)).wrap());
                        });
                    }
                });
                ui.add(egui::Hyperlink::from_label_and_url(egui::RichText::new("Справка").font(theme::medium(theme::BUTTON)).color(p.accent), source));
            });
        }
        ui.add_space(theme::SPACE[2]);
        theme::callout(ui, theme::icons::INFO, |ui| {
            ui.label(egui::RichText::new("Ссылка даёт доступ к календарю. Не публикуйте её. Outlook публикует календарь для владельцев ссылки; это может быть запрещено организацией.").font(theme::medium(theme::BUTTON)).color(theme::Palette::of(ui).muted));
        });
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
