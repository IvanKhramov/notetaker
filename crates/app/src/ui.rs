use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use eframe::egui::{self, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use notetaker_core::Mode;

use crate::{
    attachments::ImageInput,
    config::{DocumentAction, MODELS},
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
    retry_settings: bool,
    level: ToastLevel,
    count: u64,
    focused_for: Duration,
    last_tick: Instant,
    was_focused: bool,
}

/// Below this logical width the sidebar hides behind a "Меню" button.
const NARROW_WIDTH: f32 = 760.0;

enum MeetingEntry {
    Header(String),
    Upcoming(usize, String),
    Meeting(usize, String),
}
enum TaskEntry {
    Header(String),
    Task(usize),
}
#[derive(Default)]
struct TasksCache {
    key: Option<u64>,
    owners: Vec<String>,
    entries: Vec<TaskEntry>,
}
#[derive(Default)]
struct MeetingsCache {
    key: Option<u64>,
    entries: Vec<MeetingEntry>,
    count: usize,
    layout: crate::virtual_rows::VirtualRows,
}
impl MeetingsCache {
    fn sync(&mut self, state: &AppState, organization: &crate::organization::Panel) {
        let now = chrono::Utc::now();
        let key = crate::virtual_rows::hash((
            state.view_revision,
            &state.query,
            &state.folder_filter,
            &state.tag_filter,
            now.timestamp() / 60,
        ));
        if self.key == Some(key) {
            return;
        }
        self.entries.clear();
        self.count = 0;
        if state.query.is_empty() && state.folder_filter.is_none() {
            let mut events: Vec<_> = state
                .calendar_events
                .iter()
                .enumerate()
                .filter(|(_, e)| e.end >= now && e.start <= now + chrono::Duration::hours(12))
                .collect();
            events.sort_by_key(|(_, e)| e.start);
            if !events.is_empty() {
                self.entries.push(MeetingEntry::Header("Скоро".into()));
            }
            for (index, event) in events.into_iter().take(30) {
                let time = if event.all_day {
                    "Весь день".into()
                } else {
                    event
                        .start
                        .with_timezone(&chrono::Local)
                        .format("%H:%M")
                        .to_string()
                };
                self.entries.push(MeetingEntry::Upcoming(index, time));
            }
        }
        let today = chrono::Local::now().date_naive();
        let mut previous = None;
        for (index, row) in state.rows.iter().enumerate() {
            if !organization.visible(&row.meta, state) {
                continue;
            }
            let start = row.meta.started_at.with_timezone(&chrono::Local);
            let date = start.date_naive();
            if previous != Some(date) {
                self.entries
                    .push(MeetingEntry::Header(day_label(date, today)));
                previous = Some(date);
            }
            self.entries.push(MeetingEntry::Meeting(
                index,
                start.format("%H:%M").to_string(),
            ));
            self.count += 1;
        }
        self.layout.sync(key, self.entries.len(), 0.0, 1.0, 43.0);
        for (index, entry) in self.entries.iter().enumerate() {
            self.layout.set_height(
                index,
                if matches!(entry, MeetingEntry::Header(_)) {
                    49.0
                } else {
                    43.0
                },
            );
        }
        self.key = Some(key);
    }
}

pub struct NotetakerApp {
    state: AppState,
    people: crate::people::Panel,
    organization: crate::organization::Panel,
    trackers: crate::trackers::Panel,
    cache: CommonMarkCache,
    transcript_cache: TranscriptCache,
    document_preview: DocumentPreview,
    notes_cache: NotesCache,
    meetings_cache: MeetingsCache,
    tasks_cache: TasksCache,
    meeting_tasks: Option<(u64, std::path::PathBuf, Vec<usize>)>,
    image_viewer: Option<String>,
    /// Asks before a regenerated summary replaces one the user edited by hand.
    confirm_regenerate: bool,
    /// New title being typed in the rename dialog.
    rename: Option<String>,
    /// The rename field takes focus once, when its dialog opens.
    rename_focus: bool,
    confirm_delete: bool,
    import_picker: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
    enhanced_preview: bool,
    show_record: bool,
    show_settings: bool,
    settings_panel: crate::settings::Panel,
    quickstart: crate::quickstart::Panel,
    tasks_all: bool,
    tasks_mine: bool,
    tasks_person: String,
    tasks_by_meeting: bool,
    toasts: VecDeque<Toast>,
    toast_anchor: Option<egui::Pos2>,
    main_rect: Option<egui::Rect>,
    /// Where the recording pill is now, and where the next one opens.
    indicator: IndicatorPosition,
    home: bool,
    applied_zoom: Option<f32>,
    chat_page: bool,
    ask_text: String,
    focus_search: bool,
    /// Narrow windows only: the navigation is shown on demand.
    sidebar_open: bool,
    maintenance: crate::maintenance::Maintenance,
    #[cfg(any(windows, target_os = "macos"))]
    desktop: Option<crate::desktop::Desktop>,
    #[cfg(any(windows, target_os = "macos"))]
    desktop_attempted: bool,
    #[cfg(any(windows, target_os = "macos"))]
    applied_hotkey: String,
}
fn action_icon(item: DocumentAction) -> &'static str {
    match item {
        DocumentAction::EditNotes | DocumentAction::Rename => theme::icons::PENCIL,
        DocumentAction::EditSummary => theme::icons::FILE_PEN,
        DocumentAction::Read => theme::icons::BOOK_OPEN,
        DocumentAction::Regenerate => theme::icons::REFRESH_CW,
        DocumentAction::Copy => theme::icons::COPY,
        DocumentAction::CopyAll => theme::icons::FILES,
        DocumentAction::Reveal => theme::icons::FOLDER_OPEN,
        DocumentAction::SendTasks => theme::icons::SEND,
        DocumentAction::ExportSummary => theme::icons::DOWNLOAD,
        DocumentAction::Delete => theme::icons::TRASH_2,
    }
}
impl NotetakerApp {
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            people: crate::people::Panel::default(),
            organization: Default::default(),
            trackers: Default::default(),
            cache: CommonMarkCache::default(),
            transcript_cache: Default::default(),
            document_preview: Default::default(),
            notes_cache: Default::default(),
            meetings_cache: Default::default(),
            tasks_cache: Default::default(),
            meeting_tasks: None,
            image_viewer: None,
            confirm_regenerate: false,
            rename: None,
            rename_focus: false,
            confirm_delete: false,
            import_picker: None,
            enhanced_preview: true,
            indicator: IndicatorPosition::load(),
            show_record: false,
            show_settings: false,
            settings_panel: Default::default(),
            quickstart: Default::default(),
            tasks_all: false,
            tasks_mine: false,
            tasks_person: String::new(),
            tasks_by_meeting: false,
            toasts: VecDeque::new(),
            toast_anchor: None,
            main_rect: None,
            home: true,
            applied_zoom: None,
            chat_page: false,
            ask_text: String::new(),
            focus_search: false,
            sidebar_open: false,
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
            && self
                .state
                .quickstart
                .models
                .speech_ready(&self.state.record_model)
            && !self.state.quickstart.probe_busy
    }
    fn search_field(&mut self, ui: &mut egui::Ui) {
        let search = theme::search_input(
            ui,
            &mut self.state.query,
            "library_search",
            "Поиск…",
            Some(if cfg!(target_os = "macos") {
                "⌘K"
            } else {
                "Ctrl K"
            }),
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
    }
    /// Which screen the navigation points at; a change closes the narrow-window sidebar.
    fn nav_key(&self) -> (bool, bool, bool, bool, bool, Option<String>) {
        (
            self.home,
            self.chat_page,
            self.state.tasks_open,
            self.show_settings,
            self.people.active(),
            self.state.folder_filter.clone(),
        )
    }
    fn sidebar(&mut self, ui: &mut egui::Ui, narrow: bool) {
        if self.show_settings {
            ui.horizontal(|ui| {
                if theme::icon_button(ui, theme::icons::CHEVRON_LEFT, "Назад").clicked() {
                    self.settings_panel.blur(&mut self.state);
                    self.show_settings = false;
                }
                ui.label(RichText::new("Настройки").font(theme::semibold(16.0)));
            });
            ui.add_space(24.0);
            for section in crate::settings::Section::ALL {
                let label = if section == crate::settings::Section::Storage {
                    "Хранилище"
                } else {
                    section.label()
                };
                if theme::nav_row(ui, label, self.settings_panel.section == section).clicked() {
                    self.settings_panel.blur(&mut self.state);
                    self.settings_panel.section = section;
                }
            }
            return;
        }
        // In a narrow window the search lives in the top bar, so typing never hides it.
        if !narrow {
            self.search_field(ui);
            ui.add_space(24.0);
        }
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
        // Same look as the neighbouring nav rows: a ghost button here read as disabled.
        let more = theme::nav_button(Palette::of(ui), false)
            .min_size(egui::vec2(ui.available_width(), 34.0));
        let (more, _) = egui::containers::menu::MenuButton::from_button(more).ui(ui, |ui| {
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
            if crate::theme::button(
                ui,
                "Перечитать библиотеку",
                crate::theme::ButtonStyle::Ghost,
            )
            .clicked()
            {
                self.state.flush();
                self.state.refresh();
                ui.close();
            }
            if ui
                .add_enabled(
                    self.import_picker.is_none() && self.can_record(),
                    crate::theme::styled_button(
                        crate::theme::Palette::of(ui),
                        "Импортировать запись…",
                        crate::theme::ButtonStyle::Secondary,
                    ),
                )
                .on_hover_text("Аудио или видео с диска: будет распознано как новая встреча")
                .clicked()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                self.import_picker = Some(rx);
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ = tx.send(rfd::FileDialog::new().pick_file());
                    ctx.request_repaint();
                });
                ui.close();
            }
            if self.maintenance.check_updates_button(ui) {
                ui.close();
            }
        });
        if let Some(picked) = self
            .import_picker
            .as_ref()
            .and_then(|picker| picker.try_recv().ok())
        {
            self.import_picker = None;
            if let Some(file) = picked {
                self.home = false;
                self.state.import_recording(file);
            }
        }
        theme::nav_label(ui, more, "Ещё", false);
        if theme::nav_row(ui, "Настройки", self.show_settings).clicked() {
            self.show_settings = true;
        }
    }
    fn meetings(&mut self, ui: &mut egui::Ui) {
        let title = self
            .state
            .folder_filter
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("Встречи");
        theme::page_heading(ui, title);
        if self
            .state
            .quickstart
            .models
            .missing(&self.state.settings.model)
            .is_some()
        {
            crate::quickstart::model_status(ui, &mut self.state);
            if !self
                .state
                .quickstart
                .models
                .speech_ready(&self.state.record_model)
            {
                ui.small("Запись станет доступна после подготовки распознавания.");
            }
        }
        let mut select = None;
        let mut start_event = None;
        self.meetings_cache.sync(&self.state, &self.organization);
        let count = self.meetings_cache.count;
        egui::ScrollArea::vertical()
            .id_salt("meetings")
            .show_viewport(ui, |ui, viewport| {
                ui.spacing_mut().item_spacing.y = 3.0;
                let origin = ui.cursor().min;
                ui.set_min_size(egui::vec2(
                    ui.available_width(),
                    self.meetings_cache.layout.total(),
                ));
                for index in self.meetings_cache.layout.visible(viewport) {
                    let mut row_ui = ui.new_child(egui::UiBuilder::new().id_salt(index).max_rect(
                        egui::Rect::from_min_size(
                            origin + egui::vec2(0.0, self.meetings_cache.layout.offset(index)),
                            egui::vec2(ui.available_width(), 43.0),
                        ),
                    ));
                    let ui = &mut row_ui;
                    match &self.meetings_cache.entries[index] {
                        MeetingEntry::Header(label) => {
                            ui.add_space(24.0);
                            theme::section_caption(ui, label);
                        }
                        MeetingEntry::Upcoming(index, time) => {
                            let event = &self.state.calendar_events[*index];
                            let meta = event.link.call_url.as_deref().map_or("", call_platform);
                            let (_, record) = theme::meeting_row(
                                ui,
                                (&event.link.uid, event.start.timestamp()),
                                time,
                                &event.link.title,
                                meta,
                                cfg!(any(windows, target_os = "macos")) && self.can_record(),
                                false,
                            );
                            if record {
                                start_event = Some(event.clone());
                            }
                        }
                        MeetingEntry::Meeting(index, time) => {
                            let row = &self.state.rows[*index];
                            let (response, _) = theme::meeting_row(
                                ui,
                                row.meta.id,
                                time,
                                &row.meta.title,
                                row.meta.folder.as_deref().unwrap_or(""),
                                false,
                                self.organization.selected(row.meeting.path()),
                            );
                            let response = response.on_hover_text(if row.snippet.is_empty() {
                                &row.meta.title
                            } else {
                                &row.snippet
                            });
                            if self.organization.row(ui, &response, &row.meeting) {
                                select = Some((row.meeting.clone(), row.anchor.clone()));
                            }
                        }
                    }
                }
                ui.add_space(self.meetings_cache.layout.total());
                if count == 0 && !self.state.listing {
                    let empty = self.state.query.is_empty();
                    if theme::empty_state(
                        ui,
                        if empty {
                            theme::icons::MIC
                        } else {
                            theme::icons::SEARCH
                        },
                        if empty {
                            "Здесь будут ваши встречи"
                        } else {
                            "Встречи не найдены"
                        },
                        if empty {
                            "Начните запись. Заметки и транскрипт сохранятся здесь."
                        } else {
                            "Попробуйте другие слова или другую папку."
                        },
                        empty.then_some(("Новая запись", self.can_record())),
                    ) {
                        self.state.prefill_call_title();
                        self.show_record = true;
                        self.state.inventory_pending = true;
                        self.state.send(Command::Inventory);
                    }
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
        let bar = theme::bar(p).show(ui, |ui| {
            ui.horizontal(|ui| {
                let phase = self.state.recording.phase;
                let busy = self.state.recording.busy();
                if busy {
                    if phase == Phase::Recording {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.0, p.red);
                        theme::waveform(ui, self.state.recording.level());
                    } else {
                        ui.spinner();
                    }
                    let elapsed = timestamp(self.state.recording.elapsed().as_millis() as u64);
                    if phase == Phase::Recording {
                        ui.label(RichText::new("Запись").size(14.0).color(p.muted));
                    }
                    ui.add_sized(
                        [48.0, 28.0],
                        egui::Label::new(
                            RichText::new(elapsed)
                                .font(theme::medium(14.0))
                                .color(p.text),
                        ),
                    );
                }
                if !busy {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 32.0), egui::Sense::hover());
                    theme::paint_icon(ui, rect.center(), theme::icons::SPARKLES, 16.0, p.muted);
                }
                // Allocate actions first. The input takes the remaining width,
                // including when fonts or zoom change. No guessed text widths.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let stopping = !self.state.recording.importing
                        && matches!(phase, Phase::Starting | Phase::Recording);
                    if stopping {
                        stop = theme::button(ui, "Стоп", ButtonStyle::Secondary).clicked();
                    } else if !busy
                        && ui
                            .add_enabled(
                                self.can_record(),
                                theme::styled_button(p, "Новая запись", ButtonStyle::Primary)
                                    .min_size(egui::vec2(0.0, theme::CTA_HEIGHT)),
                            )
                            .on_disabled_hover_text(
                                "Запись станет доступна, когда загрузятся модели распознавания",
                            )
                            .clicked()
                    {
                        self.state.prefill_call_title();
                        self.show_record = true;
                        self.state.inventory_pending = true;
                        self.state.send(Command::Inventory);
                    }
                    if !self.ask_text.trim().is_empty() {
                        ask |= ui
                            .add_enabled(
                                self.state.ready
                                    && !self.state.chat.busy
                                    && !busy
                                    && !self.state.unsaved()
                                    && !self.state.settings_pending,
                                crate::theme::styled_button(
                                    crate::theme::Palette::of(ui),
                                    "Спросить",
                                    crate::theme::ButtonStyle::Secondary,
                                ),
                            )
                            .on_disabled_hover_text("Дождитесь сохранения встречи")
                            .clicked();
                    }
                    if busy {
                        return;
                    }
                    let response = ui.add_sized(
                        [ui.available_width().max(24.0), 32.0],
                        crate::theme::text_field(&mut self.ask_text)
                            .frame(egui::Frame::NONE)
                            .align(egui::Align2::LEFT_CENTER)
                            .hint_text(if phase == Phase::Finalizing {
                                // The final pass takes minutes after a long meeting.
                                match self.state.recording.progress {
                                    Some((completed, total)) => {
                                        format!("Распознаю запись: {completed} из {total}…")
                                    }
                                    None => "Завершаю запись…".to_owned(),
                                }
                            } else if let Some(notice) = self.state.recording.silence_notice() {
                                notice.to_owned()
                            } else if busy {
                                "Идёт запись. Чат доступен после неё.".to_owned()
                            } else if note || (self.chat_page && self.state.chat.scope.is_some()) {
                                "Спросить про эту встречу…".to_owned()
                            } else {
                                "Спросить по всем встречам…".to_owned()
                            }),
                    );
                    ask |= response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                });
            });
        });
        self.toast_anchor = Some(egui::pos2(
            bar.response.rect.center().x,
            bar.response.rect.top() - 12.0,
        ));
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

    fn sync_tasks_cache(&mut self) {
        let key = crate::virtual_rows::hash((
            self.state.view_revision,
            self.organization.revision(),
            &self.state.folder_filter,
            &self.state.tag_filter,
            self.tasks_all,
            self.tasks_mine,
            self.tasks_by_meeting,
            &self.tasks_person,
            &self.state.settings.user_name,
            &self.state.settings.user_aliases,
        ));
        if self.tasks_cache.key == Some(key) {
            return;
        }
        self.tasks_cache.owners = self
            .state
            .tasks
            .iter()
            .map(|task| task.owner.clone())
            .filter(|s| !s.is_empty())
            .collect();
        self.tasks_cache.owners.sort();
        self.tasks_cache.owners.dedup();
        let names: Vec<_> = std::iter::once(self.state.settings.user_name.as_str())
            .chain(self.state.settings.user_aliases.split([',', ';', '\n']))
            .collect();
        let mut tasks: Vec<_> = self
            .state
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| {
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
        tasks.sort_by_cached_key(|(_, task)| {
            (
                group(task),
                self.tasks_by_meeting.then(|| task.path.clone()),
                task.due.is_none(),
                task.due,
                task.path.clone(),
                task.line,
            )
        });

        self.tasks_cache.entries.clear();
        let mut previous = None;
        for (index, task) in tasks {
            let title = group(task);
            let group_key = (
                title.clone(),
                self.tasks_by_meeting.then(|| task.path.clone()),
            );
            if previous.as_ref() != Some(&group_key) {
                self.tasks_cache.entries.push(TaskEntry::Header(title));
                previous = Some(group_key);
            }
            self.tasks_cache.entries.push(TaskEntry::Task(index));
        }
        self.tasks_cache.key = Some(key);
    }
    fn tasks_screen(&mut self, ui: &mut egui::Ui) {
        theme::page_heading(ui, "Задачи");
        self.sync_tasks_cache();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            theme::segmented(
                ui,
                &mut self.tasks_all,
                &[(false, "Открытые"), (true, "Все")],
            );
            theme::toggle_chip(ui, &mut self.tasks_mine, "Мои");
            theme::toggle_chip(ui, &mut self.tasks_by_meeting, "По встречам");
            crate::theme::select("task_person")
                .width(140.0)
                .selected_text(if self.tasks_person.is_empty() {
                    "Все ответственные"
                } else {
                    &self.tasks_person
                })
                .show_ui(ui, |ui| {
                    crate::theme::select_value(
                        ui,
                        &mut self.tasks_person,
                        String::new(),
                        "Все ответственные",
                    );
                    for person in &self.tasks_cache.owners {
                        crate::theme::select_value(
                            ui,
                            &mut self.tasks_person,
                            person.clone(),
                            person,
                        );
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled_ui(!self.state.tasks_busy, |ui| {
                        theme::icon_button(ui, theme::icons::REFRESH_CW, "Обновить задачи")
                    })
                    .inner
                    .clicked()
                {
                    self.state.refresh_tasks();
                }
            });
        });
        if self.tasks_mine
            && self.state.settings.user_name.trim().is_empty()
            && self.state.settings.user_aliases.trim().is_empty()
        {
            ui.label("Укажите «Как вас зовут» в настройках");
        }
        self.sync_tasks_cache();
        let can_toggle = !self.state.tasks_busy
            && !self.state.unsaved()
            && !self.state.recording.busy()
            && self.state.enhancing.is_none();
        let mut send_task = None;
        let mut toggle = None;
        let mut open = None;
        egui::ScrollArea::vertical().id_salt("all_tasks").show_rows(
            ui,
            48.0,
            self.tasks_cache.entries.len().max(1),
            |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if self.tasks_cache.entries.is_empty() && !self.state.tasks_busy {
                    ui.weak("Нет задач по выбранным фильтрам");
                }
                for index in range {
                    let Some(entry) = self.tasks_cache.entries.get(index) else {
                        continue;
                    };
                    let task = match entry {
                        TaskEntry::Header(title) => {
                            ui.add_space(24.0);
                            theme::section_caption(ui, title);
                            ui.add_space(8.0);
                            continue;
                        }
                        TaskEntry::Task(index) => &self.state.tasks[*index],
                    };
                    ui.push_id((&task.path, task.line), |ui| {
                        let p = Palette::of(ui);
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 48.0),
                            egui::Sense::hover(),
                        );
                        theme::row_background(ui, &response, false);
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                                rect.left_top() + egui::vec2(8.0, 12.0),
                                egui::vec2(16.0, 24.0),
                            )),
                            |ui| {
                                let mut completed = task.completed;
                                if ui
                                    .add_enabled(
                                        can_toggle,
                                        theme::Checkbox::without_text(&mut completed),
                                    )
                                    .changed()
                                {
                                    toggle = Some((task.clone(), completed));
                                }
                            },
                        );
                        let title_rect = egui::Rect::from_min_size(
                            rect.min + egui::vec2(36.0, 6.0),
                            egui::vec2((rect.width() - 80.0).max(1.0), 36.0),
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(title_rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                            |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                let overdue = !task.completed
                                    && task.due.is_some_and(|date| {
                                        date < chrono::Local::now().date_naive()
                                    });
                                let label = RichText::new(&task.text)
                                    .font(theme::medium(14.0))
                                    .color(if overdue { p.red } else { p.text });
                                if ui
                                    .add(
                                        egui::Label::new(if task.completed {
                                            label.strikethrough()
                                        } else {
                                            label
                                        })
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                    )
                                    .clicked()
                                {
                                    open = Some(task.clone());
                                }
                                let due = task.due.map_or_else(
                                    || task.due_text.clone(),
                                    |date| date.format("%d.%m.%Y").to_string(),
                                );
                                let metadata = [
                                    task.meeting_title.as_str(),
                                    task.owner.as_str(),
                                    due.as_str(),
                                ]
                                .into_iter()
                                .filter(|part| !part.is_empty())
                                .collect::<Vec<_>>()
                                .join(" · ");
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(metadata).size(12.0).color(p.tertiary),
                                    )
                                    .truncate(),
                                );
                            },
                        );
                        if task.issue_url.is_some() || !task.completed {
                            ui.scope_builder(
                                egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                                    rect.right_top() - egui::vec2(36.0, -10.0),
                                    egui::vec2(28.0, 28.0),
                                )),
                                |ui| {
                                    let glyph = if task.issue_url.is_some() {
                                        theme::icons::EXTERNAL_LINK
                                    } else {
                                        theme::icons::SEND
                                    };
                                    let tooltip = if task.issue_url.is_some() {
                                        "В трекере"
                                    } else {
                                        "Отправить в трекер"
                                    };
                                    if ui
                                        .add_enabled_ui(
                                            task.issue_url.is_some() || can_toggle,
                                            |ui| {
                                                theme::icon_button_visible(
                                                    ui,
                                                    glyph,
                                                    tooltip,
                                                    ui.rect_contains_pointer(rect),
                                                )
                                            },
                                        )
                                        .inner
                                        .clicked()
                                    {
                                        if let Some(url) = &task.issue_url {
                                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                        } else {
                                            send_task = Some(task.clone());
                                        }
                                    }
                                },
                            );
                        }
                    });
                }
                if self.state.tasks_busy {
                    ui.spinner();
                }
            },
        );
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
        if ui.ctx().content_rect().height() < 600.0 {
            ui.spacing_mut().item_spacing.y = 4.0;
        }
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
        if self
            .meeting_tasks
            .as_ref()
            .is_none_or(|(revision, selected, _)| {
                *revision != self.state.view_revision || *selected != path
            })
        {
            let tasks = self
                .state
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| {
                    task.path == path && !task.completed && task.issue_url.is_none()
                })
                .map(|(index, _)| index)
                .collect();
            self.meeting_tasks = Some((self.state.view_revision, path.clone(), tasks));
        }
        let can_send_tasks = !self
            .meeting_tasks
            .as_ref()
            .expect("indexed tasks")
            .2
            .is_empty()
            && self.trackers.can_prepare(&self.state);
        let mut send_tasks = false;
        let Some(document) = self.state.documents.get_mut(&path) else {
            ui.spinner();
            return;
        };
        let mut enhance = false;
        let mut retranscribe = false;
        let mut export = None;
        let mut series_action = None;
        let mut action = None;
        let mut pin_toggle = None;
        let pinned = self.state.settings_draft.pinned_actions.clone();
        let (can_edit_summary, can_send) =
            (document.generation.is_none() && !locked, can_send_tasks);
        let enabled = move |action: DocumentAction| match action {
            DocumentAction::EditSummary => can_edit_summary,
            DocumentAction::Regenerate => can_enhance,
            DocumentAction::SendTasks => can_send,
            DocumentAction::Rename | DocumentAction::Delete => can_enhance && !locked,
            _ => true,
        };
        ui.horizontal(|ui| {
            if theme::icon_button(ui, theme::icons::CHEVRON_LEFT, "Встречи").clicked() {
                self.home = true;
            }
            if theme::button(ui, "Встречи", ButtonStyle::Ghost).clicked() {
                self.home = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let menu_response =
                    theme::icon_button(ui, theme::icons::ELLIPSIS, "Действия встречи");
                let menu_height =
                    (ui.ctx().content_rect().bottom() - menu_response.rect.bottom() - 8.0)
                        .max(80.0);
                egui::Popup::menu(&menu_response)
                    .align(egui::emath::RectAlign::BOTTOM_END)
                    .align_alternatives(&[])
                    .gap(4.0)
                    .show(|ui| {
                        ui.set_width(300.0);
                        ui.set_max_height(menu_height);
                        let compact = menu_height < 460.0;
                        if compact {
                            ui.spacing_mut().interact_size.y =
                                ((menu_height - 44.0) / 13.0).floor().clamp(18.0, 24.0);
                        }
                        ui.spacing_mut().item_spacing.y = 0.0;
                        egui::ScrollArea::vertical()
                            .id_salt("meeting-actions-scroll")
                            .max_height(menu_height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let mut row = |ui: &mut egui::Ui, item: DocumentAction| {
                                    if ui
                                        .add_enabled_ui(enabled(item), |ui| {
                                            theme::menu_item(
                                                ui,
                                                action_icon(item),
                                                item.label(),
                                                item == DocumentAction::Delete,
                                                false,
                                            )
                                        })
                                        .inner
                                        .clicked()
                                    {
                                        action = Some(item);
                                        ui.close();
                                    }
                                };
                                row(ui, DocumentAction::EditNotes);
                                row(ui, DocumentAction::EditSummary);
                                row(ui, DocumentAction::Read);
                                ui.add(egui::Separator::default().spacing(if compact {
                                    4.0
                                } else {
                                    8.0
                                }));
                                row(ui, DocumentAction::Regenerate);
                                theme::submenu(ui, theme::icons::FILE_TEXT, "Шаблон", |ui| {
                                    for name in &self.state.templates {
                                        if theme::select_value(
                                            ui,
                                            &mut document.template,
                                            name.clone(),
                                            name,
                                        )
                                        .clicked()
                                        {
                                            ui.close();
                                        }
                                    }
                                });
                                ui.add(egui::Separator::default().spacing(if compact {
                                    4.0
                                } else {
                                    8.0
                                }));
                                theme::submenu(
                                    ui,
                                    theme::icons::COPY,
                                    "Копировать",
                                    |ui| {
                                        row(ui, DocumentAction::Copy);
                                        row(ui, DocumentAction::CopyAll);
                                    },
                                );
                                theme::submenu(ui, theme::icons::DOWNLOAD, "Экспорт", |ui| {
                                    row(ui, DocumentAction::ExportSummary);
                                    theme::submenu(
                                        ui,
                                        theme::icons::DOWNLOAD,
                                        "Экспорт транскрипта",
                                        |ui| {
                                            for (format, label) in [
                                                (notetaker_core::ExportFormat::Md, "Markdown"),
                                                (notetaker_core::ExportFormat::Srt, "SRT"),
                                                (notetaker_core::ExportFormat::Vtt, "VTT"),
                                                (notetaker_core::ExportFormat::Csv, "CSV"),
                                                (notetaker_core::ExportFormat::Json, "JSON"),
                                            ] {
                                                if theme::menu_item(ui, "", label, false, false)
                                                    .clicked()
                                                {
                                                    export = Some(Command::ExportTranscript {
                                                        meeting: document.data.meeting.clone(),
                                                        transcript: document
                                                            .data
                                                            .transcript
                                                            .clone(),
                                                        format,
                                                    });
                                                    ui.close();
                                                }
                                            }
                                        },
                                    );
                                });
                                ui.add(egui::Separator::default().spacing(if compact {
                                    4.0
                                } else {
                                    8.0
                                }));
                                row(ui, DocumentAction::Reveal);
                                row(ui, DocumentAction::SendTasks);
                                theme::submenu(
                                    ui,
                                    theme::icons::REPEAT,
                                    "Серия встречи",
                                    |ui| {
                                        series_action = self.people.header(
                                            ui,
                                            &document.data.meta,
                                            !locked && can_enhance,
                                        );
                                    },
                                );
                                ui.add(egui::Separator::default().spacing(if compact {
                                    4.0
                                } else {
                                    8.0
                                }));
                                theme::submenu(
                                    ui,
                                    theme::icons::PIN,
                                    "Кнопки на панели",
                                    |ui| {
                                        for item in DocumentAction::ALL {
                                            if theme::menu_item(
                                                ui,
                                                action_icon(item),
                                                item.label(),
                                                false,
                                                pinned.contains(&item),
                                            )
                                            .clicked()
                                            {
                                                pin_toggle = Some(item);
                                            }
                                        }
                                    },
                                );
                                ui.add(egui::Separator::default().spacing(if compact {
                                    4.0
                                } else {
                                    8.0
                                }));
                                row(ui, DocumentAction::Rename);
                                row(ui, DocumentAction::Delete);
                            });
                    });
                // Right-to-left layout: reverse so pinned buttons read in menu order.
                for &item in pinned.iter().rev() {
                    if ui
                        .add_enabled(
                            enabled(item),
                            theme::styled_button(
                                Palette::of(ui),
                                item.short_label(),
                                ButtonStyle::Ghost,
                            )
                            .icon(action_icon(item))
                            .min_size(egui::vec2(0.0, theme::COMPACT_HEIGHT)),
                        )
                        .on_hover_text(item.label())
                        .clicked()
                    {
                        action = Some(item);
                    }
                }
            });
        });
        let summary = if document.data.enhanced.is_empty() {
            &document.data.notes
        } else {
            &document.data.enhanced
        };
        match action {
            Some(DocumentAction::EditNotes) => self.state.tab = Tab::Notes,
            Some(DocumentAction::EditSummary | DocumentAction::Read) => {
                self.state.tab = Tab::Enhanced;
                self.enhanced_preview = action == Some(DocumentAction::Read);
            }
            Some(DocumentAction::Regenerate) => enhance = true,
            Some(DocumentAction::Copy) => {
                ui.ctx().copy_text(summary.clone());
                self.state
                    .notices
                    .push((ToastLevel::Success, "Конспект скопирован".into()));
            }
            Some(DocumentAction::CopyAll) => {
                let transcript = document
                    .data
                    .transcript
                    .export(notetaker_core::ExportFormat::Md)
                    .unwrap_or_default();
                ui.ctx().copy_text(format!(
                    "{}\n\n## Транскрипт\n\n{}",
                    summary.trim_end(),
                    transcript.trim_end()
                ));
                self.state.notices.push((
                    ToastLevel::Success,
                    "Конспект и транскрипт скопированы".into(),
                ));
            }
            Some(DocumentAction::Reveal) => export = Some(Command::Reveal(path.clone())),
            Some(DocumentAction::SendTasks) => send_tasks = true,
            Some(DocumentAction::ExportSummary) => {
                export = Some(Command::Export {
                    meeting: document.data.meeting.clone(),
                    text: document.data.enhanced.clone(),
                })
            }
            Some(DocumentAction::Rename) => {
                self.rename = Some(document.data.meta.title.clone());
                self.rename_focus = true;
            }
            Some(DocumentAction::Delete) => self.confirm_delete = true,
            None => {}
        }
        let compact = ui.ctx().content_rect().height() < 600.0;
        ui.add_space(if compact { 4.0 } else { 24.0 });
        let mut title = egui::text::LayoutJob::simple(
            document.data.meta.title.clone(),
            theme::semibold(theme::TITLE),
            Palette::of(ui).text,
            ui.available_width(),
        );
        title.wrap.max_rows = 2;
        ui.label(title).on_hover_text(&document.data.meta.title);
        ui.add_space(if compact { 4.0 } else { 12.0 });
        let start = document.data.meta.started_at.with_timezone(&chrono::Local);
        let timeline = crate::speakers::timeline(document);
        let participants = document
            .data
            .meta
            .calendar_event
            .as_ref()
            .map(|e| e.attendees.len())
            .unwrap_or(timeline.timeline.speakers.len());
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(theme::icons::CALENDAR)
                    .font(egui::FontId::new(
                        16.0,
                        egui::FontFamily::Name("icons".into()),
                    ))
                    .color(Palette::of(ui).muted),
            );
            ui.label(
                RichText::new(start.format("%d.%m.%Y · %H:%M").to_string())
                    .small()
                    .color(Palette::of(ui).muted),
            );
            ui.label(
                RichText::new(theme::icons::USERS)
                    .font(egui::FontId::new(
                        16.0,
                        egui::FontFamily::Name("icons".into()),
                    ))
                    .color(Palette::of(ui).muted),
            );
            ui.label(
                RichText::new(format!("Участники: {participants}"))
                    .small()
                    .color(Palette::of(ui).muted),
            );
            // Folder assignment keeps its existing atomic save and sync-conflict handling.
            self.organization.meeting(ui, &mut self.state);
        });
        ui.add_space(if compact { 4.0 } else { 16.0 });
        theme::segmented(
            ui,
            &mut self.state.tab,
            &[
                (Tab::Enhanced, "Конспект"),
                (Tab::Transcript, "Транскрипт"),
                (Tab::Notes, "Заметки"),
            ],
        );
        if let Some(item) = pin_toggle {
            let mut list = pinned;
            if let Some(index) = list.iter().position(|a| *a == item) {
                list.remove(index);
            } else {
                list.push(item);
            }
            self.state.settings_draft.pinned_actions = DocumentAction::ALL
                .into_iter()
                .filter(|a| list.contains(a))
                .collect();
            self.state.save_preferences();
        }
        if send_tasks {
            let tasks = self
                .meeting_tasks
                .as_ref()
                .expect("indexed tasks")
                .2
                .iter()
                .map(|&index| self.state.tasks[index].clone())
                .collect();
            self.trackers.prepare(ui.ctx(), &self.state, tasks);
        }
        let Some(document) = self.state.documents.get_mut(&path) else {
            return;
        };
        if !document.data.meta.capture_issues.is_empty() {
            theme::callout(ui, theme::icons::TRIANGLE_ALERT, |ui| {
                crate::theme::disclosure("Неполный захват звука").show(ui, |ui| {
                    for issue in &document.data.meta.capture_issues {
                        ui.small(format!(
                            "{} с {}: {}",
                            speaker(issue.channel),
                            timestamp(issue.from_ms),
                            issue.reason
                        ));
                    }
                });
            });
        }
        if document.data.unfinished && !locked {
            theme::callout(ui, theme::icons::TRIANGLE_ALERT, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.weak("Запись прервалась, текст распознан не до конца.");
                    retranscribe = ui
                        .add_enabled(
                            can_enhance,
                            crate::theme::styled_button(
                                crate::theme::Palette::of(ui),
                                "Распознать из аудио",
                                crate::theme::ButtonStyle::Secondary,
                            ),
                        )
                        .on_hover_text(
                            "Аудио сохранилось. Распознавание пройдёт заново по всей записи.",
                        )
                        .clicked();
                });
            });
        }
        if document.summary_stale() && document.generation.is_none() {
            theme::callout(ui, theme::icons::INFO, |ui| {
                ui.horizontal(|ui| {
                    ui.weak("Заметки изменились после конспекта");
                    enhance |= ui
                        .add_enabled(
                            can_enhance,
                            crate::theme::styled_button(
                                crate::theme::Palette::of(ui),
                                "Обновить",
                                crate::theme::ButtonStyle::Secondary,
                            ),
                        )
                        .clicked();
                });
            });
        }
        if document.generation.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Пишу конспект…");
                if can_cancel
                    && crate::theme::button(ui, "Отменить", crate::theme::ButtonStyle::Ghost)
                        .clicked()
                {
                    self.state.generation_cancel.cancel();
                }
            });
        }
        ui.add_space(if compact { 4.0 } else { 24.0 });
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
        let mut rediarize = None;
        if self.state.tab == Tab::Transcript {
            let unavailable = if self.state.recording.busy() {
                Some("Доступно после завершения записи")
            } else if !document.data.system_audio {
                Some("Аудио этой встречи не сохранено, пересчитать собеседников нельзя")
            } else if document.speaker_ui.recalculating {
                Some("Пересчитываю собеседников…")
            } else if document.saving
                || speaker_busy
                || document.attachments_pending > 0
                || self.state.enhancing.is_some()
                || self.state.settings_pending
                || self.state.recording.assist.bookmarks_pending > 0
            {
                Some("Дождитесь завершения текущей операции")
            } else {
                None
            };
            let editable = !locked
                && !document.saving
                && !speaker_busy
                && document.attachments_pending == 0
                && self.state.enhancing.is_none();
            let (count, toolbar_edit) =
                crate::speakers::count_control(ui, document, unavailable, editable);
            rediarize = count;
            speaker_edit = transcript_view(
                ui,
                &mut self.transcript_cache,
                document,
                locked,
                self.state
                    .settings
                    .voice_library
                    .then_some(&self.state.voices),
                editable,
                global_allowed,
            )
            .or(toolbar_edit);
        } else if self.state.tab == Tab::Notes
            || locked
            || (document.data.enhanced.is_empty() && document.generation.is_none())
        {
            if !locked
                && !document.data.enhanced.is_empty()
                && crate::theme::button(ui, "Готово", crate::theme::ButtonStyle::Ghost).clicked()
            {
                self.state.tab = Tab::Enhanced;
                self.enhanced_preview = true;
            }
            if document.data.enhanced.is_empty() && document.generation.is_none() && !locked {
                enhance |= ui
                    .add_enabled(
                        can_enhance,
                        crate::theme::styled_button(
                            crate::theme::Palette::of(ui),
                            "Создать конспект",
                            crate::theme::ButtonStyle::Secondary,
                        ),
                    )
                    .clicked();
                ui.add_space(12.0);
            }
            images = notes_editor(
                ui,
                document,
                &mut self.notes_cache,
                &path,
                &mut self.image_viewer,
            );
        } else if !self.enhanced_preview && document.generation.is_none() {
            if crate::theme::button(ui, "Готово", crate::theme::ButtonStyle::Ghost).clicked()
            {
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
            self.document_preview.prepare(document, &path);
            ui.push_id(&path, |ui| {
                enhanced_document(
                    ui,
                    &mut self.cache,
                    &mut self.document_preview,
                    &mut self.image_viewer,
                )
            });
        }

        if let Some(count) = rediarize {
            self.state.rediarize(count);
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
            let edited = self
                .state
                .documents
                .get(&path)
                .is_some_and(|document| !crate::worker::summary_can_overwrite(&document.data));
            if edited {
                self.confirm_regenerate = true;
            } else {
                self.enhanced_preview = true;
                self.state.enhance();
            }
        }
        if retranscribe {
            self.state.retranscribe(&path);
        }
        if let Some(title) = &mut self.rename {
            let mut save = false;
            let mut cancel = false;
            let closed = theme::modal(ui.ctx(), "Переименовать встречу")
                .default_width(420.0)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    let field = ui.add(
                        crate::theme::text_field(title)
                            .desired_width(f32::INFINITY)
                            .hint_text("Название встречи"),
                    );
                    // Only once: refocusing later would make Esc leave the field forever.
                    if std::mem::take(&mut self.rename_focus) {
                        field.request_focus();
                    }
                    save = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(8.0);
                    theme::footer(ui, |ui| {
                        save |= ui
                            .add_enabled(
                                !title.trim().is_empty(),
                                theme::styled_button(
                                    Palette::of(ui),
                                    "Сохранить",
                                    ButtonStyle::Primary,
                                ),
                            )
                            .clicked();
                        cancel = theme::button(ui, "Отмена", ButtonStyle::Secondary).clicked();
                    });
                });
            if save && !title.trim().is_empty() {
                let title = std::mem::take(title);
                self.state.rename_meeting(&path, &title);
                self.rename = None;
            } else if cancel || closed {
                self.rename = None;
            }
        }
        if self.confirm_delete {
            let mut delete = false;
            let mut keep = false;
            let closed = theme::modal(ui.ctx(), "Удалить встречу?")
                .default_width(420.0)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(
                        "Встреча исчезнет из списка и поиска. Её файлы, включая аудио, переедут в папку .trash внутри библиотеки: оттуда их можно вернуть или стереть вручную.",
                    );
                    ui.add_space(8.0);
                    theme::footer(ui, |ui| {
                        delete = theme::button(ui, "Удалить", ButtonStyle::DangerFilled).clicked();
                        keep = theme::button(ui, "Отмена", ButtonStyle::Secondary).clicked();
                    });
                });
            if delete {
                self.state.trash_meeting(&path);
            }
            if delete || keep || closed {
                self.confirm_delete = false;
            }
        }
        if self.confirm_regenerate {
            let mut regenerate = false;
            let mut keep = false;
            let closed = theme::modal(ui.ctx(), "Пересоздать конспект?")
                .default_width(420.0)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Конспект правили вручную. Новый текст заменит его, правки пропадут.");
                    ui.add_space(8.0);
                    theme::footer(ui, |ui| {
                        regenerate =
                            theme::button(ui, "Пересоздать", ButtonStyle::Primary).clicked();
                        keep = theme::button(ui, "Оставить как есть", ButtonStyle::Secondary)
                            .clicked();
                    });
                });
            if regenerate {
                self.enhanced_preview = true;
                self.state.enhance();
            }
            if regenerate || keep || closed {
                self.confirm_regenerate = false;
            }
        }
        if let Some(command) = export {
            self.state.send(command);
        }
    }
    fn advance_toasts(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| {
            i.focused
                && i.viewport().focused.unwrap_or(i.focused)
                && !i.viewport().minimized.unwrap_or(false)
        });
        let now = Instant::now();
        let lost_focus = ctx.input(|i| {
            i.events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        });
        for toast in &mut self.toasts {
            if lost_focus {
                toast.was_focused = false;
            }
            toast.tick(now, focused);
        }
        self.toasts.retain(|toast| !toast.expired(now));
        for toast in &self.toasts {
            let delay = match toast.level {
                ToastLevel::Warning if focused => Some(Duration::from_millis(500)),
                ToastLevel::Info | ToastLevel::Success => {
                    Some((toast.at + Duration::from_secs(6)).saturating_duration_since(now))
                }
                _ => None,
            };
            if let Some(delay) = delay {
                ctx.request_repaint_after(delay);
            }
        }
    }
    fn notifications(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| {
            i.focused
                && i.viewport().focused.unwrap_or(i.focused)
                && !i.viewport().minimized.unwrap_or(false)
        });
        for (level, text) in self
            .state
            .warnings
            .drain(..)
            .chain(self.state.recording.messages.drain(..))
            .map(|text| (ToastLevel::Warning, text))
            .chain(self.state.notices.drain(..))
        {
            push_toast(
                &mut self.toasts,
                Toast {
                    at: Instant::now(),
                    text: notetaker_core::display_text(&text),
                    path: None,
                    retry_settings: matches!(level, ToastLevel::Error)
                        && self.state.settings_autosave.error.as_deref() == Some(text.as_str()),
                    level,
                    count: 1,
                    focused_for: Duration::ZERO,
                    last_tick: Instant::now(),
                    was_focused: focused,
                },
            );
        }
        for path in self.state.exported.drain(..) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            push_toast(
                &mut self.toasts,
                Toast {
                    at: Instant::now(),
                    text: format!("Файл обновлён: {name}"),
                    path: Some(path),
                    retry_settings: false,
                    level: ToastLevel::Success,
                    count: 1,
                    focused_for: Duration::ZERO,
                    last_tick: Instant::now(),
                    was_focused: focused,
                },
            );
        }
        self.advance_toasts(ctx);
        if self.toasts.is_empty() && self.state.error.is_none() {
            return;
        }
        egui::Area::new(egui::Id::unique("notifications"))
            .anchor(
                egui::Align2::CENTER_BOTTOM,
                self.toast_anchor.map_or(egui::vec2(0.0, -12.0), |p| {
                    egui::vec2(
                        p.x - ctx.content_rect().center().x,
                        p.y - ctx.content_rect().bottom(),
                    )
                }),
            )
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                let mut reveal = None;
                let mut dismiss = None;
                let mut retry = false;
                for (index, toast) in self.toasts.iter().enumerate() {
                    let response = theme::toast(
                        ui,
                        egui::Id::unique(("toast", toast.at)),
                        toast.level,
                        &toast.text,
                        toast.count,
                        if toast.retry_settings {
                            Some("Повторить")
                        } else {
                            toast.path.as_ref().map(|_| "Показать в папке")
                        },
                    );
                    if response.close {
                        dismiss = Some(index);
                    }
                    if response.action {
                        reveal = toast.path.clone();
                        retry |= toast.retry_settings;
                    }
                }
                if retry {
                    self.state
                        .settings_edited(false, self.state.settings_autosave.llm);
                }
                if let Some(index) = dismiss {
                    self.toasts.remove(index);
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
                            if crate::theme::button(
                                ui,
                                "Настройки",
                                crate::theme::ButtonStyle::Ghost,
                            )
                            .clicked()
                            {
                                self.show_settings = true;
                            }
                            if crate::theme::button(ui, "Закрыть", crate::theme::ButtonStyle::Ghost)
                                .clicked()
                            {
                                self.state.error = None;
                            }
                        });
                    });
                }
            });
    }
    fn recording_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_record;
        let mut cancel = false;
        let mut start_record = false;
        let can_start = cfg!(any(windows, target_os = "macos")) && self.can_record();
        theme::modal(ctx,"Новая запись").open(&mut open).show_footer(ctx,|ui| {
            theme::form_field(ui,"Название",None,|ui|ui.add(theme::text_field(&mut self.state.title).desired_width(f32::INFINITY)));
            let mode_description=mode_label(self.state.record_mode);
            theme::form_field(ui,"Режим",Some(mode_description),|ui| {self.state.record_mode_edited |= mode_selector(ui,&mut self.state.record_mode);});
            theme::form_field(ui,"Источник звука",None,|ui| {
                ui.horizontal(|ui| {
                    crate::theme::select("audio_session").width((ui.available_width()-36.0).max(32.0)).selected_text(self.state.inventory.sessions.iter().find(|s|Some(s.pid)==self.state.process_id).map_or_else(||"Весь системный звук".to_owned(),|s|format!("{} (PID {})",s.exe.as_deref().unwrap_or(&s.name),s.pid))).show_ui(ui,|ui| {
                        theme::select_value(ui,&mut self.state.process_id,None,"Весь системный звук");
                        for session in &self.state.inventory.sessions {if session.pid!=0{theme::select_value(ui,&mut self.state.process_id,Some(session.pid),format!("{} (PID {}){}",session.exe.as_deref().unwrap_or(&session.name),session.pid,if session.active{" · активна"}else{""}));}}
                    });
                    if ui.add_enabled_ui(!self.state.inventory_pending,|ui|theme::icon_button(ui,theme::icons::REFRESH_CW,"Обновить аудиосессии")).inner.clicked(){self.state.inventory_pending=true;self.state.send(Command::Inventory);}
                });
            });
            if self.state.inventory_pending{ui.spinner();}
            if let Some(error)=&self.state.inventory_error{theme::callout(ui,theme::icons::CIRCLE_ALERT,|ui|{ui.small(error);});}
            for warning in &self.state.inventory.warnings {theme::callout(ui,theme::icons::INFO,|ui|{ui.small(warning);});}
            #[cfg(target_os="macos")]
            theme::callout(ui,theme::icons::INFO,|ui|{ui.small("macOS: весь системный звук, кроме notetaker. Нужны разрешения Microphone и System Audio Recording.");});
            #[cfg(not(any(windows,target_os="macos")))]
            theme::callout(ui,theme::icons::INFO,|ui|{ui.small("Захват звука пока реализован только на Windows. Библиотека, поиск, правки и конспекты работают на этой ОС.");});
            if self.state.quickstart.models.missing(&self.state.settings.model).is_some(){crate::quickstart::model_status(ui,&mut self.state);}else{
                ui.horizontal(|ui|{let (rect,_)=ui.allocate_exact_size(egui::vec2(16.0,20.0),egui::Sense::hover());theme::paint_icon(ui,rect.center(),theme::icons::CHECK,16.0,Palette::of(ui).muted);ui.label(RichText::new("Все модели на месте").size(12.0).color(Palette::of(ui).muted));});
            }
        },|ui| {
            theme::footer(ui,|ui| {
                start_record=ui.add_enabled(can_start,theme::styled_button(Palette::of(ui),"Начать запись",ButtonStyle::Primary).icon(theme::icons::MIC)).clicked();
                cancel=theme::button(ui,"Отмена",ButtonStyle::Ghost).clicked();
            });
        });
        if start_record {
            self.state.toggle_recording();
            self.show_record = false;
        }
        if cancel {
            open = false;
        }
        self.show_record &= open;
    }
    fn exit(&mut self) {
        self.settings_panel.blur(&mut self.state);
        self.quickstart.blur(&mut self.state);
        self.state.exit();
    }
    fn settings_dialog(&mut self, ui: &mut egui::Ui) {
        if self.settings_panel.show(
            ui,
            &mut self.show_settings,
            &mut self.state,
            &mut self.maintenance,
            &mut self.organization,
        ) {
            self.quickstart.reopen();
            self.show_settings = false;
        }
    }
    fn call_notification(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.state.call_prompt.clone() else {
            return;
        };
        let mut accept = false;
        let mut dismiss = false;
        let mut builder = egui::ViewportBuilder::default();
        if let Some(rect) = self.main_rect {
            builder = builder.with_position(rect.right_bottom() - egui::vec2(398.0, 208.0));
        }
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("call-notification"),
            builder
                .with_title("notetaker · звонок")
                .with_inner_size([380.0, 190.0])
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
                                    && (matches!(prompt, CallPrompt::Stop { .. })
                                        || self
                                            .state
                                            .quickstart
                                            .models
                                            .speech_ready(&self.state.record_model))
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
            self.indicator.closed();
            return;
        }
        let mut stop = false;
        let mut open = false;
        let mut builder = egui::ViewportBuilder::default();
        if let Some(position) = self.indicator.spawn {
            builder = builder.with_position(position);
        }
        let indicator = &mut self.indicator;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("recording-indicator"),
            builder
                .with_title("notetaker · запись")
                .with_inner_size([240.0, 64.0])
                .with_min_inner_size([240.0, 64.0])
                .with_max_inner_size([240.0, 64.0])
                .with_resizable(false)
                .with_decorations(false)
                .with_transparent(true)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_active(false),
            |ui, _| {
                (open, stop) = recording_pill(
                    ui,
                    self.state.recording.elapsed(),
                    self.state.recording.level(),
                );
                ui.input(|input| {
                    if let Some(rect) = input.viewport().outer_rect
                        && !input.pointer.primary_down()
                    {
                        indicator.moved(rect.min);
                    }
                });
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
                    Action::Quit => self.exit(),
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
        crate::worker::set_waker(ctx);
        self.advance_toasts(ctx);
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
            self.exit();
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
            self.exit();
        }
        self.state.settings_draft.theme.apply(ctx);
        if self.state.ready
            && theme::sync_zoom(
                ctx,
                &mut self.applied_zoom,
                &mut self.state.settings_draft.ui_zoom,
            )
        {
            self.state.save_preferences();
        }
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
                self.settings_panel.blur(&mut self.state);
                self.quickstart.blur(&mut self.state);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.exit();
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            self.exit();
        }
        if self.state.can_exit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.state.recording.busy() || self.state.call_prompt.is_some() {
            // Allow the UI to create the indicator when a hotkey starts a
            // recording while the root window is hidden.
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
        if let Some(delay) = next_state_wake(&self.state, Instant::now()) {
            ctx.request_repaint_after(delay);
        }
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
        // Beside a call window the content needs the width; the navigation opens on demand.
        let narrow = ctx.content_rect().width() < NARROW_WIDTH;
        if !narrow {
            self.sidebar_open = false;
        }
        if narrow {
            egui::Panel::top("narrow_bar")
                .frame(
                    egui::Frame::new()
                        .fill(p.bg)
                        .inner_margin(egui::Margin::symmetric(16, 8)),
                )
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let label = if self.sidebar_open {
                            "Скрыть меню"
                        } else {
                            "Меню"
                        };
                        if theme::icon_button(ui, theme::icons::PANEL_LEFT, label).clicked() {
                            self.sidebar_open = !self.sidebar_open;
                        }
                        if self.show_settings {
                            if theme::icon_button(ui, theme::icons::CHEVRON_LEFT, "Назад").clicked()
                            {
                                self.settings_panel.blur(&mut self.state);
                                self.show_settings = false;
                            }
                            ui.label(RichText::new("Настройки").font(theme::semibold(16.0)));
                        } else {
                            self.search_field(ui);
                        }
                    });
                });
        }
        if !narrow || self.sidebar_open {
            let before = self.nav_key();
            egui::Panel::left("sidebar")
                .resizable(false)
                .exact_size(if ctx.content_rect().width() < 800.0 {
                    190.0
                } else {
                    240.0
                })
                .frame(
                    egui::Frame::new()
                        .fill(p.sidebar)
                        .inner_margin(egui::Margin::symmetric(16, 24)),
                )
                .show(ui, |ui| self.sidebar(ui, narrow));
            if narrow && self.nav_key() != before {
                self.sidebar_open = false;
            }
        }
        if self.show_settings {
            self.toast_anchor = Some(egui::pos2(
                ui.available_rect_before_wrap().center().x,
                ctx.content_rect().bottom() - 12.0,
            ));
            self.settings_dialog(ui);
            self.notifications(&ctx);
            self.indicator(&ctx);
            self.call_notification(&ctx);
            self.quickstart.show(&ctx, &mut self.state);
            return;
        }
        egui::Panel::bottom("bottom_bar")
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(24, 8)),
            )
            .show(ui, |ui| {
                let width = ui.available_width().min(720.0);
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 48.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| self.bottom_bar(ui),
                    );
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(
                if ctx.content_rect().height() < 600.0 {
                    16
                } else if ctx.content_rect().width() < 900.0 {
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
            theme::modal(&ctx, "Изображение")
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
        self.indicator(&ctx);
        self.call_notification(&ctx);
        let onboarding_open = self.quickstart.open;
        self.quickstart.show(&ctx, &mut self.state);
        if onboarding_open && !self.quickstart.open {
            self.home = true;
            self.chat_page = false;
            self.people.close();
            self.state.tasks_open = false;
        }
        if let Some(delay) = next_state_wake(&self.state, Instant::now()) {
            ctx.request_repaint_after(delay);
        }
    }
}
fn next_state_wake(state: &AppState, now: Instant) -> Option<Duration> {
    let mut deadlines = Vec::new();
    if state.recording.busy() || state.quickstart.probe_busy {
        deadlines.push(Duration::from_millis(100));
    }
    deadlines.extend(
        state
            .query_changed
            .map(|at| (at + Duration::from_millis(301)).saturating_duration_since(now)),
    );
    deadlines.extend(
        state
            .documents
            .values()
            .filter(|doc| doc.dirty.any() && !doc.saving)
            .map(|doc| {
                (doc.changed_at + Duration::from_millis(751)).saturating_duration_since(now)
            }),
    );
    if state.can_save_settings() {
        deadlines.extend(state.settings_autosave.wake_after(now));
    }
    deadlines.extend(state.connections.wake_after(now));
    deadlines.extend(crate::quickstart::wake_after(state, now));
    if state.ready
        && !state.settings.calendars.is_empty()
        && !state.calendar_busy
        && !state.exit_requested
    {
        deadlines.extend(
            state
                .calendar_checked
                .map(|at| (at + Duration::from_secs(600)).saturating_duration_since(now)),
        );
        let utc = chrono::Utc::now();
        for event in &state.calendar_events {
            for time in [
                event.start - chrono::Duration::minutes(5),
                event.start,
                event.end + chrono::Duration::seconds(1),
            ] {
                if time > utc
                    && let Ok(delay) = (time - utc).to_std()
                {
                    deadlines.push(delay);
                }
            }
        }
    }
    deadlines
        .into_iter()
        .min()
        .map(|delay| delay.max(Duration::from_millis(16)))
}

/// A click opens the main window; dragging any part except Stop moves the pill.
fn recording_pill(ui: &mut egui::Ui, elapsed: Duration, level: f32) -> (bool, bool) {
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
        .corner_radius(12)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_min_width(218.0);
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.0, p.red);
                theme::waveform(ui, level);
                ui.add_sized(
                    [48.0, 28.0],
                    egui::Label::new(timestamp(elapsed.as_millis() as u64)).selectable(false),
                );
                ui.add_space(16.0);
                stop = theme::button(ui, "Стоп", ButtonStyle::Secondary).clicked();
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

#[derive(Default)]
struct DocumentPreview {
    key: Option<(
        Instant,
        notetaker_core::Revision,
        notetaker_core::Revision,
        Option<usize>,
    )>,
    blocks: Vec<DocumentBlock>,
    layout: crate::virtual_rows::VirtualRows,
    revision: u64,
}
struct DocumentBlock {
    own: bool,
    kind: DocumentBlockKind,
}
enum DocumentBlockKind {
    Heading(usize, String),
    Item(Option<bool>, PreparedMarkdown),
    Markdown(PreparedMarkdown),
}
impl DocumentPreview {
    fn prepare(&mut self, document: &Document, path: &std::path::Path) {
        let key = (
            document.changed_at,
            document.data.revisions.notes,
            document.data.revisions.enhanced,
            document.generation.as_ref().map(String::len),
        );
        if self.key == Some(key) {
            return;
        }
        let runs = document_runs(
            &document.data.notes,
            document
                .generation
                .as_ref()
                .unwrap_or(&document.data.enhanced),
        );
        let lines: Vec<_> = runs
            .iter()
            .flat_map(|(own, text)| text.lines().map(move |line| (*own, line)))
            .collect();
        self.blocks.clear();
        let mut index = 0;
        while index < lines.len() {
            let (own, line) = lines[index];
            if line.trim().is_empty() {
                index += 1;
                continue;
            }
            if let Some((level, title)) = note_heading(line) {
                self.blocks.push(DocumentBlock {
                    own,
                    kind: DocumentBlockKind::Heading(level, title.into()),
                });
                index += 1;
            } else if let Some((_, task, body)) = note_item(line)
                && lines
                    .get(index + 1)
                    .is_none_or(|(_, next)| !next.starts_with([' ', '\t']))
            {
                self.blocks.push(DocumentBlock {
                    own,
                    kind: DocumentBlockKind::Item(task, PreparedMarkdown::new(body, path)),
                });
                index += 1;
            } else {
                let start = index;
                let mut fence = None;
                loop {
                    let current = lines[index].1.trim_start();
                    if current.starts_with("```") || current.starts_with("~~~") {
                        let marker = &current[..3];
                        if fence == Some(marker) {
                            fence = None;
                        } else if fence.is_none() {
                            fence = Some(marker);
                        }
                    }
                    index += 1;
                    if index == lines.len() || (fence.is_none() && lines[index].1.trim().is_empty())
                    {
                        break;
                    }
                }
                for run in lines[start..index].chunk_by(|a, b| a.0 == b.0) {
                    let text = run
                        .iter()
                        .map(|(_, line)| *line)
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.blocks.push(DocumentBlock {
                        own: run[0].0,
                        kind: DocumentBlockKind::Markdown(PreparedMarkdown::new(&text, path)),
                    });
                }
            }
        }
        self.key = Some(key);
        self.revision = self.revision.wrapping_add(1);
    }
}
fn enhanced_document(
    ui: &mut egui::Ui,
    cache: &mut CommonMarkCache,
    prepared: &mut DocumentPreview,
    viewer: &mut Option<String>,
) {
    let p = Palette::of(ui);
    prepared.layout.sync(
        prepared.revision,
        prepared.blocks.len(),
        ui.available_width(),
        ui.ctx().pixels_per_point(),
        36.0,
    );
    egui::ScrollArea::vertical()
        .id_salt("prepared-document")
        .content_margin(TEXT_SCROLL_MARGIN)
        .show_viewport(ui, |ui, viewport| {
            let origin = ui.cursor().min;
            ui.set_min_size(egui::vec2(ui.available_width(), prepared.layout.total()));
            for index in prepared.layout.visible(viewport) {
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    let id = egui::Id::unique("document-layout-count");
                    data.insert_temp(id, data.get_temp::<usize>(id).unwrap_or(0) + 1);
                });
                let block = &prepared.blocks[index];
                let mut row = ui.new_child(egui::UiBuilder::new().id_salt(index).max_rect(
                    egui::Rect::from_min_size(
                        origin + egui::vec2(0.0, prepared.layout.offset(index)),
                        egui::vec2(ui.available_width(), f32::INFINITY),
                    ),
                ));
                let ui = &mut row;
                let ink = if block.own { p.text } else { p.muted };
                ui.visuals_mut().override_text_color = Some(ink);
                ui.visuals_mut().widgets.noninteractive.fg_stroke.color = ink;
                ui.visuals_mut().widgets.active.fg_stroke.color = ink;
                match &block.kind {
                    DocumentBlockKind::Heading(level, title) => {
                        if index > 0 {
                            ui.add_space(20.0);
                        }
                        ui.label(
                            RichText::new(title)
                                .font(theme::semibold(if *level == 1 {
                                    theme::SECTION
                                } else {
                                    theme::SUBHEADING
                                }))
                                .color(ink),
                        );
                    }
                    DocumentBlockKind::Item(task, markdown) => {
                        ui.horizontal_top(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 22.0), egui::Sense::hover());
                            note_marker(ui.painter(), rect.center(), *task, ink);
                            markdown_preview(ui, cache, markdown, viewer);
                        });
                    }
                    DocumentBlockKind::Markdown(markdown) => {
                        markdown_preview(ui, cache, markdown, viewer)
                    }
                }
                let measured = row.min_rect().height() + 6.0;
                let old = prepared.layout.offset(index + 1) - prepared.layout.offset(index);
                prepared.layout.set_height(index, measured);
                if (old - measured).abs() >= 0.5 {
                    row.ctx().request_repaint();
                }
            }
        });
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Local => "Локально (только локальные модели)",
        Mode::Hybrid => "Гибрид (разрешён облачный LLM)",
    }
}
fn mode_selector(ui: &mut egui::Ui, mode: &mut Mode) -> bool {
    theme::segmented_full(
        ui,
        mode,
        &[(Mode::Local, "Локально"), (Mode::Hybrid, "Гибрид")],
    )
}
pub(crate) fn model_selector(ui: &mut egui::Ui, id: &str, model: &mut String) {
    crate::theme::select(id)
        .selected_text(crate::config::model_label(model))
        .show_ui(ui, |ui| {
            for name in MODELS {
                crate::theme::select_value(
                    ui,
                    model,
                    name.into(),
                    crate::config::model_label(name),
                );
            }
        });
}

/// The pill is recreated each time it hides, so keep its last position here and
/// on disk instead of letting it jump back to the default corner.
#[derive(Default)]
struct IndicatorPosition {
    spawn: Option<egui::Pos2>,
    current: Option<egui::Pos2>,
}
impl IndicatorPosition {
    fn path() -> Option<std::path::PathBuf> {
        crate::config::Settings::path()
            .ok()
            .map(|path| path.with_file_name("indicator.txt"))
    }
    fn load() -> Self {
        let spawn = Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| parse_position(&text));
        Self {
            spawn,
            current: spawn,
        }
    }
    fn moved(&mut self, position: egui::Pos2) {
        if self
            .current
            .is_some_and(|current| current.distance(position) < 1.0)
        {
            return;
        }
        self.current = Some(position);
        if let Some(path) = Self::path() {
            let _ = crate::config::atomic_write(
                &path,
                &format!("{} {}", position.x.round(), position.y.round()),
            );
        }
    }
    fn closed(&mut self) {
        // Only now: moving the builder position while the pill is open would
        // fight the user's drag.
        self.spawn = self.current;
    }
}
fn parse_position(text: &str) -> Option<egui::Pos2> {
    let mut parts = text.split_whitespace().map(str::parse::<f32>);
    let (Some(Ok(x)), Some(Ok(y)), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    (x.is_finite() && y.is_finite()).then(|| egui::pos2(x, y))
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

fn attachment_strip(ui: &mut egui::Ui, images: &[String], viewer: &mut Option<String>) {
    if images.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for uri in images {
            if ui
                .add(
                    egui::Image::from_uri(uri)
                        .max_size(egui::vec2(100.0, 64.0))
                        .sense(egui::Sense::click()),
                )
                .on_hover_text("Открыть изображение")
                .clicked()
            {
                *viewer = Some(uri.clone());
            }
        }
    });
}

struct PreparedMarkdown {
    markdown: String,
    uris: std::sync::Arc<Vec<Option<String>>>,
    image_text: std::sync::Arc<Vec<String>>,
}
impl PreparedMarkdown {
    fn new(text: &str, path: &std::path::Path) -> Self {
        // CommonMark's image widget has no click handler; use a checked HTML callback.
        let image_text: Vec<_> = notetaker_core::markdown_images(text)
            .into_iter()
            .map(|(range, _)| text[range].to_owned())
            .collect();
        let (markdown, uris) = preview_markdown(text, path);
        Self {
            markdown,
            uris: std::sync::Arc::new(uris),
            image_text: std::sync::Arc::new(image_text),
        }
    }
}
fn markdown_preview(
    ui: &mut egui::Ui,
    cache: &mut CommonMarkCache,
    prepared: &PreparedMarkdown,
    viewer: &mut Option<String>,
) {
    let clicked = std::rc::Rc::new(std::cell::Cell::new(None));
    let selected = clicked.clone();
    let image_uris = prepared.uris.clone();
    let image_text = prepared.image_text.clone();
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
        .show(ui, cache, &prepared.markdown);
    if let Some(index) = clicked.get() {
        *viewer = prepared.uris[index].clone();
    }
}

/// Only recognize ATX headings, not inline hashes or a thematic break.
fn note_heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&level).then_some(())?;
    line.get(level..)?
        .strip_prefix(' ')
        .map(|title| (level, title))
}

/// Prefix slices refer to the original source, so text and cursor indices agree.
fn note_item(line: &str) -> Option<(&str, Option<bool>, &str)> {
    let body = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))?;
    for (marker, checked) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
        if let Some(body) = body.strip_prefix(marker) {
            return Some((&line[..2 + marker.len()], Some(checked), body));
        }
    }
    Some((&line[..2], None, body))
}

fn note_marker(
    painter: &egui::Painter,
    center: egui::Pos2,
    task: Option<bool>,
    ink: egui::Color32,
) {
    if let Some(checked) = task {
        painter.circle_stroke(center, 5.0, egui::Stroke::new(1.0, ink));
        if checked {
            painter.line_segment(
                [
                    center + egui::vec2(-2.5, 0.0),
                    center + egui::vec2(-0.5, 2.0),
                ],
                egui::Stroke::new(1.0, ink),
            );
            painter.line_segment(
                [
                    center + egui::vec2(-0.5, 2.0),
                    center + egui::vec2(3.0, -2.0),
                ],
                egui::Stroke::new(1.0, ink),
            );
        }
    } else {
        painter.circle_filled(center, 2.0, ink);
    }
}

fn note_layout(text: &str, p: Palette, width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    let body = egui::TextFormat {
        font_id: egui::FontId::proportional(theme::BODY),
        color: p.text,
        line_height: Some(24.0),
        ..Default::default()
    };
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
        }
        if !fenced && let Some((level, title)) = note_heading(line) {
            let prefix = &line[..line.len() - title.len()];
            job.append(
                prefix,
                0.0,
                egui::TextFormat {
                    font_id: egui::FontId::proportional(theme::SMALL),
                    color: p.muted.gamma_multiply(0.2),
                    line_height: Some(30.0),
                    ..Default::default()
                },
            );
            job.append(
                title,
                0.0,
                egui::TextFormat {
                    font_id: theme::semibold(if level == 1 {
                        theme::SECTION
                    } else {
                        theme::SUBHEADING
                    }),
                    line_height: Some(30.0),
                    ..body.clone()
                },
            );
        } else if !fenced && let Some((prefix, _, text)) = note_item(line) {
            // Keep every source character. Decorations are drawn over the reserved
            // marker span after layout, never substituted into the text buffer.
            job.append(
                prefix,
                0.0,
                egui::TextFormat {
                    color: egui::Color32::TRANSPARENT,
                    ..body.clone()
                },
            );
            job.append(
                text,
                if prefix.len() == 2 { 12.0 } else { 0.0 },
                body.clone(),
            );
        } else {
            job.append(line, 0.0, body.clone());
        }
    }
    job
}

#[derive(Default)]
struct NotesCache {
    key: Option<(u64, u32, u32, bool)>,
    galley: Option<std::sync::Arc<egui::Galley>>,
    markers: Vec<(egui::Pos2, Option<bool>)>,
    attachment_key: Option<(Instant, notetaker_core::Revision)>,
    path: std::path::PathBuf,
    attachments: Vec<String>,
}
impl NotesCache {
    fn layout(&mut self, ui: &egui::Ui, text: &str, width: f32) -> std::sync::Arc<egui::Galley> {
        let key = (
            crate::virtual_rows::hash(text),
            width.to_bits(),
            ui.ctx().pixels_per_point().to_bits(),
            ui.visuals().dark_mode,
        );
        if self.key != Some(key) {
            let galley =
                ui.fonts_mut(|fonts| fonts.layout_job(note_layout(text, Palette::of(ui), width)));
            self.markers.clear();
            let mut offset = 0;
            let mut fenced = false;
            for line in text.split_inclusive('\n') {
                if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
                    fenced = !fenced;
                }
                if !fenced && let Some((_, task, _)) = note_item(line) {
                    let rect = galley.pos_from_cursor(egui::text::CCursor::new(offset));
                    self.markers
                        .push((egui::pos2(rect.left() + 7.0, rect.center().y), task));
                }
                offset += line.chars().count();
            }
            self.galley = Some(galley);
            self.key = Some(key);
        }
        self.galley.as_ref().expect("prepared layout").clone()
    }
}

fn notes_editor(
    ui: &mut egui::Ui,
    document: &mut Document,
    cache: &mut NotesCache,
    path: &std::path::Path,
    viewer: &mut Option<String>,
) -> Vec<ImageInput> {
    let attachment_key = (document.changed_at, document.data.revisions.notes);
    if cache.attachment_key != Some(attachment_key) || cache.path != path {
        cache.path = path.to_owned();
        cache.attachments = notetaker_core::attachment_references(&document.data.notes)
            .iter()
            .filter_map(|relative| image_uri(path, relative))
            .collect();
        cache.attachment_key = Some(attachment_key);
    }
    attachment_strip(ui, &cache.attachments, viewer);
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
            let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, width: f32| {
                cache.layout(ui, buffer.as_str(), width)
            };
            let output = egui::TextEdit::multiline(&mut document.data.notes)
                .id(id)
                .hint_text("Пишите важное здесь…")
                .layouter(&mut layouter)
                .font(egui::TextStyle::Body)
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .desired_rows(24)
                .show(ui);
            let painter = ui
                .painter()
                .with_clip_rect(output.response.rect.intersect(ui.clip_rect()));
            let top = painter.clip_rect().top() - output.galley_pos.y;
            let bottom = painter.clip_rect().bottom() - output.galley_pos.y;
            let first = cache.markers.partition_point(|(pos, _)| pos.y < top);
            for (pos, task) in cache.markers[first..]
                .iter()
                .take_while(|(pos, _)| pos.y <= bottom)
            {
                note_marker(
                    &painter,
                    *pos + output.galley_pos.to_vec2(),
                    *task,
                    Palette::of(ui).muted,
                );
            }
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
#[derive(Default)]
struct TranscriptCache {
    key: Option<(Instant, notetaker_core::Revision, usize, bool)>,
    path: std::path::PathBuf,
    revision: u64,
    rows: crate::virtual_rows::VirtualRows,
    indices: std::collections::HashMap<uuid::Uuid, usize>,
    edit_ids: std::collections::HashMap<egui::Id, usize>,
    times: Vec<String>,
    metrics: Vec<(uuid::Uuid, u64)>,
    menu_row: Option<usize>,
    geometry: Option<(u32, u32)>,
    anchor: Option<(uuid::Uuid, f32)>,
    scroll_override: Option<f32>,
    jump: Option<uuid::Uuid>,
}
fn transcript_text_id(path: &std::path::Path, id: uuid::Uuid) -> egui::Id {
    egui::Id::unique(("transcript-text", path, id))
}
impl TranscriptCache {
    fn sync(
        &mut self,
        document: &Document,
        path: &std::path::Path,
        width: f32,
        zoom: f32,
        live: bool,
    ) {
        let key = (
            document.changed_at,
            document.data.revisions.transcript,
            document.data.transcript.segments.len(),
            live,
        );
        let changed = self.key != Some(key) || self.path != path;
        let geometry = (width.to_bits(), zoom.to_bits());
        let resized = self.geometry.is_some_and(|old| old != geometry);
        let saved_heights: std::collections::HashMap<_, _> = if changed && !resized {
            self.metrics
                .iter()
                .enumerate()
                .map(|(index, key)| (*key, self.rows.height(index)))
                .collect()
        } else {
            Default::default()
        };
        if self.key != Some(key) || self.path != path {
            self.path = path.to_owned();
            self.revision = self.revision.wrapping_add(1);
            self.indices.clear();
            self.edit_ids.clear();
            self.times.clear();
            self.metrics.clear();
            for (index, segment) in document.data.transcript.segments.iter().enumerate() {
                self.metrics.push((
                    segment.id,
                    crate::virtual_rows::hash((
                        &segment.text,
                        document.data.transcript.speaker_name(&segment.speaker.0),
                        live,
                    )),
                ));
                self.indices.insert(segment.id, index);
                self.edit_ids
                    .insert(transcript_text_id(path, segment.id), index);
                self.times.push(format!(
                    "{}{}",
                    timestamp(segment.start_ms),
                    if segment.edited {
                        " · исправлено"
                    } else {
                        ""
                    }
                ));
            }
            self.menu_row = None;
            self.key = Some(key);
        }
        self.rows
            .sync(self.revision, self.times.len(), width, zoom, 80.0);
        if changed {
            for (index, metric) in self.metrics.iter().enumerate() {
                self.rows
                    .set_height(index, saved_heights.get(metric).copied().unwrap_or(80.0));
            }
        }
        if (changed || resized)
            && let Some((id, fraction)) = self.anchor
            && let Some(&index) = self.indices.get(&id)
        {
            let height = self.rows.offset(index + 1) - self.rows.offset(index);
            self.scroll_override = Some(self.rows.offset(index) + fraction * height);
        }
        self.geometry = Some(geometry);
    }
}

fn transcript_view(
    ui: &mut egui::Ui,
    cache: &mut TranscriptCache,
    document: &mut Document,
    live: bool,
    voices: Option<&notetaker_core::VoiceLibrary>,
    editable: bool,
    global_allowed: bool,
) -> Option<notetaker_core::SpeakerEdit> {
    let document_path = document.data.meeting.path().to_owned();
    let path = document_path.as_path();
    let p = Palette::of(ui);
    let mut edits = Vec::new();
    let mut action = None;
    let timeline = crate::speakers::timeline(document);
    if !live {
        ui.add_enabled_ui(editable, |ui| {
            action = crate::speakers::controls(
                ui,
                &document.data.transcript,
                &document.data.meta,
                &mut document.speaker_ui,
                &timeline.timeline,
            );
        });
    }
    let mut selected = None;
    let mut scrolled = false;
    cache.sync(
        document,
        path,
        ui.available_width(),
        ui.ctx().pixels_per_point(),
        live,
    );
    if cache.jump.is_some()
        && ui.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::MouseWheel { .. }))
                || input.pointer.any_pressed()
        })
    {
        document.speaker_ui.scroll_to = None;
    }
    cache.jump = document.speaker_ui.scroll_to;
    let target = document
        .speaker_ui
        .scroll_to
        .and_then(|id| cache.indices.get(&id).copied());
    let focused = ui
        .memory(|mem| mem.focused())
        .and_then(|id| cache.edit_ids.get(&id).copied());
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("transcript", path))
        .content_margin(TEXT_SCROLL_MARGIN)
        .stick_to_bottom(live);
    if let Some(offset) = cache.scroll_override.take()
        && !live
        && target.is_none()
    {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    let _viewport = scroll.show_viewport(ui, |ui, viewport| {
        let origin = ui.cursor().min;
        let anchor = cache.rows.row_at(viewport.top());
        let anchor_top = cache.rows.offset(anchor);
        let anchor_height = cache.rows.offset(anchor + 1) - anchor_top;
        cache.anchor = document.data.transcript.segments.get(anchor).map(|s| {
            (
                s.id,
                ((viewport.top() - anchor_top) / anchor_height.max(1.0)).clamp(0.0, 1.0),
            )
        });
        let mut visible: Vec<_> = cache.rows.visible(viewport).collect();
        if let Some(index) = focused
            && !visible.contains(&index)
        {
            visible.push(index);
        }
        if let Some(index) = target
            && !visible.contains(&index)
        {
            visible.push(index);
        }
        if egui::Popup::is_any_open(ui.ctx())
            && let Some(index) = cache.menu_row
            && !visible.contains(&index)
        {
            visible.push(index);
        }
        ui.set_min_size(egui::vec2(ui.available_width(), cache.rows.total()));
        for index in visible {
            let segment = &document.data.transcript.segments[index];
            let mut row_ui = ui.new_child(egui::UiBuilder::new().id_salt(segment.id).max_rect(
                egui::Rect::from_min_size(
                    origin + egui::vec2(0.0, cache.rows.offset(index)),
                    egui::vec2(ui.available_width(), f32::INFINITY),
                ),
            ));
            let ui = &mut row_ui;
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                let id = egui::Id::unique("transcript-layout-count");
                let count = data.get_temp::<usize>(id).unwrap_or(0);
                data.insert_temp(id, count + 1);
            });
            let me = segment.speaker.0 == "me"
                || document.data.transcript.self_speaker.as_ref() == Some(&segment.speaker.0);
            let row_id = ui.scope_id().with(("transcript-row", segment.id));
            let row_hover = ui
                .ctx()
                .data(|data| data.get_temp::<egui::Rect>(row_id))
                .is_some_and(|rect| ui.rect_contains_pointer(rect));
            let response = ui
                .push_id(segment.id, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: 10,
                            right: 0,
                            top: 0,
                            bottom: 0,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let name =
                                    document.data.transcript.speaker_name(&segment.speaker.0);
                                if ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(if me { "Я" } else { name })
                                                .font(theme::medium(13.0))
                                                .color(p.text),
                                        )
                                        .sense(egui::Sense::click()),
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
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let label = &cache.times[index];
                                if ui
                                    .add_enabled(
                                        !live && editable,
                                        egui::Label::new(RichText::new(label).size(12.0).color(
                                            if document.speaker_ui.selected.contains(&segment.id) {
                                                p.accent
                                            } else {
                                                p.tertiary
                                            },
                                        ))
                                        .sense(egui::Sense::click()),
                                    )
                                    .clicked()
                                {
                                    selected = Some(index);
                                }
                                if !live && editable {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if let Some(rename) = crate::speakers::rename_menu(
                                                ui,
                                                &document.data.transcript,
                                                &document.data.meta,
                                                &segment.speaker.0,
                                                voices,
                                                global_allowed,
                                                row_hover,
                                            ) {
                                                action = Some(rename);
                                            }
                                        },
                                    );
                                }
                            });
                            egui::Frame::new().inner_margin(0).show(ui, |ui| {
                                ui.set_max_width(ui.available_width());
                                ui.spacing_mut().interact_size.y = 0.0;
                                if live {
                                    ui.add(egui::Label::new(&segment.text).wrap());
                                } else {
                                    let mut text = segment.text.clone();
                                    let response = ui.add_enabled(
                                        editable,
                                        egui::TextEdit::multiline(&mut text)
                                            .id(transcript_text_id(path, segment.id))
                                            .margin(0)
                                            .frame(egui::Frame::NONE)
                                            .desired_width(ui.available_width())
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
                        });
                })
                .response;
            if me {
                ui.painter().line_segment(
                    [
                        response.rect.left_top() + egui::vec2(1.0, 0.0),
                        response.rect.left_bottom() + egui::vec2(1.0, 0.0),
                    ],
                    egui::Stroke::new(2.0, p.accent),
                );
            }
            ui.ctx()
                .data_mut(|data| data.insert_temp(row_id, response.rect));
            if response.contains_pointer() {
                cache.menu_row = Some(index);
            }
            if document.speaker_ui.scroll_to == Some(segment.id) {
                response.scroll_to_me(Some(egui::Align::Center));
                let desired = (response.rect.center().y - origin.y - viewport.height() / 2.0)
                    .clamp(0.0, (cache.rows.total() - viewport.height()).max(0.0));
                scrolled = (viewport.top() - desired).abs() < 1.0;
            }
            let previous = cache.rows.offset(index + 1) - cache.rows.offset(index);
            let measured = response.rect.height() + 16.0;
            cache.rows.set_height(index, measured);
            if (measured - previous).abs() >= 0.5 {
                ui.ctx().request_repaint();
            }
        }
        let adjustment = cache.rows.offset(anchor) - anchor_top;
        if adjustment.abs() >= 0.5 && !live && target.is_none() {
            cache.scroll_override = Some((viewport.top() + adjustment).max(0.0));
        }
        if document.data.transcript.segments.is_empty() {
            ui.weak(if live {
                "Текст появится здесь"
            } else {
                "У этой встречи нет транскрипта"
            });
        }
    });
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::unique("transcript-viewport-width"),
            _viewport.inner_rect.width(),
        )
    });
    if scrolled {
        document.speaker_ui.scroll_to = None;
        cache.jump = None;
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
                        egui::vec2(240.0, 64.0),
                    )),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(root, |ui| {
                            result =
                                super::recording_pill(ui, std::time::Duration::from_secs(48), 0.3);
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
        crate::theme::install(&ctx);
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
                    let imported = notes_editor(
                        ui,
                        &mut document,
                        &mut NotesCache::default(),
                        &path,
                        &mut viewer,
                    );
                    assert_eq!(imported.len(), usize::from(focused));
                });
            });
            output.drop_without_applying_deltas();
        }
        assert_eq!(document.data.notes, "Текст");
    }

    #[test]
    fn live_note_layout_preserves_unicode_cursor_and_selection() {
        use eframe::egui::{
            self,
            text::{CCursor, CCursorRange},
        };
        let source = "## Решения\n- Кириллица и emoji 🦀 с переносом длинной строки\n* [ ] Анна: проверить GPU\n";
        for dark in [false, true] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            let p = crate::theme::Palette::for_dark(dark);
            let mut text = source.to_owned();
            let id = egui::Id::unique("styled-note-test");
            let start = source.find("Кириллица").unwrap();
            let start = source[..start].chars().count();
            let end = start + "Кириллица".chars().count();
            let first = ctx.run_ui(egui::RawInput::default(), |ui| {
                let job = super::note_layout(source, p, 170.0);
                assert_eq!(job.text, source);
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                for index in [start, end, source.chars().count() - 1] {
                    let rect = galley.pos_from_cursor(CCursor::new(index));
                    assert_eq!(
                        galley.cursor_from_pos(rect.center().to_vec2()).index,
                        index.into()
                    );
                }
                let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
                state.cursor.set_char_range(Some(CCursorRange::two(
                    CCursor::new(start),
                    CCursor::new(end),
                )));
                state.store(&ctx, id);
                ui.memory_mut(|memory| memory.request_focus(id));
            });
            first.drop_without_applying_deltas();
            let second = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Text("Текст".into())],
                    ..Default::default()
                },
                |ui| {
                    let mut layouter =
                        |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, width: f32| {
                            ui.fonts_mut(|fonts| {
                                fonts.layout_job(super::note_layout(buffer.as_str(), p, width))
                            })
                        };
                    egui::TextEdit::multiline(&mut text)
                        .id(id)
                        .desired_width(170.0)
                        .layouter(&mut layouter)
                        .show(ui);
                },
            );
            second.drop_without_applying_deltas();
            assert_eq!(text, source.replacen("Кириллица", "Текст", 1));
        }
    }

    #[test]
    fn indicator_position_roundtrips_and_rejects_garbage() {
        assert_eq!(parse_position("120 -40"), Some(egui::pos2(120.0, -40.0)));
        assert_eq!(parse_position("1 2 3"), None);
        assert_eq!(parse_position("x 2"), None);
        assert_eq!(parse_position("NaN 2"), None);
        let mut position = IndicatorPosition {
            spawn: None,
            current: Some(egui::pos2(5.0, 6.0)),
        };
        assert_eq!(position.spawn, None);
        position.closed();
        assert_eq!(position.spawn, Some(egui::pos2(5.0, 6.0)));
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

impl Toast {
    fn tick(&mut self, now: Instant, focused: bool) {
        // Never charge a long hidden/minimized interval to the next focused frame.
        let elapsed = now.saturating_duration_since(self.last_tick);
        if focused && self.was_focused && elapsed <= Duration::from_secs(1) {
            self.focused_for += elapsed;
        }
        self.was_focused = focused;
        self.last_tick = now;
    }
    fn expired(&self, now: Instant) -> bool {
        match self.level {
            ToastLevel::Error => false,
            ToastLevel::Warning => self.focused_for >= Duration::from_secs(6),
            ToastLevel::Info | ToastLevel::Success => {
                now.saturating_duration_since(self.at) >= Duration::from_secs(6)
            }
        }
    }
}

fn push_toast(queue: &mut VecDeque<Toast>, toast: Toast) {
    if let Some(existing) = queue
        .iter_mut()
        .find(|t| t.level == toast.level && t.text == toast.text)
    {
        existing.count = existing.count.saturating_add(toast.count);
        existing.retry_settings |= toast.retry_settings;
        if toast.path.is_some() {
            existing.path = toast.path;
        }
        return;
    }
    queue.push_back(toast);
    while queue.len() > 5 {
        let oldest = queue
            .iter()
            .position(|t| t.level != ToastLevel::Error)
            .unwrap_or(0);
        queue.remove(oldest);
    }
}

#[cfg(test)]
mod toast_lifetime_tests {
    use super::*;
    fn toast(now: Instant, level: ToastLevel) -> Toast {
        Toast {
            at: now,
            text: "notice".into(),
            path: None,
            retry_settings: false,
            level,
            count: 1,
            focused_for: Duration::ZERO,
            last_tick: now,
            was_focused: false,
        }
    }
    #[test]
    fn warning_waits_for_six_seconds_of_focus_and_error_never_times_out() {
        let start = Instant::now();
        let mut warning = toast(start, ToastLevel::Warning);
        warning.tick(start + Duration::from_secs(60), false);
        assert!(!warning.expired(start + Duration::from_secs(60)));
        warning.tick(start + Duration::from_secs(120), true);
        assert_eq!(warning.focused_for, Duration::ZERO);
        for step in 1..=11 {
            warning.tick(
                start + Duration::from_secs(120) + Duration::from_millis(step * 500),
                true,
            );
        }
        assert!(!warning.expired(start + Duration::from_secs(126)));
        warning.tick(start + Duration::from_secs(126), true);
        assert!(warning.expired(start + Duration::from_secs(126)));
        let error = toast(start, ToastLevel::Error);
        assert!(!error.expired(start + Duration::from_secs(3600)));
    }
    #[test]
    fn a_gap_without_frames_does_not_count_as_focused_time() {
        let start = Instant::now();
        let mut warning = toast(start, ToastLevel::Warning);
        warning.tick(start, true);
        warning.tick(start + Duration::from_secs(60), true);
        assert_eq!(warning.focused_for, Duration::ZERO);
    }
    #[test]
    fn success_keeps_its_original_six_second_timeout() {
        let start = Instant::now();
        let success = toast(start, ToastLevel::Success);
        assert!(!success.expired(start + Duration::from_secs(5)));
        assert!(success.expired(start + Duration::from_secs(6)));
    }
    #[test]
    fn identical_toasts_collapse_by_level_and_text() {
        let now = Instant::now();
        let mut queue = VecDeque::new();
        for _ in 0..1000 {
            push_toast(&mut queue, toast(now, ToastLevel::Warning));
        }
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].count, 1000);
        assert_eq!(queue[0].focused_for, Duration::ZERO);
        push_toast(&mut queue, toast(now, ToastLevel::Error));
        assert_eq!(queue.len(), 2);
        let mut retry = toast(now, ToastLevel::Error);
        retry.retry_settings = true;
        push_toast(&mut queue, retry);
        assert_eq!(queue[1].count, 2);
        assert!(queue[1].retry_settings);
        assert!(!queue[1].expired(now + Duration::from_secs(60)));
    }
    #[test]
    fn toast_queue_is_bounded_and_evicts_non_errors_before_errors() {
        let now = Instant::now();
        let mut queue = VecDeque::new();
        for index in 0..100 {
            let mut error = toast(now, ToastLevel::Error);
            error.text = format!("error {index}");
            push_toast(&mut queue, error);
            assert!(queue.len() <= 5);
        }
        assert_eq!(queue[0].text, "error 95");
        push_toast(&mut queue, toast(now, ToastLevel::Warning));
        assert_eq!(queue.len(), 5);
        assert_eq!(queue[0].text, "error 95");
        assert!(queue.iter().all(|t| t.level == ToastLevel::Error));
        queue.pop_front();
        push_toast(&mut queue, toast(now, ToastLevel::Warning));
        assert_eq!(queue[0].text, "error 96");
        for index in 0..100 {
            let mut warning = toast(now, ToastLevel::Warning);
            warning.text = format!("warning {index}");
            push_toast(&mut queue, warning);
            assert_eq!(queue.len(), 5);
            assert_eq!(queue[0].text, "error 96");
        }
        assert_eq!(queue[4].text, "warning 99");
    }
}
