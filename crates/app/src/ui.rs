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

struct Toast {
    at: Instant,
    text: String,
    path: Option<std::path::PathBuf>,
    level: ToastLevel,
}

pub struct NotetakerApp {
    state: AppState,
    people: crate::people::Panel,
    cache: CommonMarkCache,
    image_viewer: Option<String>,
    notes_preview: bool,
    enhanced_preview: bool,
    show_record: bool,
    show_import: bool,
    import_path: String,
    show_settings: bool,
    quickstart: crate::quickstart::Panel,
    tasks_all: bool,
    tasks_mine: bool,
    tasks_person: String,
    tasks_by_meeting: bool,
    toasts: VecDeque<Toast>,
    main_rect: Option<egui::Rect>,
    live_summary_tab: bool,
    maintenance: crate::maintenance::Maintenance,
    #[cfg(windows)]
    desktop: Option<crate::desktop::Desktop>,
    #[cfg(windows)]
    desktop_attempted: bool,
    #[cfg(windows)]
    applied_hotkey: String,
}
impl NotetakerApp {
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            people: crate::people::Panel::default(),
            cache: CommonMarkCache::default(),
            image_viewer: None,
            notes_preview: false,
            enhanced_preview: true,
            show_record: false,
            show_import: false,
            import_path: String::new(),
            show_settings: false,
            quickstart: Default::default(),
            tasks_all: false,
            tasks_mine: false,
            tasks_person: String::new(),
            tasks_by_meeting: false,
            toasts: VecDeque::new(),
            main_rect: None,
            live_summary_tab: true,
            maintenance: Default::default(),
            #[cfg(windows)]
            desktop: None,
            #[cfg(windows)]
            desktop_attempted: false,
            #[cfg(windows)]
            applied_hotkey: String::new(),
        }
    }
    fn meetings(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        ui.heading(RichText::new("notetaker").size(20.0).strong());
        self.quickstart.sidebar(ui, &self.state);
        ui.add_space(12.0);
        ui.add_enabled_ui(
            self.state.ready
                && !self.state.recording.busy()
                && !self.state.settings_pending
                && !self.state.downloading
                && !self.state.quickstart.probe_busy,
            |ui| {
                if theme::button(ui, "+ Новая заметка", ButtonStyle::Primary).clicked()
                {
                    self.state.prefill_call_title();
                    self.show_record = true;
                    self.state.inventory_pending = true;
                    self.state.send(Command::Inventory);
                }
                if theme::button(ui, "Импорт записи", ButtonStyle::Secondary).clicked()
                {
                    self.show_import = true;
                }
            },
        );
        if ui.button("Спросить").clicked() {
            self.state.chat.open(None, String::new());
        }
        if ui
            .selectable_label(self.state.tasks_open, "Задачи")
            .clicked()
        {
            self.people.close();
            self.state.tasks_open = true;
            self.state.refresh_tasks();
        }
        self.people.sidebar(ui, &mut self.state);
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.state.query)
                .id_salt("library_search")
                .hint_text("Поиск…  Ctrl+K")
                .desired_width(f32::INFINITY),
        );
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::K)) {
            search.request_focus();
        }
        if search.changed() {
            self.state.query_changed = Some(Instant::now());
        }
        egui::Panel::bottom("library_actions")
            .frame(egui::Frame::new().inner_margin(4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if theme::button(ui, "Настройки", ButtonStyle::Secondary).clicked() {
                        self.show_settings = true;
                    }
                    if ui.small_button("Обновить").clicked() {
                        self.state.flush();
                        self.state.refresh();
                    }
                });
                ui.small(if self.state.unsaved() {
                    "Сохранение…"
                } else {
                    &self.state.status
                });
                if self.state.exit_requested {
                    ui.small("Завершаем задачи перед выходом…");
                }
            });
        let mut start_event = None;
        let mut brief_event = None;
        if !self.state.settings.calendars.is_empty() {
            egui::CollapsingHeader::new("Ближайшие события")
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("calendar_events")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            let now = chrono::Utc::now();
                            let events: Vec<_> = self
                                .state
                                .calendar_events
                                .iter()
                                .filter(|e| {
                                    e.end >= now && e.start <= now + chrono::Duration::hours(12)
                                })
                                .collect();
                            if events.is_empty() {
                                ui.small("Нет ближайших событий");
                            }
                            for event in events.into_iter().take(30) {
                                ui.label(format!(
                                    "{} · {}",
                                    if event.all_day {
                                        "Весь день".into()
                                    } else {
                                        event
                                            .start
                                            .with_timezone(&chrono::Local)
                                            .format("%H:%M")
                                            .to_string()
                                    },
                                    event.link.title
                                ));
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .add_enabled(
                                            cfg!(windows)
                                                && self.state.ready
                                                && !self.state.recording.busy()
                                                && !self.state.settings_pending
                                                && !self.state.downloading
                                                && !self.state.quickstart.probe_busy
                                                && !self.state.draft_busy,
                                            egui::Button::new("Начать запись"),
                                        )
                                        .clicked()
                                    {
                                        start_event = Some(event.clone());
                                    }
                                    if ui
                                        .add_enabled(
                                            !self.state.draft_busy,
                                            egui::Button::new("Бриф"),
                                        )
                                        .clicked()
                                    {
                                        brief_event = Some(event.clone());
                                    }
                                    if let Some(url) = &event.link.call_url {
                                        ui.hyperlink_to("Открыть звонок", url);
                                    }
                                });
                            }
                            if self.state.calendar_busy {
                                ui.spinner();
                            }
                            ui.small(&self.state.calendar_status);
                        });
                });
        }
        if let Some(event) = brief_event {
            self.state.prepare_brief(event);
        }
        if let Some(event) = start_event {
            self.state.start_event(event);
        }
        let mut select = None;
        let today = chrono::Local::now().date_naive();
        egui::ScrollArea::vertical()
            .id_salt("meetings")
            .show(ui, |ui| {
                let mut previous = None;
                for row in &self.state.rows {
                    let start = row.meta.started_at.with_timezone(&chrono::Local);
                    let date = start.date_naive();
                    if previous != Some(date) {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(day_label(date, today))
                                .size(12.0)
                                .color(p.muted),
                        );
                        previous = Some(date);
                    }
                    let selected = self.state.selected.as_deref() == Some(row.meeting.path());
                    let recording = self.state.recording.phase == Phase::Recording
                        && self.state.recording.path.as_deref() == Some(row.meeting.path());
                    let response = egui::Frame::new()
                        .fill(if selected {
                            p.card
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .corner_radius(8)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width((ui.available_width() - 2.0).max(1.0));
                            ui.add(
                                egui::Label::new(RichText::new(&row.meta.title).strong())
                                    .truncate(),
                            );
                            ui.horizontal(|ui| {
                                if recording {
                                    theme::recording_dot(ui);
                                }
                                let detail = if recording {
                                    "идёт запись".into()
                                } else {
                                    duration_label(
                                        row.meta
                                            .ended_at
                                            .map(|end| (end - row.meta.started_at).num_seconds()),
                                    )
                                };
                                ui.label(
                                    RichText::new(format!("{} · {detail}", start.format("%H:%M")))
                                        .size(12.0)
                                        .color(p.muted),
                                );
                            });
                        })
                        .response;
                    if ui
                        .interact(
                            response.rect,
                            response.id.with("select"),
                            egui::Sense::click(),
                        )
                        .on_hover_text(&row.snippet)
                        .clicked()
                    {
                        select = Some(row.meeting.clone());
                    }
                }
                if self.state.rows.is_empty() && !self.state.listing {
                    ui.weak("Здесь будут ваши встречи");
                }
                if self.state.listing {
                    ui.spinner();
                }
            });
        if let Some(meeting) = select {
            self.people.close();
            self.state.select(meeting);
        }
    }
    fn draft_panels(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        if let Some(event) = self.state.calendar_reminder.clone() {
            egui::Window::new("Скоро встреча")
                .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -16.0])
                .collapsible(false)
                .show(&ctx, |ui| {
                    ui.label(format!(
                        "{} в {}",
                        event.link.title,
                        event.start.with_timezone(&chrono::Local).format("%H:%M")
                    ));
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!self.state.draft_busy, egui::Button::new("Бриф"))
                            .clicked()
                        {
                            self.state.prepare_brief(event.clone());
                            self.state.calendar_reminder = None;
                        }
                        if ui.button("Закрыть").clicked() {
                            self.state.calendar_reminder = None;
                        }
                    });
                });
        }
        if self.state.brief_open {
            egui::Panel::right("premeeting_brief")
                .resizable(true)
                .default_size(380.0)
                .show(root, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Бриф");
                        if ui.button("Закрыть").clicked() {
                            self.state.brief_open = false;
                        }
                    });
                    ui.checkbox(
                        &mut self.state.save_brief,
                        "Добавить в заметки при начале записи",
                    );
                    if self.state.draft_busy {
                        ui.spinner();
                        if ui.button("Отменить").clicked() {
                            self.state.draft_cancel.cancel();
                        }
                    }
                    if let Some(brief) = &self.state.brief {
                        ui.strong(&brief.event.link.title);
                        if brief.mode == Mode::Local {
                            ui.small("Local: новая запись с этим брифом будет Local");
                        }
                        if let Some(warning) = &brief.warning {
                            ui.small(warning);
                        }
                        egui::ScrollArea::vertical()
                            .id_salt("brief_text")
                            .show(ui, |ui| {
                                CommonMarkViewer::new().show(ui, &mut self.cache, &brief.text);
                            });
                    }
                });
        }
        if self.state.followup_open {
            let mut open = true;
            egui::Window::new("Черновик сообщения")
                .open(&mut open)
                .default_size([650.0, 500.0])
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        let previous = self.state.followup_kind;
                        ui.add_enabled_ui(!self.state.draft_busy, |ui| {
                            ui.selectable_value(
                                &mut self.state.followup_kind,
                                notetaker_llm::FollowupKind::Team,
                                "В команду",
                            );
                            ui.selectable_value(
                                &mut self.state.followup_kind,
                                notetaker_llm::FollowupKind::Client,
                                "Клиенту",
                            );
                        });
                        if previous != self.state.followup_kind {
                            self.state.followup_text.clear();
                        }
                        if ui
                            .add_enabled(
                                !self.state.draft_busy && !self.state.unsaved(),
                                egui::Button::new("Создать черновик"),
                            )
                            .clicked()
                        {
                            self.state.prepare_followup();
                        }
                    });
                    if self.state.draft_busy {
                        ui.spinner();
                        if ui.button("Отменить").clicked() {
                            self.state.draft_cancel.cancel();
                        }
                    }
                    ui.add(
                        egui::TextEdit::multiline(&mut self.state.followup_text)
                            .desired_rows(18)
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.state.draft_busy && !self.state.followup_text.is_empty(),
                                egui::Button::new("Копировать"),
                            )
                            .clicked()
                        {
                            ctx.copy_text(self.state.followup_text.clone());
                        }
                        if ui
                            .add_enabled(
                                !self.state.draft_busy
                                    && !self.state.followup_text.trim().is_empty(),
                                egui::Button::new("Сохранить в заметку"),
                            )
                            .clicked()
                        {
                            self.state.save_followup();
                        }
                    });
                });
            self.state.followup_open = open;
        }
    }
    fn recording_bar(&mut self, ui: &mut egui::Ui) {
        let p = Palette::of(ui);
        theme::bar(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let r = &self.state.recording;
                if r.phase == Phase::Recording {
                    theme::recording_dot(ui);
                } else {
                    ui.spinner();
                }
                ui.label(RichText::new(timestamp(r.elapsed().as_millis() as u64)).strong());
                ui.weak(match r.phase {
                    _ if r.importing => "Импорт записи: декодирование и распознавание…",
                    Phase::Starting => "Загружаю модель и открываю аудио…",
                    Phase::Finalizing => "Чистовой транскрипт…",
                    _ if r.live_disabled => "Запись идёт, текст появится после остановки",
                    _ if r.fallback => "Микрофон и весь системный звук пишутся",
                    _ => "Микрофон и звук звонка пишутся",
                });
                if let Some((done, total)) = r.progress {
                    ui.add(
                        egui::ProgressBar::new(done as f32 / total.max(1) as f32)
                            .desired_width(100.0),
                    );
                }
                let can_stop =
                    !r.importing && matches!(r.phase, Phase::Starting | Phase::Recording);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if can_stop && theme::button(ui, "Стоп", ButtonStyle::Danger).clicked() {
                        self.state.toggle_recording();
                    }
                });
                if self.state.recording.phase == Phase::Recording {
                    if ui
                        .add_enabled(
                            !self.state.recording.assist.schedule.busy(),
                            egui::Button::new("Что я пропустил?"),
                        )
                        .clicked()
                    {
                        self.state.request_live(true, true);
                    }
                    if ui.button("Закладка").clicked() {
                        self.state.add_bookmark();
                    }
                    let mut save = false;
                    if let Some((_, _, label)) = &mut self.state.recording.assist.label {
                        let response = ui.add(
                            egui::TextEdit::singleline(label)
                                .hint_text("Метка (необязательно)")
                                .desired_width(160.0),
                        );
                        save = ui.small_button("Готово").clicked()
                            || (response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    }
                    if save {
                        self.state.save_bookmark_label();
                    }
                }
            });
        });
    }
    fn tasks_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Задачи");
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.tasks_all, false, "Открытые");
            ui.selectable_value(&mut self.tasks_all, true, "Все");
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
                (self.tasks_all || !task.completed)
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
                        ui.strong(&title);
                        previous = Some(key);
                    }
                    ui.push_id((&task.path, task.line), |ui| {
                        ui.horizontal_wrapped(|ui| {
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
                                && task
                                    .due
                                    .is_some_and(|date| date < chrono::Local::now().date_naive());
                            let label = RichText::new(&task.text);
                            if ui
                                .link(if overdue {
                                    label.color(egui::Color32::RED)
                                } else {
                                    label
                                })
                                .clicked()
                            {
                                open = Some(task.clone());
                            }
                            ui.small(format!(
                                "{} · {} · {}:{}",
                                task.owner,
                                task.due.map_or_else(
                                    || task.due_text.clone(),
                                    |date| date.format("%d.%m.%Y").to_string()
                                ),
                                task.meeting_title,
                                task.line
                            ));
                        });
                    });
                }
                if self.state.tasks_busy {
                    ui.spinner();
                }
            });
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
            ui.add_space(60.0);
            ui.heading("Место для важных мыслей");
            ui.weak("Начните встречу, пишите заметки. Транскрипт и конспект будут рядом.");
            return;
        };
        let can_enhance = self.state.enhancing.is_none()
            && !self.state.recording.busy()
            && !self.state.unsaved()
            && !self.state.settings_pending;
        let can_cancel_generation = self.state.generation_cancellable();
        let locked =
            self.state.recording.busy() && self.state.recording.path.as_ref() == Some(&path);
        let live = locked && self.state.recording.phase == Phase::Recording;
        let target_line = self
            .state
            .task_target
            .as_ref()
            .filter(|(target, _)| target == &path)
            .map(|(_, line)| *line);
        let global_allowed = !self.state.unsaved();
        let speaker_busy = self.state.documents.values().any(|doc| doc.speaker_ui.busy);
        let Some(document) = self.state.documents.get_mut(&path) else {
            ui.spinner();
            return;
        };
        ui.heading(RichText::new(&document.data.meta.title).strong());
        let series_action = self.people.header(
            ui,
            &document.data.meta,
            !locked && !document.saving && !document.dirty.any() && can_enhance,
        );
        if speaker_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Сохраняем спикеров…");
            });
        }
        if let Some(event) = &document.data.meta.calendar_event {
            ui.horizontal_wrapped(|ui| {
                for attendee in &event.attendees {
                    if ui.link(attendee.label()).clicked() {
                        crate::people::request(
                            ui,
                            crate::people::Request {
                                name: attendee.name.clone(),
                                email: attendee.email.clone(),
                                voice: None,
                            },
                        );
                    }
                }
                if let Some(url) = &event.call_url {
                    ui.hyperlink_to("Открыть звонок", url);
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            let start = document.data.meta.started_at.with_timezone(&chrono::Local);
            let range = match document.data.meta.ended_at {
                Some(end) => format!(
                    "{}-{}",
                    start.format("%d.%m, %H:%M"),
                    end.with_timezone(&chrono::Local).format("%H:%M")
                ),
                None => start.format("%d.%m, %H:%M").to_string(),
            };
            theme::chip(ui, &range);
            if let Some(app) = &document.data.meta.app {
                theme::chip(ui, app);
            }
            theme::chip(
                ui,
                match document.data.meta.mode {
                    Mode::Local => "Local",
                    Mode::Hybrid => "Hybrid",
                },
            );
            theme::chip(ui, &format!("Шаблон: {}", document.template));
            if !locked && !document.data.transcript.segments.is_empty() {
                theme::chip(ui, "Транскрипт готов");
            }
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
        });
        let mut update_summary = false;
        if document.summary_stale() {
            ui.horizontal(|ui| {
                theme::chip(ui, "Конспект устарел");
                let edited = document
                    .data
                    .meta
                    .summary
                    .as_ref()
                    .is_some_and(|stamp| stamp.edited(&document.data.enhanced));
                update_summary = ui
                    .add_enabled(
                        can_enhance,
                        egui::Button::new(if edited {
                            "Пересоздать"
                        } else {
                            "Обновить"
                        }),
                    )
                    .clicked();
            });
        }
        ui.add_space(12.0);
        if document.generation.is_some() {
            theme::bar(Palette::of(ui)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Пишу конспект…");
                    if can_cancel_generation && ui.button("Отменить").clicked() {
                        self.state.generation_cancel.cancel();
                    }
                });
            });
        }
        let mut images = Vec::new();
        if live {
            let mut update_live = false;
            let height = ui.available_height();
            ui.columns(2, |columns| {
                columns[0].set_height(height);
                columns[1].set_height(height);
                columns[0].weak("МОИ ЗАМЕТКИ");
                images = notes_editor(&mut columns[0], document, &path, &mut self.image_viewer);
                columns[1].horizontal(|ui| {
                    ui.selectable_value(&mut self.live_summary_tab, false, "Транскрипт");
                    ui.selectable_value(&mut self.live_summary_tab, true, "Живой конспект");
                });
                if self.live_summary_tab {
                    let assist = &self.state.recording.assist;
                    update_live = columns[1].add_enabled(!assist.schedule.busy(), egui::Button::new("Обновить сейчас")).clicked();
                    if let Some(updated) = assist.updated {
                        columns[1].weak(format!("обновлено {} мин назад", updated.elapsed().as_secs() / 60));
                    }
                    if assist.schedule.busy() { columns[1].spinner(); }
                    if let Some(error) = &assist.error { columns[1].label(error); }
                    if assist.summary.is_empty() {
                        columns[1].weak("Конспект появится после новых строк транскрипта. Нужен настроенный провайдер.");
                    }
                    egui::ScrollArea::vertical().id_salt("live_summary").show(&mut columns[1], |ui| {
                        CommonMarkViewer::new().show(ui, &mut self.cache, &assist.summary);
                    });
                } else {
                    transcript_view(&mut columns[1], document, &path, true, None, false, false);
                }
            });
            if let Some(crate::people::HeaderAction::Open(meeting)) = series_action {
                self.state.select(meeting);
            }
            if update_live {
                self.state.request_live(false, true);
            }
            self.state.attach(images);
            return;
        }
        let mut enhance = update_summary;
        let mut export = None;
        let mut speaker_edit = None;
        let speaker_editable = !locked
            && self.state.recording.assist.bookmarks_pending == 0
            && !document.saving
            && !speaker_busy
            && document.attachments_pending == 0
            && self.state.enhancing.is_none()
            && !self.state.settings_pending;
        let mut ask = None;
        let mut followup = false;
        ui.add_enabled_ui(!locked && !speaker_busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.menu_button("Экспорт", |ui| {
                    if ui
                        .add_enabled(
                            !document.data.enhanced.is_empty(),
                            egui::Button::new("Конспект Markdown"),
                        )
                        .clicked()
                    {
                        export = Some(Command::Export {
                            meeting: document.data.meeting.clone(),
                            text: document.data.enhanced.clone(),
                        });
                        ui.close();
                    }
                    for (format, label) in [
                        (notetaker_core::ExportFormat::Md, "Транскрипт Markdown"),
                        (notetaker_core::ExportFormat::Srt, "Субтитры SRT"),
                        (notetaker_core::ExportFormat::Vtt, "Субтитры VTT"),
                        (notetaker_core::ExportFormat::Csv, "Реплики CSV"),
                        (notetaker_core::ExportFormat::Json, "Журнал таймингов JSON"),
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
                    ui.weak("Файл в папке встречи обновляется при повторном экспорте");
                });
                if ui
                    .add_enabled(
                        !self.state.chat.busy,
                        egui::Button::new("Спросить про эту встречу"),
                    )
                    .clicked()
                {
                    ask = Some((
                        document.data.meeting.clone(),
                        document.data.meta.title.clone(),
                    ));
                }
                if ui
                    .add_enabled(
                        can_enhance
                            && !self.state.draft_busy
                            && !document.data.enhanced.trim().is_empty(),
                        egui::Button::new("Черновик сообщения"),
                    )
                    .clicked()
                {
                    followup = true;
                }
            });
        });
        ui.horizontal(|ui| {
            theme::segmented(
                ui,
                &mut self.state.tab,
                &[
                    (Tab::Enhanced, "Конспект"),
                    (Tab::Notes, "Мои заметки"),
                    (Tab::Transcript, "Транскрипт"),
                ],
            );
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match self.state.tab {
                    Tab::Notes => {
                        ui.checkbox(&mut self.notes_preview, "Просмотр Markdown");
                    }
                    Tab::Enhanced => {
                        ui.checkbox(&mut self.enhanced_preview, "Просмотр Markdown");
                    }
                    Tab::Transcript => {}
                },
            );
        });
        if self.state.tab == Tab::Enhanced {
            egui::Panel::bottom("summary_actions")
                .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 10)))
                .show(ui, |ui| {
                    theme::bar(Palette::of(ui)).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_enabled_ui(document.generation.is_none(), |ui| {
                                egui::ComboBox::from_id_salt("template")
                                    .selected_text(&document.template)
                                    .show_ui(ui, |ui| {
                                        for name in &self.state.templates {
                                            ui.selectable_value(
                                                &mut document.template,
                                                name.clone(),
                                                name,
                                            );
                                        }
                                    });
                            });
                            ui.add_enabled_ui(can_enhance, |ui| {
                                enhance |= theme::button(
                                    ui,
                                    if document.data.enhanced.is_empty()
                                        && document.data.meta.summary.is_none()
                                    {
                                        "Создать конспект"
                                    } else {
                                        "Пересоздать"
                                    },
                                    ButtonStyle::Secondary,
                                )
                                .clicked();
                            });
                            if theme::button(ui, "Копировать", ButtonStyle::Secondary).clicked()
                            {
                                ui.ctx().copy_text(
                                    document
                                        .generation
                                        .as_ref()
                                        .unwrap_or(&document.data.enhanced)
                                        .clone(),
                                );
                            }
                            ui.add_enabled_ui(document.generation.is_none(), |ui| {
                                if theme::button(ui, "Показать в папке", ButtonStyle::Secondary)
                                    .clicked()
                                {
                                    export = Some(Command::Reveal(path.join("enhanced.md")));
                                }
                            });
                        });
                    });
                });
        }
        ui.add_enabled_ui(!speaker_busy, |ui| {
        match self.state.tab {
            Tab::Notes => {
                if self.notes_preview {
                    egui::ScrollArea::vertical()
                        .id_salt(("notes_preview", &path))
                        .show(ui, |ui| {
                            markdown_preview(
                                ui,
                                &mut self.cache,
                                &document.data.notes,
                                &path,
                                &mut self.image_viewer,
                            );
                        });
                } else {
                    images = notes_editor(ui, document, &path, &mut self.image_viewer);
                }
            }
            Tab::Enhanced => {
                egui::ScrollArea::vertical()
                    .id_salt(("enhanced", &path))
                    .show(ui, |ui| {
                        if let Some(text) = &document.generation {
                            markdown_preview(
                                ui,
                                &mut self.cache,
                                text,
                                &path,
                                &mut self.image_viewer,
                            );
                        } else if document.data.enhanced.is_empty() && self.enhanced_preview {
                            ui.weak(
                                if document.data.meta.summary.is_some() { "Конспект очищен вручную. Нажмите «Пересоздать», чтобы создать его заново." }
                                else { "Выберите шаблон и нажмите «Создать конспект», чтобы собрать конспект." },
                            );
                        } else if self.enhanced_preview {
                            markdown_preview(
                                ui,
                                &mut self.cache,
                                &document.data.enhanced,
                                &path,
                                &mut self.image_viewer,
                            );
                        } else {
                            let id = egui::Id::unique(("enhanced_edit", &path));
                            let cursor = target_line.map(|line|document.data.enhanced.split_inclusive('\n').take(line.saturating_sub(1)).map(|line|line.chars().count()).sum::<usize>());
                            if let Some(cursor) = cursor {
                                let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                                state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(cursor))));
                                state.store(ui.ctx(), id);
                            }
                            let output = egui::TextEdit::multiline(&mut document.data.enhanced).id(id).frame(egui::Frame::NONE).desired_width(f32::INFINITY).desired_rows(24).show(ui);
                            if let Some(cursor) = cursor {
                                output.response.request_focus();
                                ui.scroll_to_rect(output.galley.pos_from_cursor(egui::text::CCursor::new(cursor)).translate(output.galley_pos.to_vec2()), Some(egui::Align::Center));
                                self.state.task_target = None;
                            }
                            if output.response.changed() { document.dirty.enhanced = true; document.changed(); }
                        }
                    });
            }
            Tab::Transcript => {
                speaker_edit = transcript_view(
                    ui, document, &path, locked, self.state.settings.voice_library.then_some(&self.state.voices),
                    speaker_editable, global_allowed,
                );
            }
        }
        });
        if let Some((meeting, title)) = ask {
            self.state.chat.open(Some(meeting), title);
        }
        if followup {
            self.state.brief_open = false;
            self.state.followup_open = true;
            if self.state.followup_path.as_ref() != Some(&path) {
                self.state.followup_text.clear();
            }
            self.state.followup_path = Some(path.clone());
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
            self.state.enhance();
        }
        if let Some(command) = export {
            self.state.send(command);
        }
    }
    fn notifications(&mut self, ctx: &egui::Context) {
        let mut dismissed = false;
        if let Some(text) = &self.state.recording.assist.recap {
            egui::Window::new("Что я пропустил?")
                .id(egui::Id::unique("recap-card"))
                .resizable(true)
                .default_width(380.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            ui.label(text);
                        });
                    dismissed = ui.button("Закрыть").clicked();
                });
        }
        if dismissed {
            self.state.recording.assist.recap = None;
        }
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
        egui::Window::new("Новая запись").open(&mut open).collapsible(false).show(ctx, |ui| {
            ui.label("Название"); ui.text_edit_singleline(&mut self.state.title);
            model_selector(ui, "record_model", &mut self.state.record_model);
            mode_selector(ui, &mut self.state.record_mode);
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
            #[cfg(not(windows))]
            ui.label("Захват звука пока реализован только на Windows. Библиотека, поиск, правки и конспекты работают на этой ОС.");
            ui.horizontal(|ui| {
                if ui.add_enabled(cfg!(windows) && !self.state.recording.busy(), egui::Button::new("Начать запись")).clicked() { self.state.toggle_recording(); self.show_record = false; }
                if ui.button("Скачать модель…").clicked() { self.show_settings = true; }
            });
        });
        self.show_record &= open;
    }
    fn import_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_import;
        egui::Window::new("Импорт записи")
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Путь к файлу аудио или видео (можно перетащить файл в это окно)");
                ui.add(egui::TextEdit::singleline(&mut self.import_path).desired_width(480.0));
                ui.small("wav, mp3, m4a, ogg, opus, flac, mp4, webm, mkv");
                model_selector(ui, "import_model", &mut self.state.record_model);
                mode_selector(ui, &mut self.state.record_mode);
                if ui
                    .add_enabled(
                        !self.state.recording.busy() && !self.import_path.trim().is_empty(),
                        egui::Button::new("Импортировать"),
                    )
                    .clicked()
                {
                    self.state.import_recording(self.import_path.trim().into());
                    self.show_import = false;
                }
            });
        self.show_import &= open;
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        let mut quickstart = false;
        egui::Window::new("Настройки").open(&mut open).default_width(640.0).default_height(600.0).resizable(true).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if ui.button("Быстрый старт").clicked() { self.quickstart.open = true; quickstart = true; }
                let settings = &mut self.state.settings_draft;
                self.maintenance.settings(ui, settings);
                ui.horizontal(|ui| {
                    ui.label("Тема:");
                    egui::ComboBox::from_id_salt("theme").selected_text(settings.theme.label()).show_ui(ui, |ui| {
                        for theme in [crate::theme::Theme::System, crate::theme::Theme::Light, crate::theme::Theme::Dark] {
                            ui.selectable_value(&mut settings.theme, theme, theme.label());
                        }
                    });
                });
                ui.label("Как вас зовут");
                crate::quickstart::identity_control(ui, settings);
                ui.label("Библиотека (полный путь)");
                let mut library = notetaker_core::display_path(&settings.library);
                if ui.text_edit_singleline(&mut library).changed() { settings.library = library.into(); }
                model_selector(ui, "default_model", &mut settings.model);
                egui::ComboBox::from_id_salt("asr_acceleration").selected_text(match settings.asr_acceleration {
                    notetaker_asr_proto::Provider::Auto => "Ускорение: Авто",
                    notetaker_asr_proto::Provider::Cpu => "Ускорение: Только CPU",
                    _ => "Ускорение: GPU (DirectML)",
                }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut settings.asr_acceleration, notetaker_asr_proto::Provider::Auto, "Авто");
                    ui.selectable_value(&mut settings.asr_acceleration, notetaker_asr_proto::Provider::Cpu, "Только CPU");
                    ui.add_enabled_ui(false, |ui| {
                        ui.selectable_value(&mut settings.asr_acceleration, notetaker_asr_proto::Provider::Directml, "GPU (DirectML) - недоступно в этой сборке");
                    });
                });
                ui.small("GPU (DirectML): недоступно в этой сборке. Авто использует CPU без дополнительной загрузки.");
                ui.horizontal(|ui| {
                    ui.label("Потоков ASR (0 - авто)");
                    ui.add(egui::DragValue::new(&mut settings.asr_threads).range(0..=8));
                });
                ui.small("Авто: до 8 физических ядер. Применяется также к VAD и разделению собеседников.");
                ui.checkbox(&mut settings.diarize, "Разделять собеседников");
                egui::ComboBox::from_id_salt("speakers").selected_text(format!("Участников: {}", settings.speakers.map_or("авто".into(), |n| n.to_string()))).show_ui(ui, |ui| {
                    ui.selectable_value(&mut settings.speakers, None, "авто");
                    for n in 1..=32 { ui.selectable_value(&mut settings.speakers, Some(n), n.to_string()); }
                });
                ui.checkbox(&mut settings.digest_reminder, "Напоминать о дайджесте в пятницу в 17:00");
                ui.checkbox(&mut settings.voice_library, "Голоса: запоминать имена");
                ui.small("Голосовые отпечатки - биометрические данные; хранятся только локально. Применяется к новым встречам.");
                ui.label("Словарь (один термин на строку)");
                ui.add(egui::TextEdit::multiline(&mut settings.vocabulary).desired_rows(4));
                ui.small("Алиасы: werf = верф, вёрф. Дополнения библиотеки: .notetaker/vocabulary.txt");
                mode_selector(ui, &mut settings.mode);
                ui.horizontal(|ui| {
                    let mut forever = settings.retention_days.is_none();
                    if ui.checkbox(&mut forever, "Хранить аудио навсегда").changed() { settings.retention_days = if forever { None } else { Some(7) }; }
                    if let Some(days) = &mut settings.retention_days { ui.add(egui::DragValue::new(days).range(0..=36_500).suffix(" дней")); }
                });
                ui.small("0 - удалить аудио после успешного чистового прохода. Срок применяется к новым записям; просроченное аудио очищается после записи.");
                ui.label("Глобальная клавиша старт/стоп (Windows)"); ui.text_edit_singleline(&mut settings.hotkey);
                ui.label("Что я пропустил? (Windows)"); ui.text_edit_singleline(&mut settings.recap_hotkey);
                ui.label("Закладка (Windows)"); ui.text_edit_singleline(&mut settings.bookmark_hotkey);
                let summary_available = self.state.llm_configured && self.state.llm.validate().ok()
                    .and_then(|text| notetaker_llm::Config::parse(&text).ok())
                    .is_some_and(|config| config.select_scenario(settings.mode, notetaker_llm::Scenario::Summary, None).is_ok());
                let mut live_enabled = settings.live_summary && summary_available;
                if ui.add_enabled(summary_available, egui::Checkbox::new(&mut live_enabled, "Живой конспект")).changed() {
                    settings.live_summary = live_enabled;
                }
                ui.small("Работает только с настроенным провайдером конспекта. Local разрешает только локальный провайдер.");
                ui.horizontal(|ui| {
                    ui.label("Обновлять каждые");
                    ui.add(egui::DragValue::new(&mut settings.live_summary_minutes).range(2..=10).suffix(" мин"));
                });
                ui.horizontal(|ui| {
                    ui.label("Что я пропустил: последние");
                    ui.add(egui::DragValue::new(&mut settings.recap_minutes).range(1..=30).suffix(" мин"));
                });
                ui.small("Например Ctrl+Alt+R. Модификаторы: Ctrl, Alt, Shift, Super; клавиша: A–Z, 0–9, F1–F24.");
                ui.separator();
                ui.add_enabled_ui(cfg!(windows), |ui| {
                    ui.checkbox(&mut settings.detect_calls, "Предлагать запись при обнаружении звонка");
                    ui.checkbox(&mut settings.detect_any_microphone, "Предлагать для любых приложений с микрофоном");
                    ui.checkbox(&mut settings.detect_browsers, "Учитывать браузеры")
                        .on_hover_text("Включите для Meet, Толка и Телемоста в браузере. Диктовка и голосовые сообщения тоже могут выглядеть как звонок.");
                    ui.label("Свои приложения для звонков");
                    ui.add(egui::TextEdit::multiline(&mut settings.call_apps).desired_rows(3).hint_text("Telemost.exe\nKonturTalk.exe"));
                    ui.small("Имена exe из Диспетчера задач, по одному на строку. Дополняют список известных приложений.");
                });
                ui.horizontal(|ui| {
                    ui.label("Автостоп по тишине:");
                    ui.add(egui::DragValue::new(&mut settings.auto_stop_minutes).range(0..=u32::MAX).suffix(" мин"));
                });
                ui.small("0 - выключено. Завершение звонка само по себе не останавливает запись.");
                ui.separator(); ui.heading("Модели ASR");
                crate::quickstart::model_control(ui, &mut self.state, "gigaam-v3", "GigaAM v3 · русский");
                crate::quickstart::model_control(ui, &mut self.state, "parakeet-v3", "Parakeet v3 · многоязычный");
                crate::quickstart::model_control(ui, &mut self.state, "auto", "Авто (рус/англ) · обе ASR-модели + Whisper tiny");
                crate::quickstart::model_control(ui, &mut self.state, "diarization", "Модели разделения собеседников (47 МБ)");
                ui.checkbox(&mut self.state.settings_draft.auto_download_models, "Скачивать модели автоматически");
                ui.checkbox(&mut self.state.settings_draft.hide_quickstart, "Больше не показывать быстрый старт при запуске");
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
                ui.checkbox(&mut self.state.settings_draft.auto_summary, "Делать конспект автоматически");
                ui.separator(); ui.heading("Календари");
                ui.small("Приватные ICS-подписки. URL хранится в Credential Manager. Обновление при запуске и каждые 10 минут.");
                ui.horizontal(|ui| { ui.label("Часовой пояс без TZID:"); ui.text_edit_singleline(&mut self.state.settings_draft.calendar_timezone); });
                #[cfg(not(windows))]
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
                    ui.add_enabled(cfg!(windows), egui::Link::new("Подключить календарь")).clicked()
                } else { ui.add_enabled(cfg!(windows), egui::Button::new("Добавить календарь")).clicked() };
                if connect {
                    self.state.settings_draft.calendars.push(notetaker_llm::calendar::Subscription { id: format!("calendar-{}", uuid::Uuid::now_v7()), name: "Календарь".into() });
                }
                if ui.add_enabled(!self.state.calendar_busy, egui::Button::new("Обновить календари")).clicked() { self.state.refresh_calendar(); }
                ui.small(&self.state.calendar_status);
                ui.checkbox(&mut self.state.settings_draft.mcp_exclude_local, "MCP: не отдавать Local-встречи");
                ui.separator(); ui.heading("Подключение AI");
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
                    ui.push_id(index, |ui| { ui.group(|ui| {
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
                if ui.add_enabled(can_save, egui::Button::new("Сохранить настройки")).clicked() { self.state.save_settings(); }
                if self.state.recording.busy() || self.state.enhancing.is_some() { ui.small("Сохранение настроек доступно после завершения записи и конспекта."); }
                ui.small("Настройки приложения и LLM сохраняются отдельно. Ключи шлюзов хранятся в Credential Manager на Windows; также поддерживается окружение.");
                #[cfg(not(windows))]
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
        let title = self
            .state
            .recording
            .path
            .as_ref()
            .and_then(|path| self.state.documents.get(path))
            .map_or("Встреча", |doc| doc.data.meta.title.as_str());
        let mut stop = false;
        let mut open = false;
        let mut recap = false;
        let mut bookmark = false;
        let mut dismiss = false;
        let mut save_label = false;
        let expanded = self.state.recording.assist.recap.is_some()
            || self.state.recording.assist.schedule.busy()
            || self.state.recording.assist.error.is_some()
            || self.state.recording.assist.label.is_some();
        let height = if expanded { 290.0 } else { 52.0 };
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("recording-indicator"),
            egui::ViewportBuilder::default()
                .with_title("notetaker · запись")
                .with_inner_size([710.0, height])
                .with_min_inner_size([710.0, height])
                .with_max_inner_size([710.0, height])
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
                    .corner_radius(26)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_width(690.0);
                        ui.horizontal(|ui| {
                            theme::recording_dot(ui);
                            ui.label(
                                RichText::new(timestamp(
                                    self.state.recording.elapsed().as_millis() as u64,
                                ))
                                .strong(),
                            );
                            ui.add_sized(
                                [145.0, 28.0],
                                egui::Label::new(RichText::new(title).color(p.muted)).truncate(),
                            );
                            recap = ui
                                .add_enabled(
                                    !self.state.recording.assist.schedule.busy(),
                                    egui::Button::new("Что пропустил?"),
                                )
                                .clicked();
                            bookmark = ui.button("Закладка").clicked();
                            open = theme::button(ui, "Открыть", ButtonStyle::Secondary).clicked();
                            stop = theme::button(ui, "Стоп", ButtonStyle::Danger).clicked();
                        });
                        if let Some((_, _, label)) = &mut self.state.recording.assist.label {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(label)
                                        .hint_text("Метка (необязательно)"),
                                );
                                save_label = ui.button("Готово").clicked();
                            });
                        }
                        if self.state.recording.assist.schedule.busy() {
                            ui.label("Готовлю ответ…");
                        }
                        if let Some(error) = &self.state.recording.assist.error {
                            ui.label(error);
                        }
                        if let Some(text) = &self.state.recording.assist.recap {
                            egui::ScrollArea::vertical()
                                .max_height(150.0)
                                .show(ui, |ui| {
                                    ui.label(text);
                                });
                            dismiss = ui.button("Закрыть ответ").clicked();
                        }
                    });
                if ui.input(|input| input.viewport().close_requested()) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                ui.ctx().request_repaint_after(Duration::from_millis(200));
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
        if recap {
            self.state.request_live(true, true);
        }
        if bookmark {
            self.state.add_bookmark();
        }
        if dismiss {
            self.state.recording.assist.recap = None;
        }
        if save_label {
            self.state.save_bookmark_label();
        }
    }
    #[cfg(windows)]
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
            let signature = format!(
                "{}|{}|{}",
                settings.hotkey, settings.recap_hotkey, settings.bookmark_hotkey
            );
            if self.applied_hotkey != signature {
                self.applied_hotkey = signature;
                if let Err(error) = desktop.set_hotkeys([
                    &settings.hotkey,
                    &settings.recap_hotkey,
                    &settings.bookmark_hotkey,
                ]) {
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
                    Action::Recap => self.state.request_live(true, true),
                    Action::Bookmark => self.state.add_bookmark(),
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
        self.state.poll();
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
        #[cfg(windows)]
        self.desktop(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.state.can_exit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            #[cfg(windows)]
            if self.desktop.is_some() && !self.state.exit_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.state.exit();
            }
            #[cfg(not(windows))]
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
        if self.show_import {
            if let Some(file) = dropped.first() {
                self.import_path = notetaker_core::display_path(file.path());
            }
        } else {
            self.state.attach(
                dropped
                    .into_iter()
                    .map(|file| ImageInput::File(file.path().to_owned()))
                    .collect(),
            );
        }
        let p = Palette::of(ui);
        egui::Panel::left("library")
            .resizable(true)
            .default_size(255.0)
            .size_range(210.0..=400.0)
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(16))
            .show(ui, |ui| self.meetings(ui));
        if self.state.recording.busy() {
            egui::Panel::bottom("recording_bar")
                .frame(egui::Frame::new().fill(p.bg).inner_margin(16))
                .show(ui, |ui| self.recording_bar(ui));
        }
        self.people.poll(&ctx, &mut self.state);
        self.draft_panels(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(24))
            .show(ui, |ui| {
                ui.add_enabled_ui(!self.state.settings_pending, |ui| {
                    if self.people.active() {
                        self.people.screen(ui, &mut self.state);
                    } else if self.state.tasks_open {
                        self.tasks_screen(ui);
                    } else {
                        self.meeting(ui);
                    }
                });
            });
        if let Some(uri) = self.image_viewer.clone() {
            let mut open = true;
            egui::Window::new("Изображение")
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
        let can_ask = self.state.ready
            && !self.state.settings_pending
            && !self.state.recording.busy()
            && !self.state.unsaved();
        if self.state.chat.open {
            match self.state.chat.show(&ctx, can_ask) {
                Some(crate::chat::Action::Ask) => self.state.ask(),
                Some(crate::chat::Action::Open(meeting)) => {
                    self.people.close();
                    self.state.select(meeting);
                }
                None => {}
            }
        }
        self.notifications(&ctx);
        if self.show_record {
            self.recording_dialog(&ctx);
        }
        if self.show_import {
            self.import_dialog(&ctx);
        }
        if self.show_settings {
            self.settings_dialog(&ctx);
        }
        self.indicator(&ctx);
        self.call_notification(&ctx);
        if self
            .quickstart
            .show(&ctx, &mut self.state, &mut self.maintenance)
        {
            self.show_settings = true;
        }
    }
}
fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Local => "Local (только локальные модели)",
        Mode::Hybrid => "Hybrid (разрешён облачный LLM)",
    }
}
fn mode_selector(ui: &mut egui::Ui, mode: &mut Mode) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Режим:");
        ui.selectable_value(mode, Mode::Local, "Local");
        ui.selectable_value(mode, Mode::Hybrid, "Hybrid");
    });
    ui.small(mode_label(*mode));
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
fn duration_label(seconds: Option<i64>) -> String {
    seconds.map_or_else(
        || "без длительности".into(),
        |s| format!("{} мин", s.max(0) / 60),
    )
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
            if let Some(cursor) = document.inserted_cursor.take() {
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
                .hint_text("Пишите важное здесь… Вставьте скриншот или перетащите PNG/JPEG")
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .desired_rows(24)
                .show(ui);
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
    if !document.data.transcript.bookmarks.is_empty() {
        egui::CollapsingHeader::new("Важные моменты")
            .default_open(true)
            .show(ui, |ui| {
                let mut changed = false;
                let mut deleted = None;
                let mut jump_to = None;
                for bookmark in &mut document.data.transcript.bookmarks {
                    ui.horizontal(|ui| {
                        if ui
                            .link(format!("⚑ {}", timestamp(bookmark.at_ms)))
                            .clicked()
                        {
                            jump_to = Some(bookmark.at_ms);
                        }
                        if !live && editable {
                            let mut label = bookmark.label.clone().unwrap_or_default();
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut label)
                                        .id_salt(bookmark.id)
                                        .hint_text("Важный момент")
                                        .desired_width(180.0),
                                )
                                .changed()
                            {
                                bookmark.label = (!label.trim().is_empty()).then_some(label);
                                changed = true;
                            }
                            if ui.small_button("Удалить").clicked() {
                                deleted = Some(bookmark.id);
                            }
                        } else {
                            ui.label(bookmark.label.as_deref().unwrap_or("Важный момент"));
                        }
                    });
                }
                if let Some(at_ms) = jump_to {
                    document.speaker_ui.scroll_to =
                        document.data.transcript.segment_at(at_ms).map(|s| s.id);
                }
                if let Some(id) = deleted {
                    document.data.transcript.bookmarks.retain(|b| b.id != id);
                    changed = true;
                }
                if changed {
                    document.dirty.transcript = true;
                    document.changed();
                }
            });
    }
    let marker_targets: std::collections::HashMap<_, _> = document
        .data
        .transcript
        .bookmarks
        .iter()
        .filter_map(|b| {
            document
                .data
                .transcript
                .segment_at(b.at_ms)
                .map(|s| (b.id, s.id))
        })
        .collect();
    let mut selected = None;
    let mut scrolled = false;
    egui::ScrollArea::vertical()
        .id_salt(("transcript", path))
        .stick_to_bottom(live)
        .show(ui, |ui| {
            for (index, segment) in document.data.transcript.segments.iter().enumerate() {
                for bookmark in document
                    .data
                    .transcript
                    .bookmarks
                    .iter()
                    .filter(|b| marker_targets.get(&b.id) == Some(&segment.id))
                {
                    if ui
                        .link(format!(
                            "⚑ {} {}",
                            timestamp(bookmark.at_ms),
                            bookmark.label.as_deref().unwrap_or("Важный момент")
                        ))
                        .clicked()
                    {
                        document.speaker_ui.scroll_to = Some(segment.id);
                    }
                }
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
    #[test]
    fn formats_duration_without_wrapping_at_one_hour() {
        assert_eq!(duration_label(None), "без длительности");
        assert_eq!(duration_label(Some(59)), "0 мин");
        assert_eq!(duration_label(Some(3600)), "60 мин");
        assert_eq!(duration_label(Some(-1)), "0 мин");
    }
}
