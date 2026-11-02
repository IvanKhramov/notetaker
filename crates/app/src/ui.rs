use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use eframe::egui::{self, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use notetaker_core::Mode;

use crate::{
    attachments::ImageInput,
    config::{MODELS, ProviderSettings},
    detection::CallPrompt,
    state::{AppState, Document, Phase, Tab, edit_segment, speaker, timestamp},
    theme::{self, ButtonStyle, Palette, ToastLevel},
    worker::Command,
};

const TEXT_SCROLL_MARGIN: egui::Margin = egui::Margin {
    right: 16,
    ..egui::Margin::ZERO
};

struct Toast {
    at: Instant,
    text: String,
    path: Option<std::path::PathBuf>,
    level: ToastLevel,
}

pub struct NotetakerApp {
    state: AppState,
    people: crate::people::Panel,
    organization: crate::organization::Panel,
    trackers: crate::trackers::Panel,
    cache: CommonMarkCache,
    image_viewer: Option<String>,
    enhanced_preview: bool,
    show_record: bool,
    show_settings: bool,
    quickstart: crate::quickstart::Panel,
    tasks_all: bool,
    tasks_mine: bool,
    tasks_person: String,
    tasks_by_meeting: bool,
    toasts: VecDeque<Toast>,
    main_rect: Option<egui::Rect>,
    home: bool,
    chat_page: bool,
    ask_text: String,
    focus_search: bool,
    maintenance: crate::maintenance::Maintenance,
    #[cfg(any(windows, target_os = "macos"))]
    desktop: Option<crate::desktop::Desktop>,
    #[cfg(any(windows, target_os = "macos"))]
    desktop_attempted: bool,
    #[cfg(any(windows, target_os = "macos"))]
    applied_hotkey: String,
}
impl NotetakerApp {
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            people: crate::people::Panel::default(),
            organization: Default::default(),
            trackers: Default::default(),
            cache: CommonMarkCache::default(),
            image_viewer: None,
            enhanced_preview: true,
            show_record: false,
            show_settings: false,
            quickstart: Default::default(),
            tasks_all: false,
            tasks_mine: false,
            tasks_person: String::new(),
            tasks_by_meeting: false,
            toasts: VecDeque::new(),
            main_rect: None,
            home: true,
            chat_page: false,
            ask_text: String::new(),
            focus_search: false,
            maintenance: Default::default(),
            #[cfg(any(windows, target_os = "macos"))]
            desktop: None,
            #[cfg(any(windows, target_os = "macos"))]
            desktop_attempted: false,
            #[cfg(any(windows, target_os = "macos"))]
            applied_hotkey: String::new(),
        }
    }
    fn can_record(&self) -> bool {
        self.state.ready
            && !self.state.recording.busy()
            && !self.state.settings_pending
            && !self.state.downloading
            && !self.state.quickstart.probe_busy
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.state.query)
                .id_salt("library_search")
                .hint_text(if cfg!(target_os = "macos") {
                    "Поиск…  Cmd+K"
                } else {
                    "Поиск…  Ctrl+K"
                })
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_search) {
            search.request_focus();
        }
        if search.changed() {
            self.state.query_changed = Some(Instant::now());
            self.home = true;
            self.chat_page = false;
            self.people.close();
            self.state.tasks_open = false;
        }
        ui.add_space(20.0);
        if theme::nav_row(
            ui,
            "Встречи",
            self.state.folder_filter.is_none()
                && !self.chat_page
                && !self.people.active()
                && !self.state.tasks_open,
        )
        .clicked()
        {
            self.home = true;
            self.chat_page = false;
            self.people.close();
            self.state.tasks_open = false;
            if self.state.folder_filter.take().is_some() {
                self.state.record_mode = self.state.settings.mode;
                self.state.record_mode_edited = false;
                self.state.refresh_results();
            }
        }
        if theme::nav_row(ui, "Чат", self.chat_page).clicked() {
            self.chat_page = true;
            self.people.close();
            self.state.tasks_open = false;
            self.state.chat.open(None, String::new());
        }
        ui.add_space(24.0);
        let previous = self.state.folder_filter.clone();
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - 136.0).max(48.0))
            .id_salt("sidebar_folders")
            .show(ui, |ui| self.organization.sidebar(ui, &mut self.state));
        if previous != self.state.folder_filter {
            self.home = true;
            self.chat_page = false;
            self.people.close();
            self.state.tasks_open = false;
        }
        ui.add_space((ui.available_height() - 88.0).max(0.0));
        egui::containers::menu::MenuButton::from_button(theme::styled_button(
            Palette::of(ui),
            "Ещё",
            ButtonStyle::Ghost,
        ))
        .ui(ui, |ui| {
            ui.set_min_width(180.0);
            if theme::nav_row(ui, "Задачи", self.state.tasks_open).clicked() {
                self.people.close();
                self.chat_page = false;
                self.state.tasks_open = true;
                self.state.refresh_tasks();
                ui.close();
            }
            self.people.sidebar(ui, &mut self.state);
            if self.people.active() {
                self.chat_page = false;
            }
            ui.separator();
            if ui.button("Быстрый старт").clicked() {
                self.quickstart.open = true;
                ui.close();
            }
            if ui.button("Перечитать библиотеку").clicked() {
                self.state.flush();
                self.state.refresh();
                ui.close();
            }
        });
        if theme::nav_row(ui, "Настройки", self.show_settings).clicked() {
            self.show_settings = true;
        }
    }
    fn meetings(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        let title = self
            .state
            .folder_filter
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("Встречи");
        theme::page_heading(ui, title);
        let mut select = None;
        let mut start_event = None;
        let today = chrono::Local::now().date_naive();
        egui::ScrollArea::vertical()
            .id_salt("meetings")
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                let now = chrono::Utc::now();
                let mut events: Vec<_> = self
                    .state
                    .calendar_events
                    .iter()
                    .filter(|e| e.end >= now && e.start <= now + chrono::Duration::hours(12))
                    .collect();
                events.sort_by_key(|e| e.start);
                if self.state.query.is_empty()
                    && self.state.folder_filter.is_none()
                    && !events.is_empty()
                {
                    theme::section_caption(ui, "Скоро");
                    ui.add_space(8.0);
                    for event in events.into_iter().take(30) {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [54.0, 40.0],
                                egui::Label::new(
                                    RichText::new(if event.all_day {
                                        "Весь день".into()
                                    } else {
                                        event
                                            .start
                                            .with_timezone(&chrono::Local)
                                            .format("%H:%M")
                                            .to_string()
                                    })
                                    .small()
                                    .color(p.muted),
                                ),
                            );
                            let reserve = 172.0;
                            ui.add_sized(
                                [(ui.available_width() - reserve).max(40.0), 40.0],
                                egui::Label::new(&event.link.title)
                                    .truncate()
                                    .halign(egui::Align::Min),
                            )
                            .on_hover_text(&event.link.title);
                            if let Some(url) = &event.link.call_url {
                                ui.add_sized(
                                    [76.0, 24.0],
                                    egui::Label::new(
                                        RichText::new(call_platform(url)).small().color(p.muted),
                                    )
                                    .truncate(),
                                );
                            }
                            if ui
                                .add_enabled(
                                    cfg!(any(windows, target_os = "macos")) && self.can_record(),
                                    theme::styled_button(p, "Записать", ButtonStyle::Ghost),
                                )
                                .clicked()
                            {
                                start_event = Some(event.clone());
                            }
                        });
                        ui.separator();
                    }
                    ui.add_space(28.0);
                }
                let mut previous = None;
                let mut count = 0;
                for row in &self.state.rows {
                    if !self.organization.visible(&row.meta, &self.state) {
                        continue;
                    }
                    count += 1;
                    let start = row.meta.started_at.with_timezone(&chrono::Local);
                    let date = start.date_naive();
                    if previous != Some(date) {
                        ui.add_space(16.0);
                        theme::section_caption(ui, &day_label(date, today));
                        ui.add_space(8.0);
                        previous = Some(date);
                    }
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 56.0),
                        egui::Sense::click_and_drag(),
                    );
                    if response.hovered() || self.organization.selected(row.meeting.path()) {
                        ui.painter().rect_filled(rect, 6, p.hover);
                    }
                    ui.painter().text(
                        egui::pos2(rect.left() + 8.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        start.format("%H:%M"),
                        egui::FontId::proportional(12.0),
                        p.muted,
                    );
                    let attendees = row
                        .meta
                        .calendar_event
                        .as_ref()
                        .map(|e| e.attendees.as_slice())
                        .unwrap_or_default();
                    let avatar_width = (attendees.len().min(3) as f32 * 24.0 + 12.0).max(16.0);
                    let title_rect = egui::Rect::from_min_max(
                        rect.min + egui::vec2(70.0, 0.0),
                        rect.max - egui::vec2(avatar_width, 0.0),
                    );
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(title_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        |ui| {
                            ui.add(egui::Label::new(&row.meta.title).truncate());
                        },
                    );
                    for (n, attendee) in attendees.iter().take(3).enumerate() {
                        let center =
                            egui::pos2(rect.right() - 20.0 - n as f32 * 24.0, rect.center().y);
                        ui.painter().circle_filled(center, 13.0, p.soft);
                        ui.painter().text(
                            center,
                            egui::Align2::CENTER_CENTER,
                            attendee
                                .label()
                                .chars()
                                .next()
                                .unwrap_or('?')
                                .to_uppercase(),
                            egui::FontId::proportional(11.0),
                            p.muted,
                        );
                    }
                    let response = response.on_hover_text(if row.snippet.is_empty() {
                        &row.meta.title
                    } else {
                        &row.snippet
                    });
                    if self.organization.row(ui, &response, &row.meeting) {
                        select = Some((row.meeting.clone(), row.anchor.clone()));
                    }
                    ui.separator();
                }
                if count == 0 && !self.state.listing {
                    ui.add_space(48.0);
                    ui.label(if self.state.query.is_empty() {
                        "Здесь будут ваши встречи"
                    } else {
                        "Встречи не найдены"
                    });
                    ui.weak(if self.state.query.is_empty() {
                        "Начните запись. Заметки и транскрипт сохранятся здесь."
                    } else {
                        "Попробуйте другие слова или другую папку."
                    });
                }
                if self.state.listing {
                    ui.spinner();
                }
            });
        if let Some(event) = start_event {
            self.home = false;
            self.state.start_event(event);
        }
        if let Some((meeting, anchor)) = select {
            self.home = false;
            self.people.close();
            if anchor.is_none() {
                self.state.tab = Tab::Enhanced;
                self.enhanced_preview = true;
            }
            self.state.select_anchor(meeting, anchor);
        }
    }
    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        let note = !self.home && !self.chat_page && !self.people.active() && !self.state.tasks_open;
        let mut ask = false;
        let mut stop = false;
        theme::bar(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let r = &self.state.recording;
                if r.busy() {
                    if r.phase == Phase::Recording {
                        theme::waveform(ui);
                    } else {
                        ui.spinner();
                    }
                    ui.label(
                        RichText::new(timestamp(r.elapsed().as_millis() as u64)).color(p.muted),
                    );
                    if matches!(r.phase, Phase::Finalizing) {
                        ui.weak("Сохраняем…");
                    }
                }
                let space = (if note {
                    if r.busy() { 236.0 } else { 250.0 }
                } else {
                    146.0
                }) + if self.ask_text.trim().is_empty() {
                    0.0
                } else {
                    96.0
                };
                let response = ui.add_sized(
                    [(ui.available_width() - space).max(60.0), 32.0],
                    egui::TextEdit::singleline(&mut self.ask_text)
                        .frame(egui::Frame::NONE)
                        .hint_text(
                            if note || (self.chat_page && self.state.chat.scope.is_some()) {
                                "Спросить про эту встречу…"
                            } else {
                                "Спросить по всем встречам…"
                            },
                        ),
                );
                ask |= response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if !self.ask_text.trim().is_empty() {
                    ask |= ui
                        .add_enabled(
                            self.state.ready
                                && !self.state.chat.busy
                                && !r.busy()
                                && !self.state.unsaved()
                                && !self.state.settings_pending,
                            egui::Button::new("Спросить"),
                        )
                        .on_disabled_hover_text("Дождитесь сохранения встречи")
                        .clicked();
                }
                if note
                    && ui
                        .button(if self.state.tab == Tab::Transcript {
                            "Заметка"
                        } else {
                            "Транскрипт"
                        })
                        .clicked()
                {
                    self.state.tab = if self.state.tab == Tab::Transcript {
                        Tab::Enhanced
                    } else {
                        Tab::Transcript
                    };
                }
                let stopping =
                    !r.importing && matches!(r.phase, Phase::Starting | Phase::Recording);
                if stopping {
                    stop = ui.button("Стоп").clicked();
                } else if !r.busy()
                    && ui
                        .add_enabled(
                            self.can_record(),
                            egui::Button::new(RichText::new("Новая запись").color(p.on_accent))
                                .fill(p.accent)
                                .stroke(egui::Stroke::NONE),
                        )
                        .clicked()
                {
                    self.state.prefill_call_title();
                    self.show_record = true;
                    self.state.inventory_pending = true;
                    self.state.send(Command::Inventory);
                }
            });
        });
        if stop {
            self.state.toggle_recording();
        }
        if ask
            && !self.ask_text.trim().is_empty()
            && self.state.ready
            && !self.state.chat.busy
            && !self.state.recording.busy()
            && !self.state.unsaved()
            && !self.state.settings_pending
        {
            if !self.chat_page {
                let scope = if note {
                    self.state
                        .selected
                        .as_ref()
                        .and_then(|p| self.state.documents.get(p))
                        .map(|d| (d.data.meeting.clone(), d.data.meta.title.clone()))
                } else {
                    None
                };
                self.state.chat.open(
                    scope.as_ref().map(|(m, _)| m.clone()),
                    scope.map(|(_, t)| t).unwrap_or_default(),
                );
            }
            self.state.chat.question = std::mem::take(&mut self.ask_text);
            self.state.ask();
            self.chat_page = true;
            self.people.close();
            self.state.tasks_open = false;
        }
    }

    fn tasks_screen(&mut self, ui: &mut egui::Ui) {
        theme::page_heading(ui, "Задачи");
        ui.horizontal_wrapped(|ui| {
            theme::segmented(
                ui,
                &mut self.tasks_all,
                &[(false, "Открытые"), (true, "Все")],
            );
            ui.checkbox(&mut self.tasks_mine, "Мои");
            ui.checkbox(&mut self.tasks_by_meeting, "По встречам");
            if ui
                .add_enabled(!self.state.tasks_busy, egui::Button::new("Обновить"))
                .clicked()
            {
                self.state.refresh_tasks();
            }
        });
        let mut people: Vec<_> = self
            .state
            .tasks
            .iter()
            .map(|task| task.owner.clone())
            .filter(|s| !s.is_empty())
            .collect();
        people.sort();
        people.dedup();
        egui::ComboBox::from_id_salt("task_person")
            .selected_text(if self.tasks_person.is_empty() {
                "Все ответственные"
            } else {
                &self.tasks_person
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.tasks_person, String::new(), "Все ответственные");
                for person in people {
                    ui.selectable_value(&mut self.tasks_person, person.clone(), person);
                }
            });
        if self.tasks_mine
            && self.state.settings.user_name.trim().is_empty()
            && self.state.settings.user_aliases.trim().is_empty()
        {
            ui.label("Укажите «Как вас зовут» в настройках");
        }
        let names: Vec<_> = std::iter::once(self.state.settings.user_name.as_str())
            .chain(self.state.settings.user_aliases.split([',', ';', '\n']))
            .collect();
        let mut tasks: Vec<_> = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                self.organization.task_visible(&task.path, &self.state)
                    && (self.tasks_all || !task.completed)
                    && (self.tasks_person.is_empty()
                        || notetaker_core::person_matches(&task.owner, &self.tasks_person))
                    && (!self.tasks_mine
                        || names
                            .iter()
                            .any(|name| notetaker_core::person_matches(&task.owner, name)))
            })
            .collect();
        let group = |task: &notetaker_core::Task| {
            if self.tasks_by_meeting {
                task.meeting_title.clone()
            } else if task.owner.is_empty() {
                "Без ответственного".into()
            } else {
                task.owner.clone()
            }
        };
        tasks.sort_by_key(|task| {
            (
                group(task),
                self.tasks_by_meeting.then(|| task.path.clone()),
                task.due.is_none(),
                task.due,
                task.path.clone(),
                task.line,
            )
        });
        let can_toggle = !self.state.tasks_busy
            && !self.state.unsaved()
            && !self.state.recording.busy()
            && self.state.enhancing.is_none();
        let mut send_task = None;
        let mut toggle = None;
        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt("all_tasks")
            .show(ui, |ui| {
                let mut previous = None;
                for task in tasks {
                    let title = group(task);
                    let key = (
                        title.clone(),
                        self.tasks_by_meeting.then(|| task.path.clone()),
                    );
                    if previous.as_ref() != Some(&key) {
                        ui.add_space(12.0);
                        theme::section_caption(ui, &title);
                        previous = Some(key);
                    }
                    ui.push_id((&task.path, task.line), |ui| {
                        theme::card(Palette::of(ui), false).show(ui, |ui| {
                            ui.set_width((ui.available_width() - 2.0).max(1.0));
                            ui.horizontal(|ui| {
                                let mut completed = task.completed;
                                if ui
                                    .add_enabled(
                                        can_toggle,
                                        egui::Checkbox::without_text(&mut completed),
                                    )
                                    .changed()
                                {
                                    toggle = Some((task.clone(), completed));
                                }
                                let overdue = !task.completed
                                    && task.due.is_some_and(|date| {
                                        date < chrono::Local::now().date_naive()
                                    });
                                let label = RichText::new(&task.text)
                                    .font(theme::semibold(14.0))
                                    .color(if overdue {
                                        Palette::of(ui).red
                                    } else {
                                        Palette::of(ui).text
                                    });
                                if ui
                                    .add(
                                        egui::Label::new(if task.completed {
                                            label.strikethrough()
                                        } else {
                                            label
                                        })
                                        .wrap()
                                        .sense(egui::Sense::click()),
                                    )
                                    .clicked()
                                {
                                    open = Some(task.clone());
                                }
                            });
                            ui.label(
                                RichText::new(format!(
                                    "{} · {} · {}:{}",
                                    task.owner,
                                    task.due.map_or_else(
                                        || task.due_text.clone(),
                                        |date| date.format("%d.%m.%Y").to_string()
                                    ),
                                    task.meeting_title,
                                    task.line
                                ))
                                .small()
                                .color(Palette::of(ui).muted),
                            );
                            if let Some(url) = &task.issue_url {
                                ui.hyperlink_to("В трекере", url);
                            } else if !task.completed
                                && ui
                                    .add_enabled(
                                        can_toggle,
                                        theme::styled_button(
                                            Palette::of(ui),
                                            "Отправить в трекер",
                                            ButtonStyle::Ghost,
                                        ),
                                    )
                                    .clicked()
                            {
                                send_task = Some(task.clone());
                            }
                        });
                    });
                }
                if self.state.tasks_busy {
                    ui.spinner();
                }
            });
        if let Some(task) = send_task {
            self.trackers.prepare(ui.ctx(), &self.state, vec![task]);
        }
        if let Some((task, completed)) = toggle {
            self.state.toggle_task(task, completed);
        }
        if let Some(task) = open {
            self.enhanced_preview = false;
            self.state.open_task(&task);
        }
    }
    fn meeting(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.state.selected.clone() else {
            self.home = true;
            return;
        };
        let can_enhance = self.state.enhancing.is_none()
            && !self.state.recording.busy()
            && !self.state.unsaved()
            && !self.state.settings_pending;
        let locked =
            self.state.recording.busy() && self.state.recording.path.as_ref() == Some(&path);
        let global_allowed = !self.state.unsaved();
        let speaker_busy = self.state.documents.values().any(|d| d.speaker_ui.busy);
        let can_cancel = self.state.generation_cancellable();
        let Some(document) = self.state.documents.get_mut(&path) else {
            ui.spinner();
            return;
        };
        let mut enhance = false;
        let mut export = None;
        let mut series_action = None;
        ui.horizontal(|ui| {
            if theme::button(ui, "‹ Встречи", ButtonStyle::Ghost).clicked() {
                self.home = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::containers::menu::MenuButton::from_button(theme::styled_button(
                    Palette::of(ui),
                    "Ещё",
                    ButtonStyle::Ghost,
                ))
                .ui(ui, |ui| {
                    if ui.button("Редактировать мои заметки").clicked() {
                        self.state.tab = Tab::Notes;
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            document.generation.is_none() && !locked,
                            egui::Button::new("Редактировать конспект"),
                        )
                        .clicked()
                    {
                        self.state.tab = Tab::Enhanced;
                        self.enhanced_preview = false;
                        ui.close();
                    }
                    if ui.button("Читать документ").clicked() {
                        self.state.tab = Tab::Enhanced;
                        self.enhanced_preview = true;
                        ui.close();
                    }
                    ui.separator();
                    egui::ComboBox::from_id_salt("template")
                        .selected_text(&document.template)
                        .show_ui(ui, |ui| {
                            for name in &self.state.templates {
                                ui.selectable_value(&mut document.template, name.clone(), name);
                            }
                        });
                    enhance = ui
                        .add_enabled(can_enhance, egui::Button::new("Пересоздать конспект"))
                        .clicked();
                    if enhance {
                        ui.close();
                    }
                    if ui.button("Копировать").clicked() {
                        ui.ctx().copy_text(if document.data.enhanced.is_empty() {
                            document.data.notes.clone()
                        } else {
                            document.data.enhanced.clone()
                        });
                        ui.close();
                    }
                    if ui.button("Показать в папке").clicked() {
                        export = Some(Command::Reveal(path.clone()));
                        ui.close();
                    }
                    ui.menu_button("Серия встречи", |ui| {
                        series_action =
                            self.people
                                .header(ui, &document.data.meta, !locked && can_enhance);
                    });
                    if ui.button("Экспорт конспекта Markdown").clicked() {
                        export = Some(Command::Export {
                            meeting: document.data.meeting.clone(),
                            text: document.data.enhanced.clone(),
                        });
                        ui.close();
                    }
                    ui.menu_button("Экспорт транскрипта", |ui| {
                        for (format, label) in [
                            (notetaker_core::ExportFormat::Md, "Markdown"),
                            (notetaker_core::ExportFormat::Srt, "SRT"),
                            (notetaker_core::ExportFormat::Vtt, "VTT"),
                            (notetaker_core::ExportFormat::Csv, "CSV"),
                            (notetaker_core::ExportFormat::Json, "JSON"),
                        ] {
                            if ui.button(label).clicked() {
                                export = Some(Command::ExportTranscript {
                                    meeting: document.data.meeting.clone(),
                                    transcript: document.data.transcript.clone(),
                                    format,
                                });
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
        ui.add_space(24.0);
        let mut title = egui::text::LayoutJob::simple(
            document.data.meta.title.clone(),
            theme::semibold(30.0),
            Palette::of(ui).text,
            ui.available_width(),
        );
        title.wrap.max_rows = 2;
        ui.label(title).on_hover_text(&document.data.meta.title);
        ui.add_space(12.0);
        let start = document.data.meta.started_at.with_timezone(&chrono::Local);
        let participants = document
            .data
            .meta
            .calendar_event
            .as_ref()
            .map(|e| e.attendees.len())
            .unwrap_or_else(|| {
                document
                    .data
                    .transcript
                    .segments
                    .iter()
                    .map(|s| &s.speaker.0)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            });
        ui.horizontal_wrapped(|ui| {
            theme::chip(ui, &start.format("%d.%m.%Y · %H:%M").to_string());
            theme::chip(ui, &format!("Участники: {participants}"));
            // Folder assignment keeps its existing atomic save and sync-conflict handling.
            self.organization.meeting(ui, &mut self.state);
        });
        self.trackers.meeting(ui, &self.state);
        let Some(document) = self.state.documents.get_mut(&path) else {
            return;
        };
        if !document.data.meta.capture_issues.is_empty() {
            ui.collapsing("Неполный захват звука", |ui| {
                for issue in &document.data.meta.capture_issues {
                    ui.small(format!(
                        "{} с {}: {}",
                        speaker(issue.channel),
                        timestamp(issue.from_ms),
                        issue.reason
                    ));
                }
            });
        }
        if document.summary_stale() && document.generation.is_none() {
            ui.horizontal(|ui| {
                ui.weak("Заметки изменились после конспекта");
                enhance |= ui
                    .add_enabled(can_enhance, egui::Button::new("Обновить"))
                    .clicked();
            });
        }
        if document.generation.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Пишу конспект…");
                if can_cancel && ui.button("Отменить").clicked() {
                    self.state.generation_cancel.cancel();
                }
            });
        }
        ui.add_space(28.0);
        if self
            .state
            .search_target
            .as_ref()
            .is_some_and(|(p, _)| p == &path)
            && let Some((_, anchor)) = self.state.search_target.take()
        {
            match anchor {
                notetaker_core::Anchor::Notes { line } => {
                    self.state.tab = Tab::Notes;
                    document.inserted_cursor = Some(
                        document
                            .data
                            .notes
                            .split_inclusive('\n')
                            .take(line.saturating_sub(1))
                            .map(|s| s.chars().count())
                            .sum(),
                    );
                }
                notetaker_core::Anchor::Summary { line } => {
                    self.state.tab = Tab::Enhanced;
                    self.enhanced_preview = false;
                    self.state.task_target = Some((path.clone(), line));
                }
                notetaker_core::Anchor::Transcript { segment, .. } => {
                    self.state.tab = Tab::Transcript;
                    document.speaker_ui.scroll_to = Some(segment);
                }
            }
        }
        let mut images = Vec::new();
        let mut speaker_edit = None;
        if self.state.tab == Tab::Transcript {
            speaker_edit = transcript_view(
                ui,
                document,
                &path,
                locked,
                self.state
                    .settings
                    .voice_library
                    .then_some(&self.state.voices),
                !locked
                    && !document.saving
                    && !speaker_busy
                    && document.attachments_pending == 0
                    && self.state.enhancing.is_none(),
                global_allowed,
            );
        } else if self.state.tab == Tab::Notes
            || locked
            || (document.data.enhanced.is_empty() && document.generation.is_none())
        {
            if !locked && !document.data.enhanced.is_empty() && ui.small_button("Готово").clicked()
            {
                self.state.tab = Tab::Enhanced;
                self.enhanced_preview = true;
            }
            if document.data.enhanced.is_empty() && document.generation.is_none() && !locked {
                enhance |= ui
                    .add_enabled(can_enhance, egui::Button::new("Создать конспект"))
                    .clicked();
                ui.add_space(12.0);
            }
            images = notes_editor(ui, document, &path, &mut self.image_viewer);
        } else if !self.enhanced_preview && document.generation.is_none() {
            if ui.small_button("Готово").clicked() {
                self.enhanced_preview = true;
            }
            let target = self
                .state
                .task_target
                .as_ref()
                .filter(|(p, _)| p == &path)
                .map(|(_, line)| *line);
            egui::ScrollArea::vertical()
                .id_salt(("enhanced_edit", &path))
                .show(ui, |ui| {
                    let id = egui::Id::unique(("enhanced_edit", &path));
                    let cursor = target.map(|line| {
                        document
                            .data
                            .enhanced
                            .split_inclusive('\n')
                            .take(line.saturating_sub(1))
                            .map(|s| s.chars().count())
                            .sum::<usize>()
                    });
                    if let Some(cursor) = cursor {
                        let mut state =
                            egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(
                                egui::text::CCursor::new(cursor),
                            )));
                        state.store(ui.ctx(), id);
                    }
                    let output = egui::TextEdit::multiline(&mut document.data.enhanced)
                        .font(egui::TextStyle::Body)
                        .id(id)
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY)
                        .desired_rows(24)
                        .show(ui);
                    if let Some(cursor) = cursor {
                        output.response.request_focus();
                        ui.scroll_to_rect(
                            output
                                .galley
                                .pos_from_cursor(egui::text::CCursor::new(cursor))
                                .translate(output.galley_pos.to_vec2()),
                            Some(egui::Align::Center),
                        );
                        self.state.task_target = None;
                    }
                    if output.response.changed() {
                        document.dirty.enhanced = true;
                        document.changed();
                    }
                });
        } else {
            egui::ScrollArea::vertical()
                .id_salt(("document", &path))
                .content_margin(TEXT_SCROLL_MARGIN)
                .show(ui, |ui| {
                    enhanced_document(
                        ui,
                        &mut self.cache,
                        &document.data.notes,
                        document
                            .generation
                            .as_ref()
                            .unwrap_or(&document.data.enhanced),
                        &path,
                        &mut self.image_viewer,
                    );
                });
        }
        if let Some(edit) = speaker_edit {
            self.state.edit_speakers(edit);
        }
        if let Some(action) = series_action {
            match action {
                crate::people::HeaderAction::Open(meeting) => self.state.select(meeting),
                crate::people::HeaderAction::Insert(previous) => {
                    self.state.insert_series_context(previous)
                }
            }
        }
        self.state.attach(images);
        if enhance {
            self.enhanced_preview = true;
            self.state.enhance();
        }
        if let Some(command) = export {
            self.state.send(command);
        }
    }
    fn notifications(&mut self, ctx: &egui::Context) {
        for (level, text) in self
            .state
            .warnings
            .drain(..)
            .chain(self.state.recording.messages.drain(..))
            .map(|text| (ToastLevel::Warning, text))
            .chain(self.state.notices.drain(..))
        {
            if self.toasts.len() == 5 {
                self.toasts.pop_front();
            }
            self.toasts.push_back(Toast {
                at: Instant::now(),
                text: notetaker_core::display_text(&text),
                path: None,
                level,
            });
        }
        for path in self.state.exported.drain(..) {
            if self.toasts.len() == 5 {
                self.toasts.pop_front();
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            self.toasts.push_back(Toast {
                at: Instant::now(),
                text: format!("Файл обновлён: {name}"),
                path: Some(path),
                level: ToastLevel::Success,
            });
        }
        self.toasts
            .retain(|toast| toast.at.elapsed() < Duration::from_secs(6));
        if self.toasts.is_empty() && self.state.error.is_none() {
            return;
        }
        egui::Area::new(egui::Id::unique("notifications"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -18.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                let mut reveal = None;
                for toast in &self.toasts {
                    theme::bar(Palette::of(ui)).show(ui, |ui| {
                        ui.colored_label(toast.level.color(Palette::of(ui)), &toast.text);
                        if let Some(path) = &toast.path
                            && ui.button("Показать в папке").clicked()
                        {
                            reveal = Some(path.clone());
                        }
                    });
                }
                if let Some(path) = reveal {
                    self.state.send(Command::Reveal(path));
                }
                if let Some(error) = self.state.error.clone() {
                    theme::bar(Palette::of(ui)).show(ui, |ui| {
                        ui.colored_label(
                            ToastLevel::Error.color(Palette::of(ui)),
                            notetaker_core::display_text(&error),
                        );
                        ui.horizontal(|ui| {
                            if ui.button("Настройки").clicked() {
                                self.show_settings = true;
                            }
                            if ui.button("Закрыть").clicked() {
                                self.state.error = None;
                            }
                        });
                    });
                }
            });
    }
    fn recording_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_record;
        theme::window(ctx, "Новая запись").open(&mut open).collapsible(false).show(ctx, |ui| {
            ui.label("Название"); ui.text_edit_singleline(&mut self.state.title);
            model_selector(ui, "record_model", &mut self.state.record_model);
            self.state.record_mode_edited |= mode_selector(ui, &mut self.state.record_mode);
            egui::ComboBox::from_id_salt("audio_session").selected_text(self.state.inventory.sessions.iter().find(|s| Some(s.pid) == self.state.process_id)
                .map_or_else(|| "Весь системный звук".to_owned(), |s| format!("{} (PID {})", s.exe.as_deref().unwrap_or(&s.name), s.pid))).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.state.process_id, None, "Весь системный звук");
                for session in &self.state.inventory.sessions {
                    if session.pid != 0 { ui.selectable_value(&mut self.state.process_id, Some(session.pid), format!("{} (PID {}){}", session.exe.as_deref().unwrap_or(&session.name), session.pid, if session.active { " · активна" } else { "" })); }
                }
            });
            if ui.add_enabled(!self.state.inventory_pending, egui::Button::new("Обновить аудиосессии")).clicked() {
                self.state.inventory_pending = true; self.state.send(Command::Inventory);
            }
            if self.state.inventory_pending { ui.spinner(); }
            if let Some(error) = &self.state.inventory_error { ui.label(error); }
            for warning in &self.state.inventory.warnings { ui.small(warning); }
            #[cfg(target_os = "macos")]
            ui.small("macOS: весь системный звук, кроме notetaker. Нужны разрешения Microphone и System Audio Recording.");
            #[cfg(not(any(windows, target_os = "macos")))]
            ui.label("Захват звука пока реализован только на Windows. Библиотека, поиск, правки и конспекты работают на этой ОС.");
            ui.horizontal(|ui| {
                if ui.add_enabled(cfg!(any(windows, target_os = "macos")) && !self.state.recording.busy(), egui::Button::new(RichText::new("Начать запись").color(Palette::of(ui).on_accent)).fill(Palette::of(ui).accent)).clicked() { self.state.toggle_recording(); self.show_record = false; }
                if ui.button("Скачать модель…").clicked() { self.show_settings = true; }
            });
        });
        self.show_record &= open;
    }
    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let mut quickstart = false;
        theme::window(ctx, "Настройки").collapsible(false).open(&mut open).default_width(640.0).default_height(580.0).resizable(true).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if ui.button("Быстрый старт").clicked() { self.quickstart.open = true; quickstart = true; }
                let settings = &mut self.state.settings_draft;
                theme::setting_row(ui, "Тема", |ui| {
                    egui::ComboBox::from_id_salt("theme").selected_text(settings.theme.label()).show_ui(ui, |ui| {
                        for theme in [crate::theme::Theme::Light, crate::theme::Theme::Dark, crate::theme::Theme::System] { ui.selectable_value(&mut settings.theme, theme, theme.label()); }
                    });
                });
                theme::setting_row(ui, "Ваше имя", |ui| { ui.text_edit_singleline(&mut settings.user_name); });
                theme::setting_row(ui, "Модель распознавания", |ui| { model_selector(ui, "default_model", &mut settings.model); });
                theme::setting_row(ui, "Горячая клавиша записи", |ui| { ui.text_edit_singleline(&mut settings.hotkey); });
                theme::toggle(ui, &mut settings.detect_calls, "Предлагать запись звонков");
                theme::toggle(ui, &mut settings.auto_summary, "Создавать конспект после встречи");
                theme::setting_row(ui, "Библиотека", |ui| {
                    let mut library = notetaker_core::display_path(&settings.library);
                    if ui.text_edit_singleline(&mut library).changed() { settings.library = library.into(); }
                });
                ui.collapsing("Запись и хранение", |ui| {
                    theme::setting_row(ui, "Режим новых встреч", |ui| { mode_selector(ui, &mut settings.mode); });
                    ui.small("Local: текст только у локального AI. Hybrid: текст может быть отправлен выбранному провайдеру. Аудио остаётся на компьютере.");
                    theme::setting_row(ui, "Хранить аудио", |ui| {
                        let mut forever = settings.retention_days.is_none();
                        if ui.checkbox(&mut forever, "Всегда").changed() { settings.retention_days = if forever { None } else { Some(7) }; }
                        if let Some(days) = &mut settings.retention_days { ui.add(egui::DragValue::new(days).range(0..=36_500).suffix(" дней")); }
                    });
                    ui.small("0 дней: удалить после чистового прохода. Применяется к новым записям.");
                    theme::setting_row(ui, "Автостоп по тишине", |ui| { ui.add(egui::DragValue::new(&mut settings.auto_stop_minutes).range(0..=u32::MAX).suffix(" мин")); });
                    ui.small("0 минут: выключен. Завершение звонка не останавливает запись.");
                    theme::toggle(ui, &mut settings.detect_browsers, "Учитывать браузеры");
                    theme::toggle(ui, &mut settings.detect_any_microphone, "Любое приложение с микрофоном");
                    ui.label("Свои приложения для звонков (одно имя процесса на строку)");
                    ui.add(egui::TextEdit::multiline(&mut settings.call_apps).desired_rows(2));
                    #[cfg(target_os = "macos")]
                    ui.small("macOS: системный звук целиком, кроме notetaker. Разрешения: Microphone и System Audio Recording.");
                });
                ui.collapsing("Распознавание и участники", |ui| {
                    theme::toggle(ui, &mut settings.diarize, "Разделять собеседников");
                    theme::setting_row(ui, "Число собеседников", |ui| {
                        egui::ComboBox::from_id_salt("speakers").selected_text(settings.speakers.map_or("Авто".into(), |n| n.to_string())).show_ui(ui, |ui| {
                            ui.selectable_value(&mut settings.speakers, None, "Авто");
                            for n in 1..=32 { ui.selectable_value(&mut settings.speakers, Some(n), n.to_string()); }
                        });
                    });
                    theme::setting_row(ui, "Потоков ASR (0: авто)", |ui| { ui.add(egui::DragValue::new(&mut settings.asr_threads).range(0..=8)); });
                    theme::setting_row(ui, "Ускорение ASR", |ui| {
                        egui::ComboBox::from_id_salt("asr_acceleration").selected_text(if settings.asr_acceleration == notetaker_asr_proto::Provider::Auto { "Авто" } else { "CPU" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut settings.asr_acceleration, notetaker_asr_proto::Provider::Auto, "Авто");
                            ui.selectable_value(&mut settings.asr_acceleration, notetaker_asr_proto::Provider::Cpu, "CPU");
                        });
                    });
                    theme::toggle(ui, &mut settings.voice_library, "Запоминать голоса");
                    ui.small("Голосовые отпечатки хранятся только локально.");
                    theme::setting_row(ui, "Другие варианты вашего имени", |ui| { ui.text_edit_singleline(&mut settings.user_aliases); });
                    theme::toggle(ui, &mut settings.digest_reminder, "Напоминать о дайджесте по пятницам");
                    ui.label("Словарь (один термин на строку)");
                    ui.add(egui::TextEdit::multiline(&mut settings.vocabulary).desired_rows(3));
                    ui.small("Алиасы: werf = верф, вёрф.");
                });
                self.maintenance.settings(ui, settings);
                ui.add_space(16.0); theme::section_caption(ui, "Модели ASR");
                crate::quickstart::model_control(ui, &mut self.state, "gigaam-v3", "GigaAM v3 · русский");
                crate::quickstart::model_control(ui, &mut self.state, "parakeet-v3", "Parakeet v3 · многоязычный");
                crate::quickstart::model_control(ui, &mut self.state, "auto", "Авто (рус/англ) · обе ASR-модели + Whisper tiny");
                crate::quickstart::model_control(ui, &mut self.state, "diarization", "Модели разделения собеседников (47 МБ)");
                theme::toggle(ui, &mut self.state.settings_draft.auto_download_models, "Скачивать модели автоматически");
                theme::toggle(ui, &mut self.state.settings_draft.hide_quickstart, "Больше не показывать быстрый старт при запуске");
                ui.collapsing("Голоса", |ui| {
                    if self.state.voices.voices.is_empty() { ui.small("Голосов пока нет. Назовите участника в транскрипте или нажмите «Это я»."); }
                    let mut delete = None;
                    for voice in &self.state.voices.voices {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}{}", voice.name, if voice.is_me { " (это я)" } else { "" }));
                            if ui.button("Удалить").clicked() { delete = Some(voice.name.clone()); }
                        });
                    }
                    if let Some(name) = delete { self.state.send(Command::Voices { root:self.state.settings.library.clone(), delete:Some(name) }); }
                });
                theme::toggle(ui, &mut self.state.settings_draft.auto_summary, "Делать конспект автоматически");
                ui.add_space(16.0); theme::section_caption(ui, "Календари");
                ui.small("Приватные ICS-подписки. URL хранится в Credential Manager. Обновление при запуске и каждые 10 минут.");
                ui.horizontal(|ui| { ui.label("Часовой пояс без TZID:"); ui.text_edit_singleline(&mut self.state.settings_draft.calendar_timezone); });
                #[cfg(not(any(windows, target_os = "macos")))]
                ui.small("Сохранение URL доступно на Windows, как и ключей шлюзов.");
                let mut remove_calendar = None;
                for calendar in &mut self.state.settings_draft.calendars {
                    ui.push_id(&calendar.id, |ui| {
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut calendar.name);
                            if ui.button("Удалить").clicked() { remove_calendar = Some(calendar.id.clone()); }
                        });
                        let secret = self.state.calendar_secrets.entry(calendar.id.clone()).or_default();
                        ui.add(egui::TextEdit::singleline(secret).password(true).hint_text("Приватный HTTPS URL ICS (пусто - сохранённый)"));
                    });
                }
                if let Some(id) = remove_calendar {
                    self.state.settings_draft.calendars.retain(|s| s.id != id);
                    self.state.calendar_secrets.remove(&id);
                }
                let connect = if self.state.settings_draft.calendars.is_empty() {
                    ui.add_enabled(cfg!(any(windows, target_os = "macos")), egui::Link::new("Подключить календарь")).clicked()
                } else { ui.add_enabled(cfg!(any(windows, target_os = "macos")), egui::Button::new("Добавить календарь")).clicked() };
                if connect {
                    self.state.settings_draft.calendars.push(notetaker_llm::calendar::Subscription { id: format!("calendar-{}", uuid::Uuid::now_v7()), name: "Календарь".into() });
                }
                if ui.add_enabled(!self.state.calendar_busy, egui::Button::new("Обновить календари")).clicked() { self.state.refresh_calendar(); }
                ui.small(&self.state.calendar_status);
                theme::toggle(ui, &mut self.state.settings_draft.mcp_exclude_local, "MCP: не отдавать Local-встречи");
                ui.separator();
                crate::quickstart::semantic_step(ui,&mut self.state);
                self.organization.shared_step(ui, !self.state.semantic_running && self.state.can_save_settings());
                ui.collapsing("Удалённый поиск по смыслу (только Hybrid)",|ui| {
                    ui.small("Используется только без локальной модели. Local-встречи остаются в поиске по словам.");
                    let llm=&mut self.state.llm_draft;
                    egui::ComboBox::from_id_salt("embedding_provider").selected_text(if llm.embeddings.provider.is_empty() {"Выключено"} else {&llm.embeddings.provider}).show_ui(ui,|ui| {
                        ui.selectable_value(&mut llm.embeddings.provider,String::new(),"Выключено");
                        for provider in &llm.providers {if provider.kind=="openai" {ui.selectable_value(&mut llm.embeddings.provider,provider.id.clone(),&provider.id);}}
                    });
                    ui.add(egui::TextEdit::singleline(&mut llm.embeddings.model).hint_text("Модель embeddings"));
                });
                ui.add_space(16.0); theme::section_caption(ui, "Подключение AI");
                let can_default = self.state.ready && !self.state.recording.busy() && self.state.enhancing.is_none() && !self.state.chat.busy && !self.state.unsaved() && !self.state.settings_pending;
                let actions = self.state.connections.ui(ui, &self.state.llm_draft.defaults.hybrid, can_default);
                for action in actions { self.state.send(Command::Ai(action)); }
                ui.collapsing("Модели по сценариям", |ui| {
                    let llm = &mut self.state.llm_draft;
                    for (label, choice) in [("Конспект", &mut llm.scenarios.summary), ("Чат", &mut llm.scenarios.chat)] {
                        ui.push_id(label, |ui| {
                            ui.label(label);
                            egui::ComboBox::from_id_salt("provider").selected_text(if choice.provider.is_empty() { "По умолчанию" } else { &choice.provider }).show_ui(ui, |ui| {
                                ui.selectable_value(&mut choice.provider, String::new(), "По умолчанию");
                                for provider in &llm.providers { ui.selectable_value(&mut choice.provider, provider.id.clone(), &provider.id); }
                            });
                            ui.add(egui::TextEdit::singleline(&mut choice.model).hint_text("Модель (пусто - модель провайдера)"));
                        });
                    }
                    ui.small("Для Local облачный сценарий заменяется локальным провайдером по умолчанию и его моделью.");
                });
                ui.collapsing("Расширенные", |ui| {
                ui.label("Указывайте имена переменных окружения; значения ключей здесь не хранятся. Для Local разрешены только loopback-провайдеры.");
                let llm = &mut self.state.llm_draft;
                ui.horizontal(|ui| { ui.label("По умолчанию Local:"); ui.text_edit_singleline(&mut llm.defaults.local); });
                ui.horizontal(|ui| { ui.label("По умолчанию Hybrid:"); ui.text_edit_singleline(&mut llm.defaults.hybrid); });
                let mut remove = None;
                for (index, provider) in llm.providers.iter_mut().enumerate() {
                    ui.push_id(index, |ui| { ui.scope(|ui| {
                        ui.horizontal(|ui| { ui.label("id"); ui.text_edit_singleline(&mut provider.id); if ui.button("Удалить провайдер").clicked() { remove = Some(index); } });
                        egui::ComboBox::from_id_salt("kind").selected_text(&provider.kind).show_ui(ui, |ui| {
                            ui.selectable_value(&mut provider.kind, "openai".into(), "OpenAI-compatible (Ollama, OpenRouter, vLLM…)");
                            ui.selectable_value(&mut provider.kind, "anthropic".into(), "Anthropic");
                            ui.selectable_value(&mut provider.kind, "claude-cli".into(), "Claude CLI");
                            ui.selectable_value(&mut provider.kind, "codex-cli".into(), "Codex CLI");
                        });
                        ui.horizontal(|ui| { ui.label("base_url"); ui.text_edit_singleline(&mut provider.base_url); });
                        ui.horizontal(|ui| { ui.label("model"); ui.text_edit_singleline(&mut provider.model); });
                        egui::ComboBox::from_id_salt("key_source").selected_text(&provider.key_source).show_ui(ui, |ui| {
                            ui.selectable_value(&mut provider.key_source, "env".into(), "env");
                            ui.selectable_value(&mut provider.key_source, "credential".into(), "credential");
                        });
                        let mut env = provider.api_key_env.clone().unwrap_or_default();
                        ui.horizontal(|ui| { ui.label("api_key_env"); if ui.text_edit_singleline(&mut env).changed() { provider.api_key_env = (!env.is_empty()).then_some(env.clone()); } });
                        ui.horizontal(|ui| { ui.label("context_tokens"); ui.add(egui::DragValue::new(&mut provider.context_tokens).range(1..=2_000_000)); });
                    }); });
                }
                if let Some(index) = remove { llm.providers.remove(index); }
                if ui.button("Добавить провайдер").clicked() { llm.providers.push(ProviderSettings { id: format!("provider-{}", llm.providers.len() + 1), ..ProviderSettings::default() }); }
                });
                ui.separator();
                let can_save = self.state.ready && !self.state.recording.busy() && self.state.enhancing.is_none() && !self.state.chat.busy && !self.state.unsaved() && !self.state.settings_pending;
                if ui.add_enabled(can_save, theme::styled_button(Palette::of(ui), "Сохранить настройки", ButtonStyle::Primary)).clicked() { self.state.save_settings(); }
                if self.state.recording.busy() || self.state.enhancing.is_some() { ui.small("Сохранение настроек доступно после завершения записи и конспекта."); }
                ui.small(if cfg!(target_os = "macos") { "Ключи и URL календарей хранятся в macOS Keychain. Настройки приложения и LLM сохраняются отдельно." } else { "Настройки приложения и LLM сохраняются отдельно. Ключи шлюзов хранятся в Credential Manager на Windows; также поддерживается окружение." });
                #[cfg(not(any(windows, target_os = "macos")))]
                ui.small("Трей и глобальная клавиша включены только на Windows; GTK/AppIndicator не нужны для сборки Linux.");
            });
        });
        self.show_settings = open && !quickstart;
    }
    fn call_notification(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.state.call_prompt.clone() else {
            return;
        };
        let mut accept = false;
        let mut dismiss = false;
        let mut builder = egui::ViewportBuilder::default();
        if let Some(rect) = self.main_rect {
            builder = builder.with_position(rect.right_bottom() - egui::vec2(368.0, 160.0));
        }
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("call-notification"),
            builder
                .with_title("notetaker · звонок")
                .with_inner_size([350.0, 142.0])
                .with_resizable(false)
                .with_decorations(false)
                .with_transparent(true)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_active(false),
            |ui, _| {
                let p = Palette::of(ui);
                egui::Frame::new()
                    .fill(p.card)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(12)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.set_width(318.0);
                        let (title, question, button) = match &prompt {
                            CallPrompt::Start {
                                app, meeting_title, ..
                            } => (
                                format!("Звонок в {app}"),
                                meeting_title.as_deref().unwrap_or("Записать встречу?"),
                                "Записать",
                            ),
                            CallPrompt::Stop { .. } => (
                                "Звонок закончился".into(),
                                "Остановить запись?",
                                "Остановить",
                            ),
                        };
                        ui.label(RichText::new(title).strong());
                        ui.weak(question);
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(
                                self.state.ready
                                    && !self.state.downloading
                                    && !self.state.quickstart.probe_busy
                                    && !self.state.settings_pending,
                                |ui| {
                                    accept =
                                        theme::button(ui, button, ButtonStyle::Primary).clicked();
                                },
                            );
                            dismiss =
                                theme::button(ui, "Не сейчас", ButtonStyle::Secondary).clicked();
                        });
                    });
                if ui.input(|input| input.viewport().close_requested()) {
                    dismiss = true;
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            },
        );
        if accept {
            self.state.accept_call();
        } else if dismiss {
            self.state.dismiss_call();
        }
    }
    fn indicator(&mut self, ctx: &egui::Context) {
        let show = ctx.input(|input| {
            indicator_visible(
                self.state.recording.phase,
                input.viewport().minimized.unwrap_or(false),
                input.viewport().focused.unwrap_or(false),
            )
        });
        if !show {
            return;
        }
        let mut stop = false;
        let mut open = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("recording-indicator"),
            egui::ViewportBuilder::default()
                .with_title("notetaker · запись")
                .with_inner_size([220.0, 52.0])
                .with_min_inner_size([220.0, 52.0])
                .with_max_inner_size([220.0, 52.0])
                .with_resizable(false)
                .with_decorations(false)
                .with_transparent(true)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_active(false),
            |ui, _| {
                (open, stop) = recording_pill(ui, self.state.recording.elapsed());
                if ui.input(|i| i.viewport().close_requested()) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            },
        );
        if open {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Minimized(false),
            );
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
        if stop {
            self.state.toggle_recording();
        }
    }
    #[cfg(any(windows, target_os = "macos"))]
    fn desktop(&mut self, ctx: &egui::Context) {
        use crate::desktop::{Action, Desktop};
        if !self.state.ready {
            return;
        }
        if !self.desktop_attempted {
            self.desktop_attempted = true;
            match Desktop::new(ctx) {
                Ok(desktop) => self.desktop = Some(desktop),
                Err(error) => {
                    self.state.error = Some(format!("Не удалось включить трей/хоткей: {error:#}"))
                }
            }
        }
        if let Some(desktop) = &mut self.desktop {
            if desktop.set_recording(self.state.recording.busy()).is_err() {
                notetaker_runtime::diagnostics::event("tray icon update failed");
            }
            let settings = &self.state.settings;
            if self.applied_hotkey != settings.hotkey {
                self.applied_hotkey = settings.hotkey.clone();
                if let Err(error) = desktop.set_hotkey(&settings.hotkey) {
                    self.state.error = Some(format!("Хоткей не зарегистрирован: {error:#}"));
                }
            }
            for action in desktop.poll() {
                match action {
                    Action::Show => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    Action::Toggle => self.state.toggle_recording(),
                    Action::Quit => self.state.exit(),
                }
            }
        }
    }
}
impl eframe::App for NotetakerApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe calls this even when the root window is hidden in the tray.
        let previous = self.state.recording.path.clone();
        self.state.poll();
        if self.state.recording.path != previous && self.state.recording.busy() {
            self.home = false;
            self.chat_page = false;
            self.people.close();
            self.state.tasks_open = false;
        }
        #[cfg(target_os = "macos")]
        if crate::macos::quit_requested() {
            self.state.exit();
        }
        self.organization.poll(ctx, &mut self.state);
        self.quickstart.tick(&mut self.state);
        if self.maintenance.tick(
            ctx,
            self.state.ready && self.state.settings.check_updates,
            !self.state.ready
                || self.state.recording.busy()
                || self.state.downloading
                || self.state.quickstart.probe_busy
                || self.state.settings_pending
                || self.state.enhancing.is_some()
                || self.state.chat.busy
                || self.state.unsaved(),
        ) {
            self.state.exit();
        }
        self.state.settings.theme.apply(ctx);
        ctx.input(|input| {
            if !input.viewport().minimized.unwrap_or(false)
                && let Some(rect) = input.viewport().outer_rect
            {
                self.main_rect = Some(rect);
            }
        });
        #[cfg(any(windows, target_os = "macos"))]
        self.desktop(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.state.can_exit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            #[cfg(any(windows, target_os = "macos"))]
            if self.desktop.is_some() && !self.state.exit_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.state.exit();
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            self.state.exit();
        }
        if self.state.can_exit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.state.recording.busy() || self.state.call_prompt.is_some() {
            // Allow the UI to create the indicator when a hotkey starts a
            // recording while the root window is hidden.
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
        ctx.request_repaint_after(Duration::from_millis(
            if self.state.recording.busy() || self.state.enhancing.is_some() {
                100
            } else {
                500
            },
        ));
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.organization.windows(
            &ctx,
            !self.state.semantic_running && self.state.can_save_settings(),
        );
        self.trackers.poll(&ctx, &mut self.state);
        self.maintenance.dialogs(
            &ctx,
            !self.state.ready
                || self.state.recording.busy()
                || self.state.downloading
                || self.state.quickstart.probe_busy
                || self.state.settings_pending
                || self.state.enhancing.is_some()
                || self.state.chat.busy
                || self.state.unsaved(),
        );
        let dropped = ctx.input_mut(|input| std::mem::take(&mut input.raw.dropped_files));
        self.state.attach(
            dropped
                .into_iter()
                .map(|file| ImageInput::File(file.path().to_owned()))
                .collect(),
        );
        let p = Palette::of(ui);
        self.people.poll(&ctx, &mut self.state);
        if self.state.search_target.is_some() || self.state.task_target.is_some() {
            self.home = false;
            self.chat_page = false;
            if self.state.task_target.is_some() {
                self.enhanced_preview = false;
                self.state.tab = Tab::Enhanced;
            }
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.people.close();
            self.state.tasks_open = false;
            self.focus_search = true;
        }
        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(220.0)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(egui::Margin::symmetric(16, 24)),
            )
            .show(ui, |ui| self.sidebar(ui));
        egui::Panel::bottom("bottom_bar")
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(24, 16)),
            )
            .show(ui, |ui| {
                let width = ui.available_width().min(660.0);
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 56.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| self.bottom_bar(ui),
                    );
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(
                if ctx.content_rect().width() < 900.0 {
                    24
                } else {
                    48
                },
            ))
            .show(ui, |ui| {
                ui.add_enabled_ui(!self.state.settings_pending, |ui| {
                    let selected = self.state.selected.clone();
                    if self.people.active() {
                        self.people.screen(ui, &mut self.state);
                    } else if self.state.tasks_open {
                        self.tasks_screen(ui);
                    } else if self.chat_page {
                        if let Some(crate::chat::Action::Open(meeting, anchor)) = self
                            .state
                            .chat
                            .content(ui, self.state.folder_filter.as_deref())
                        {
                            self.state.select_anchor(meeting, anchor);
                            self.chat_page = false;
                            self.home = false;
                        }
                    } else if self.home {
                        self.meetings(ui);
                    } else {
                        self.meeting(ui);
                    }
                    if self.state.selected != selected {
                        self.home = false;
                        self.chat_page = false;
                        if self.state.search_target.is_none() && self.state.task_target.is_none() {
                            self.state.tab = Tab::Enhanced;
                            self.enhanced_preview = true;
                        }
                    }
                });
            });
        if let Some(uri) = self.image_viewer.clone() {
            let mut open = true;
            theme::window(&ctx, "Изображение")
                .open(&mut open)
                .resizable(true)
                .default_size([800.0, 600.0])
                .show(&ctx, |ui| {
                    ui.add(
                        egui::Image::from_uri(uri)
                            .max_size(ui.available_size())
                            .shrink_to_fit(),
                    );
                });
            if !open {
                self.image_viewer = None;
            }
        }
        self.notifications(&ctx);
        if self.show_record {
            self.recording_dialog(&ctx);
        }
        if self.show_settings {
            self.settings_dialog(&ctx);
        }
        self.indicator(&ctx);
        self.call_notification(&ctx);
        if self.quickstart.show(
            &ctx,
            &mut self.state,
            &mut self.maintenance,
            &mut self.organization,
        ) {
            self.show_settings = true;
        }
    }
}
/// A click opens the main window; dragging any part except Stop moves the pill.
fn recording_pill(ui: &mut egui::Ui, elapsed: Duration) -> (bool, bool) {
    let p = Palette::of(ui);
    let background = ui.interact(
        ui.max_rect(),
        ui.scope_id().with("drag"),
        egui::Sense::click_and_drag(),
    );
    if background.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    let mut stop = false;
    theme::bar(p)
        .corner_radius(26)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_min_width(198.0);
            ui.horizontal(|ui| {
                theme::waveform(ui);
                ui.add(egui::Label::new(timestamp(elapsed.as_millis() as u64)).selectable(false));
                ui.add_space(16.0);
                stop = ui.button("Стоп").clicked();
            });
        });
    (background.clicked(), stop)
}

fn call_platform(url: &str) -> &str {
    if url.contains("zoom.") {
        "Zoom"
    } else if url.contains("meet.google.") {
        "Meet"
    } else if url.contains("teams.") {
        "Teams"
    } else if url.contains("telemost.") {
        "Телемост"
    } else {
        "Звонок"
    }
}

fn note_line_key(line: &str) -> &str {
    line.trim()
        .trim_start_matches(['#', '-', '*', '+', '>', ' '])
        .trim()
}

/// Only exact source lines are attributed to the author. Paraphrases remain AI text.
fn document_runs(notes: &str, enhanced: &str) -> Vec<(bool, String)> {
    let originals: std::collections::BTreeSet<_> = notes
        .lines()
        .map(note_line_key)
        .filter(|s| !s.is_empty())
        .collect();
    let enhanced_lines: std::collections::BTreeSet<_> =
        enhanced.lines().map(note_line_key).collect();
    let missing = notes
        .lines()
        .filter(|line| !enhanced_lines.contains(note_line_key(line)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut runs = Vec::new();
    if !missing.trim().is_empty() {
        runs.push((true, format!("{missing}\n\n")));
    }
    for line in enhanced.split_inclusive('\n') {
        let own = originals.contains(note_line_key(line));
        if let Some((last_own, text)) = runs.last_mut()
            && *last_own == own
        {
            text.push_str(line);
        } else {
            runs.push((own, line.to_owned()));
        }
    }
    runs
}

fn enhanced_document(
    ui: &mut egui::Ui,
    cache: &mut CommonMarkCache,
    notes: &str,
    enhanced: &str,
    path: &std::path::Path,
    viewer: &mut Option<String>,
) {
    let p = Palette::of(ui);
    for (own, text) in document_runs(notes, enhanced) {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.style_mut()
                .text_styles
                .insert(egui::TextStyle::Heading, theme::semibold(16.0));
            let ink = if own { p.text } else { p.muted };
            ui.visuals_mut().override_text_color = Some(ink);
            ui.visuals_mut().widgets.noninteractive.fg_stroke.color = ink;
            ui.visuals_mut().widgets.active.fg_stroke.color = ink;
            markdown_preview(ui, cache, &text, path, viewer);
        });
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Local => "Local (только локальные модели)",
        Mode::Hybrid => "Hybrid (разрешён облачный LLM)",
    }
}
fn mode_selector(ui: &mut egui::Ui, mode: &mut Mode) -> bool {
    let mut selected = false;
    ui.horizontal_wrapped(|ui| {
        ui.label("Режим:");
        selected |= ui.selectable_value(mode, Mode::Local, "Local").clicked();
        selected |= ui.selectable_value(mode, Mode::Hybrid, "Hybrid").clicked();
    });
    ui.small(mode_label(*mode));
    selected
}
fn model_selector(ui: &mut egui::Ui, id: &str, model: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(crate::config::model_label(model))
        .show_ui(ui, |ui| {
            for name in MODELS {
                ui.selectable_value(model, name.into(), crate::config::model_label(name));
            }
        });
}

fn indicator_visible(phase: Phase, minimized: bool, focused: bool) -> bool {
    phase == Phase::Recording && (minimized || !focused)
}
fn day_label(date: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    if date == today {
        "Сегодня".into()
    } else if Some(date) == today.pred_opt() {
        "Вчера".into()
    } else {
        date.format("%d.%m.%Y").to_string()
    }
}
// Pure string conversion keeps Windows canonical paths testable on every OS.
fn file_uri(path: &str) -> Option<String> {
    let path = notetaker_core::display_path(path);
    let path = if path.starts_with('/') {
        path
    } else {
        path.replace('\\', "/")
    };
    let prefix = if path.starts_with("//") {
        "file:"
    } else if path.starts_with('/') {
        "file://"
    } else if path.as_bytes().get(1..3) == Some(b":/") {
        "file:///"
    } else {
        return None;
    };
    let mut uri = prefix.to_owned();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(uri, "%{byte:02X}").ok()?;
        }
    }
    Some(uri)
}

fn image_uri(path: &std::path::Path, relative: &str) -> Option<String> {
    let image = notetaker_core::attachment_image_path(path, relative)?;
    file_uri(image.to_str()?)
}

fn preview_markdown(text: &str, path: &std::path::Path) -> (String, Vec<Option<String>>) {
    let images = notetaker_core::markdown_images(text);
    let uris = images
        .iter()
        .map(|(_, destination)| image_uri(path, destination))
        .collect();
    let mut markdown = text.to_owned();
    // Replace inline images too: CommonMark must never load an unchecked URI.
    for (index, (range, _)) in images.iter().enumerate().rev() {
        markdown.replace_range(
            range.clone(),
            &format!("\n\n<div data-notetaker-image=\"{index}\"></div>\n\n"),
        );
    }
    (markdown, uris)
}

fn attachment_strip(
    ui: &mut egui::Ui,
    notes: &str,
    path: &std::path::Path,
    viewer: &mut Option<String>,
) {
    let images = notetaker_core::attachment_references(notes);
    if images.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for relative in images {
            let Some(uri) = image_uri(path, &relative) else {
                continue;
            };
            if ui
                .add(
                    egui::Image::from_uri(&uri)
                        .max_size(egui::vec2(100.0, 64.0))
                        .sense(egui::Sense::click()),
                )
                .on_hover_text("Открыть изображение")
                .clicked()
            {
                *viewer = Some(uri);
            }
        }
    });
}

fn markdown_preview(
    ui: &mut egui::Ui,
    cache: &mut CommonMarkCache,
    text: &str,
    path: &std::path::Path,
    viewer: &mut Option<String>,
) {
    // CommonMark's image widget has no click handler; use a checked HTML callback.
    let image_text: Vec<_> = notetaker_core::markdown_images(text)
        .into_iter()
        .map(|(range, _)| text[range].to_owned())
        .collect();
    let (markdown, uris) = preview_markdown(text, path);
    let clicked = std::rc::Rc::new(std::cell::Cell::new(None));
    let selected = clicked.clone();
    let image_uris = uris.clone();
    let render = move |ui: &mut egui::Ui, html: &str| {
        if let Some(index) = html
            .trim()
            .strip_prefix("<div data-notetaker-image=\"")
            .and_then(|text| text.strip_suffix("\"></div>"))
            .and_then(|text| text.parse::<usize>().ok())
            .filter(|index| *index < image_uris.len())
        {
            let Some(uri) = &image_uris[index] else {
                ui.label(&image_text[index]);
                return;
            };
            if ui
                .add(
                    egui::Image::from_uri(uri)
                        .max_width(ui.available_width())
                        .sense(egui::Sense::click()),
                )
                .on_hover_text("Открыть изображение")
                .clicked()
            {
                selected.set(Some(index));
            }
        } else {
            ui.label(html);
        }
    };
    CommonMarkViewer::new()
        .render_html_fn(Some(&render))
        .show(ui, cache, &markdown);
    if let Some(index) = clicked.get() {
        *viewer = uris[index].clone();
    }
}

fn notes_editor(
    ui: &mut egui::Ui,
    document: &mut Document,
    path: &std::path::Path,
    viewer: &mut Option<String>,
) -> Vec<ImageInput> {
    attachment_strip(ui, &document.data.notes, path, viewer);
    let mut images = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt(("notes", path))
        .content_margin(TEXT_SCROLL_MARGIN)
        .show(ui, |ui| {
            let id = egui::Id::unique(("notes_edit", path));
            if ui.memory(|memory| memory.has_focus(id)) {
                ui.input_mut(|input| {
                    input.events.retain(|event| {
                        if let egui::Event::PasteImage(image) = event {
                            images.push(ImageInput::Clipboard(image.clone()));
                            false
                        } else {
                            true
                        }
                    })
                });
            }
            let scroll_cursor = document.inserted_cursor.take();
            if let Some(cursor) = scroll_cursor {
                let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(cursor),
                    )));
                state.store(ui.ctx(), id);
            }
            let output = egui::TextEdit::multiline(&mut document.data.notes)
                .id(id)
                .hint_text("Пишите важное здесь…")
                .font(egui::TextStyle::Body)
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .desired_rows(24)
                .show(ui);
            if let Some(cursor) = scroll_cursor {
                output.response.request_focus();
                ui.scroll_to_rect(
                    output
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(cursor))
                        .translate(output.galley_pos.to_vec2()),
                    Some(egui::Align::Center),
                );
            }
            if let Some(range) = output.cursor_range {
                let [start, end] = range.sorted_cursors();
                document.notes_cursor = Some(start.index.into()..end.index.into());
            }
            if output.response.changed() {
                document.dirty.notes = true;
                document.changed();
            }
        });
    images
}
fn transcript_view(
    ui: &mut egui::Ui,
    document: &mut Document,
    path: &std::path::Path,
    live: bool,
    voices: Option<&notetaker_core::VoiceLibrary>,
    editable: bool,
    global_allowed: bool,
) -> Option<notetaker_core::SpeakerEdit> {
    let p = Palette::of(ui);
    let mut edits = Vec::new();
    let mut action = None;
    if !live {
        ui.add_enabled_ui(editable, |ui| {
            action = crate::speakers::controls(
                ui,
                &document.data.transcript,
                &document.data.meta,
                &mut document.speaker_ui,
                voices,
                global_allowed,
            );
        });
    }
    let mut selected = None;
    let mut scrolled = false;
    egui::ScrollArea::vertical()
        .id_salt(("transcript", path))
        .content_margin(TEXT_SCROLL_MARGIN)
        .stick_to_bottom(live)
        .show(ui, |ui| {
            for (index, segment) in document.data.transcript.segments.iter().enumerate() {
                let me = segment.speaker.0 == "me"
                    || document.data.transcript.self_speaker.as_ref() == Some(&segment.speaker.0);
                let width = (ui.available_width() * 0.90).max(1.0);
                let response = ui
                    .push_id(segment.id, |ui| {
                        ui.with_layout(
                            if me {
                                egui::Layout::top_down(egui::Align::RIGHT)
                            } else {
                                egui::Layout::top_down(egui::Align::LEFT)
                            },
                            |ui| {
                                ui.horizontal(|ui| {
                                    if live || !editable {
                                        let name = document
                                            .data
                                            .transcript
                                            .speaker_name(&segment.speaker.0);
                                        if ui
                                            .link(
                                                egui::RichText::new(name).color(
                                                    crate::speakers::color(&segment.speaker.0),
                                                ),
                                            )
                                            .clicked()
                                        {
                                            crate::people::request(
                                                ui,
                                                crate::people::Request {
                                                    name: name.into(),
                                                    email: String::new(),
                                                    voice: document
                                                        .data
                                                        .transcript
                                                        .speaker_voice_ids
                                                        .get(&segment.speaker.0)
                                                        .cloned(),
                                                },
                                            );
                                        }
                                    } else {
                                        ui.add_enabled_ui(editable, |ui| {
                                            if let Some(rename) = crate::speakers::rename_menu(
                                                ui,
                                                &document.data.transcript,
                                                &document.data.meta,
                                                &segment.speaker.0,
                                                voices,
                                                global_allowed,
                                            ) {
                                                action = Some(rename);
                                            }
                                        });
                                    }
                                    let label = format!(
                                        "{}{}",
                                        timestamp(segment.start_ms),
                                        if segment.edited {
                                            " · исправлено"
                                        } else {
                                            ""
                                        }
                                    );
                                    if ui
                                        .add_enabled(
                                            !live && editable,
                                            egui::Button::selectable(
                                                document.speaker_ui.selected.contains(&segment.id),
                                                RichText::new(label).size(12.0).color(p.muted),
                                            ),
                                        )
                                        .clicked()
                                    {
                                        selected = Some(index);
                                    }
                                });
                                egui::Frame::new()
                                    .fill(if document.speaker_ui.selected.contains(&segment.id) {
                                        ui.visuals().selection.bg_fill
                                    } else if me {
                                        p.me
                                    } else {
                                        p.soft
                                    })
                                    .corner_radius(10)
                                    .inner_margin(10)
                                    .show(ui, |ui| {
                                        ui.set_max_width(width);
                                        if live {
                                            ui.add(egui::Label::new(&segment.text).wrap());
                                        } else {
                                            let mut text = segment.text.clone();
                                            let response = ui.add_enabled(
                                                editable,
                                                egui::TextEdit::multiline(&mut text)
                                                    .frame(egui::Frame::NONE)
                                                    .desired_width(width)
                                                    .desired_rows(1),
                                            );
                                            if response.changed() {
                                                edits.push((segment.id, text));
                                            }
                                            if response.clicked() {
                                                selected = Some(index);
                                            }
                                        }
                                    });
                            },
                        );
                    })
                    .response;
                if document.speaker_ui.scroll_to == Some(segment.id) {
                    response.scroll_to_me(Some(egui::Align::Center));
                    scrolled = true;
                }
                ui.add_space(8.0);
            }
            if document.data.transcript.segments.is_empty() {
                ui.weak("Текст появится здесь");
            }
        });
    if scrolled {
        document.speaker_ui.scroll_to = None;
    }
    if let Some(index) = selected {
        let modifiers = ui.input(|input| input.modifiers);
        document.speaker_ui.select(
            &document.data.transcript,
            index,
            modifiers.shift,
            modifiers.ctrl || modifiers.command,
        );
    }
    for (id, text) in edits {
        if edit_segment(&mut document.data.transcript, id, text) {
            document.dirty.transcript = true;
            document.changed();
        }
    }
    action
}

#[cfg(test)]
mod tests {
    #[test]
    fn recording_pill_empty_area_opens_and_drags_without_stopping() {
        use eframe::egui::{self, Event, Modifiers, PointerButton};
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let position = egui::pos2(4.0, 4.0);
        let render = |events| {
            let mut result = (false, false);
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(220.0, 52.0),
                    )),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(root, |ui| {
                            result = super::recording_pill(ui, std::time::Duration::from_secs(48));
                        });
                },
            );
            (result, output)
        };
        render(vec![]).1.drop_without_applying_deltas();
        render(vec![
            Event::PointerMoved(position),
            Event::PointerButton {
                pos: position,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ])
        .1
        .drop_without_applying_deltas();
        let (result, output) = render(vec![Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        assert_eq!(result, (true, false));
        output.drop_without_applying_deltas();
        render(vec![Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }])
        .1
        .drop_without_applying_deltas();
        let (result, output) = render(vec![Event::PointerMoved(egui::pos2(30.0, 16.0))]);
        assert!(!result.1);
        assert!(
            output
                .viewport_output
                .values()
                .flat_map(|v| &v.commands)
                .any(|c| matches!(c, egui::ViewportCommand::StartDrag))
        );
        output.drop_without_applying_deltas();
    }

    #[test]
    fn document_preserves_missing_personal_notes_without_attributing_paraphrases() {
        let notes = "## Решения\n- Переходим на новый пайплайн\n- Не забыть проверить бюджет\n";
        let enhanced =
            "## Решения\n- Переходим на новый пайплайн\n- Запуск нового пайплайна в пятницу\n";
        let runs = super::document_runs(notes, enhanced);
        assert!(
            runs.iter()
                .any(|(own, text)| *own && text.contains("Не забыть проверить бюджет"))
        );
        assert!(
            runs.iter()
                .any(|(own, text)| *own && text.contains("Переходим на новый пайплайн"))
        );
        assert!(
            runs.iter()
                .any(|(own, text)| !*own && text.contains("Запуск нового пайплайна в пятницу"))
        );
        let rendered: String = runs.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(rendered.matches("Переходим на новый пайплайн").count(), 1);
        assert!(rendered.ends_with(enhanced));
    }

    #[test]
    fn empty_personal_notes_leave_all_generated_text_gray() {
        let runs = super::document_runs("", "## Решения\n- Готово\n");
        assert_eq!(runs, vec![(false, "## Решения\n- Готово\n".into())]);
    }

    #[test]
    fn file_uris_strip_windows_verbatim_prefix_and_encode_utf8() {
        for (path, uri) in [
            (
                r"\\?\C:\Users\A B\attachments\тест.png",
                "file:///C:/Users/A%20B/attachments/%D1%82%D0%B5%D1%81%D1%82.png",
            ),
            (r"C:\notes\a#b%.png", "file:///C:/notes/a%23b%25.png"),
            (
                r"\\?\UNC\server\share\a b.png",
                "file://server/share/a%20b.png",
            ),
            (r"\\server\share\a.png", "file://server/share/a.png"),
            ("/tmp/a b.png", "file:///tmp/a%20b.png"),
            (r"/tmp/a\b.png", "file:///tmp/a%5Cb.png"),
        ] {
            assert_eq!(super::file_uri(path).as_deref(), Some(uri));
        }
        assert!(super::file_uri("relative/path.png").is_none());
    }

    #[test]
    fn preview_checks_standalone_inline_and_reference_images() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("attachments")).unwrap();
        std::fs::write(dir.path().join("attachments/a.png"), b"image").unwrap();
        let text = "![](attachments/a.png)\n\ninline ![](file://attacker/share/i.png) and ![](../../x.png)\n\n![remote][r]\n\n[r]: https://example.org/i.png\n\n![outer ![inner](attachments/a.png)](file://attacker/share/i.png)\n";
        let (markdown, uris) = super::preview_markdown(text, dir.path());
        assert_eq!(uris.len(), 5);
        assert!(uris[0].is_some());
        assert!(uris[1..].iter().all(Option::is_none));
        assert!(notetaker_core::markdown_images(&markdown).is_empty());
    }

    use super::*;
    #[test]
    fn editor_preserves_text_paste_and_only_captures_images_while_focused() {
        let root = tempfile::tempdir().unwrap();
        let meeting = notetaker_core::Library::open(root.path())
            .unwrap()
            .create_meeting(notetaker_core::MeetingMeta::new(
                "Images",
                chrono::Utc::now(),
            ))
            .unwrap();
        let path = meeting.path().to_owned();
        let mut document = Document {
            speaker_ui: Default::default(),
            data: crate::worker::DocumentData::read(meeting).unwrap(),
            attachments_pending: 0,
            notes_cursor: None,
            inserted_cursor: None,
            dirty: Default::default(),
            saving: false,
            changed_at: Instant::now(),
            generation: None,
            summary_status: None,
            template: "general".into(),
        };
        let ctx = egui::Context::default();
        let mut viewer = None;
        let image = std::sync::Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
        for focused in [false, true] {
            let input = egui::RawInput {
                events: vec![
                    egui::Event::Paste("Текст".into()),
                    egui::Event::PasteImage(image.clone()),
                ],
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    if focused {
                        ui.memory_mut(|memory| {
                            memory.request_focus(egui::Id::unique(("notes_edit", path.as_path())))
                        });
                    }
                    let imported = notes_editor(ui, &mut document, &path, &mut viewer);
                    assert_eq!(imported.len(), usize::from(focused));
                });
            });
            output.drop_without_applying_deltas();
        }
        assert_eq!(document.data.notes, "Текст");
    }

    #[test]
    fn indicator_only_shows_during_recording_outside_main_window() {
        assert!(!indicator_visible(Phase::Recording, false, true));
        assert!(indicator_visible(Phase::Recording, true, true));
        assert!(indicator_visible(Phase::Recording, false, false));
        for phase in [Phase::Idle, Phase::Starting, Phase::Finalizing] {
            assert!(!indicator_visible(phase, true, false));
        }
    }
    #[test]
    fn groups_by_local_calendar_day_including_year_boundary() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        assert_eq!(day_label(today, today), "Сегодня");
        assert_eq!(day_label(today.pred_opt().unwrap(), today), "Вчера");
        assert_eq!(
            day_label(
                chrono::NaiveDate::from_ymd_opt(2025, 12, 30).unwrap(),
                today
            ),
            "30.12.2025"
        );
    }
}
