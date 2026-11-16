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
pub fn reminder(ui: &mut egui::Ui, event: &CalendarEvent, reason: Option<&str>) -> Action {
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
                    reason.is_none(),
                    theme::styled_button(
                        theme::Palette::of(ui),
                        action_label(event),
                        ButtonStyle::Primary,
                    ),
                )
                .on_disabled_hover_text(reason.unwrap_or_default())
                .clicked()
            {
                action = Action::Start;
            }
        });
        if let Some(reason) = reason {
            ui.weak(reason);
        }
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
            reminder(ui, &event, None);
        });
        crate::test_ui::text_rect(&output.shapes, "Начать запись");
        assert!(ctx.memory(|m| m.has_focus(field)));
        output.drop_without_applying_deltas();
        let mut chosen = Action::None;
        crate::test_ui::click(
            &ctx,
            &mut |ui| {
                let action = reminder(ui, &event, None);
                if action != Action::None {
                    chosen = action;
                }
            },
            "Начать запись",
        );
        assert_eq!(chosen, Action::Start);
    }
}

pub fn refresh_enabled(configured: bool, busy: bool) -> bool {
    configured && !busy
}
fn status_line(ui: &mut egui::Ui, icon: &str, text: &str, color: egui::Color32) {
    ui.horizontal_wrapped(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 20.0), egui::Sense::hover());
        theme::paint_icon(ui, rect.center(), icon, 16.0, color);
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .font(theme::medium(theme::SMALL))
                    .color(color),
            )
            .wrap(),
        );
    });
}
pub fn skipped_text(skipped: &[notetaker_core::calendar::SkippedEvent]) -> String {
    let mut counts = std::collections::BTreeMap::new();
    for event in skipped {
        *counts.entry(event.reason).or_insert(0) += 1;
    }
    format!(
        "Пропущено событий: {} ({})",
        skipped.len(),
        counts
            .into_iter()
            .map(|(reason, count)| format!("{}: {count}", reason.label()))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
fn snapshot_label(at: DateTime<Utc>, now: DateTime<Utc>, failed: bool) -> String {
    let local = at.with_timezone(&chrono::Local);
    let when = if local.date_naive() == now.with_timezone(&chrono::Local).date_naive() {
        format!("в {}", local.format("%H:%M"))
    } else {
        format!("{} в {}", local.format("%d.%m"), local.format("%H:%M"))
    };
    if failed {
        format!(
            "Данные от {}",
            if local.date_naive() == now.with_timezone(&chrono::Local).date_naive() {
                local.format("%H:%M").to_string()
            } else {
                format!("{} в {}", local.format("%d.%m"), local.format("%H:%M"))
            }
        )
    } else {
        format!("Обновлено {when}")
    }
}
pub fn subscription_status(
    ui: &mut egui::Ui,
    report: Option<&notetaker_llm::calendar::SubscriptionStatus>,
    error: Option<&str>,
    busy: bool,
) {
    let p = theme::Palette::of(ui);
    if let Some(report) = report
        && let Some(at) = report.updated_at
    {
        status_line(
            ui,
            theme::icons::CHECK,
            &format!(
                "{} · событий на ближайшие дни: {}",
                snapshot_label(at, Utc::now(), report.error.is_some() || error.is_some()),
                report.events.iter().filter(|event| event.end > at).count()
            ),
            p.muted,
        );
    } else {
        status_line(
            ui,
            theme::icons::REFRESH_CW,
            if busy {
                "Обновляем календарь…"
            } else {
                "Календарь ещё не обновлён"
            },
            p.muted,
        );
    }
    if let Some(error) = report.and_then(|r| r.error.as_deref()).or(error) {
        theme::callout(ui, theme::icons::CIRCLE_ALERT, |ui| {
            ui.colored_label(p.red, error);
            ui.label("Проверьте сеть и сохранённую ICS-подписку, затем обновите календарь.");
        });
    }
    if let Some(report) = report
        && !report.skipped.is_empty()
    {
        status_line(
            ui,
            theme::icons::TRIANGLE_ALERT,
            &skipped_text(&report.skipped),
            p.warning,
        );
        theme::disclosure("Пропущенные события").show(ui, |ui| {
            for event in &report.skipped {
                ui.add(egui::Label::new(&event.title).truncate())
                    .on_hover_text(&event.title);
            }
        });
    }
}
#[derive(Default)]
pub struct HomeRows {
    key: Option<(u64, chrono::NaiveDate)>,
    rows: Vec<usize>,
}
impl HomeRows {
    fn update(&mut self, events: &[CalendarEvent], revision: u64, now: DateTime<Utc>) {
        let today = now.with_timezone(&chrono::Local).date_naive();
        if self.key == Some((revision, today)) {
            return;
        }
        let tomorrow = today.succ_opt().unwrap_or(today);
        self.rows.clear();
        self.rows.extend(
            events
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.end.with_timezone(&chrono::Local).date_naive() >= today
                        && e.start.with_timezone(&chrono::Local).date_naive() <= tomorrow
                })
                .map(|(i, _)| i),
        );
        self.rows.sort_by_key(|index| events[*index].start);
        self.key = Some((revision, today));
    }
}
/// First representable instant of the next local day, including midnight DST gaps.
pub fn next_local_midnight(now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    use chrono::TimeZone;
    let day = now.with_timezone(&chrono::Local).date_naive().succ_opt()?;
    let midnight = day.and_hms_opt(0, 0, 0)?;
    for minute in 0..=2880 {
        let wall = midnight.checked_add_signed(chrono::Duration::minutes(minute))?;
        if let Some(at) = chrono::Local.from_local_datetime(&wall).earliest()
            && at.with_timezone(&Utc) > now
        {
            return Some(at.with_timezone(&Utc));
        }
    }
    None
}
pub struct Home<'a> {
    pub revision: u64,
    pub events: &'a [CalendarEvent],
    pub reports: &'a [notetaker_llm::calendar::SubscriptionStatus],
    pub error: Option<&'a str>,
    pub loading: bool,
    pub now: DateTime<Utc>,
    pub reason: Option<&'a str>,
}
#[derive(Default)]
pub enum HomeAction {
    #[default]
    None,
    Settings,
    Start(CalendarEvent),
}
pub fn home(
    ui: &mut egui::Ui,
    model: Home<'_>,
    expanded: &mut bool,
    cache: &mut HomeRows,
) -> HomeAction {
    cache.update(model.events, model.revision, model.now);
    let mut action = HomeAction::None;
    let p = theme::Palette::of(ui);
    theme::card(p, false).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 24.0), egui::Sense::hover());
            theme::paint_icon(ui, rect.center(), theme::icons::CALENDAR, 18.0, p.text);
            ui.label(
                egui::RichText::new("Календарь")
                    .font(theme::semibold(theme::BODY))
                    .color(p.text),
            );
        });
        let today = model.now.with_timezone(&chrono::Local).date_naive();
        let events = cache
            .rows
            .iter()
            .filter_map(|i| model.events.get(*i))
            .filter(|event| event.end > model.now);
        let count = events.clone().count();
        let issue = model
            .error
            .or_else(|| model.reports.iter().find_map(|r| r.error.as_deref()));
        let skipped = model.reports.iter().map(|r| r.skipped.len()).sum::<usize>();
        if let Some(issue) = issue {
            ui.add(egui::Label::new(egui::RichText::new(issue).color(p.warning)).wrap());
        }
        if issue.is_some()
            && theme::button(ui, "Открыть настройки календаря", ButtonStyle::Ghost).clicked()
        {
            action = HomeAction::Settings;
        }
        if model.loading {
            ui.horizontal(|ui| {
                crate::theme::spinner(ui);
                ui.label("Загружаем календарь…");
            });
        } else if count == 0 {
            ui.label(if issue.is_some() {
                "Календарь пока недоступен"
            } else {
                "Сегодня и завтра встреч нет"
            });
        }
        let mut day = None;
        for event in events.take(if *expanded { count } else { 6 }) {
            let date = event
                .start
                .with_timezone(&chrono::Local)
                .date_naive()
                .max(today);
            if day != Some(date) {
                theme::section_caption(
                    ui,
                    if date == today {
                        "Сегодня"
                    } else {
                        "Завтра"
                    },
                );
                day = Some(date);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(if event.all_day {
                        "Весь день".into()
                    } else {
                        event
                            .start
                            .with_timezone(&chrono::Local)
                            .format("%H:%M")
                            .to_string()
                    })
                    .color(p.muted),
                );
                ui.add(egui::Label::new(&event.link.title).wrap());
                if call_link(event).is_some() {
                    ui.label(
                        egui::RichText::new("Звонок")
                            .font(theme::medium(theme::SMALL))
                            .color(p.accent),
                    );
                }
                if event.start <= model.now {
                    ui.colored_label(p.accent, "идёт");
                }
                if !event.all_day
                    && event.start <= model.now + chrono::Duration::minutes(5)
                    && ui
                        .add_enabled(
                            model.reason.is_none(),
                            theme::styled_button(p, action_label(event), ButtonStyle::Secondary),
                        )
                        .on_disabled_hover_text(model.reason.unwrap_or_default())
                        .clicked()
                {
                    action = HomeAction::Start(event.clone());
                }
            });
        }
        if count > 6
            && theme::button(
                ui,
                &if *expanded {
                    "Свернуть".into()
                } else {
                    format!("ещё {}", count - 6)
                },
                ButtonStyle::Ghost,
            )
            .clicked()
        {
            *expanded = !*expanded;
        }
        if issue.is_none() && skipped > 0 {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!("Не показано событий: {skipped} ·"))
                        .small()
                        .color(p.muted),
                );
                if theme::button(ui, "Подробнее", ButtonStyle::Ghost).clicked() {
                    action = HomeAction::Settings;
                }
            });
        }
    });
    ui.add_space(theme::SPACE[2]);
    action
}

#[cfg(test)]
mod visibility_tests;
