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
    undo_title: Option<crate::state::TitleUndo>,
    undo_ktalk: Option<crate::ktalk::Undo>,
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

#[derive(Default)]
struct MeetingLayout {
    compact: bool,
    hide_prompts: bool,
    scroll_key: Option<(std::path::PathBuf, Tab, bool)>,
    metadata_visible: bool,
    scroll_y: f32,
}
impl MeetingLayout {
    fn update(&mut self, height: f32) {
        if height < 640.0 {
            self.compact = true;
        } else if height > 700.0 {
            self.compact = false;
        }
        if !self.compact {
            self.hide_prompts = false;
        }
    }
    fn metadata_at_top(
        &mut self,
        path: &std::path::Path,
        tab: Tab,
        preview: bool,
        offset: f32,
    ) -> bool {
        let key = (path.to_owned(), tab, preview);
        if self.scroll_key.as_ref() != Some(&key) {
            self.scroll_key = Some(key);
            self.metadata_visible = true;
        } else if offset > 32.0 {
            self.metadata_visible = false;
        } else if offset <= 0.5 {
            self.metadata_visible = true;
        }
        self.metadata_visible
    }
    fn document_height(&mut self, height: f32, minimum: f32) {
        if self.compact && height < minimum {
            self.hide_prompts = true;
        } else if height > minimum + 48.0 {
            self.hide_prompts = false;
        }
    }
}

fn meeting_audio_status(document: &Document, recording: bool) -> String {
    if recording {
        return "Аудио записывается".into();
    }
    let meeting = &document.data.meeting;
    crate::wording::audio_status(
        meeting.mic_audio_path().is_file() || meeting.system_audio_path().is_file(),
        document.data.meta.audio_expires_at,
    )
}

enum MeetingEntry {
    Header(String),
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
    calendar_rows: crate::calendar_ui::HomeRows,
    tasks_cache: TasksCache,
    meeting_layout: MeetingLayout,
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
    main_hidden: bool,
    main_focused: bool,
    /// Where the recording pill is now, and where the next one opens.
    indicator: IndicatorPosition,
    home: bool,
    applied_zoom: Option<f32>,
    chat_page: bool,
    ask_text: String,
    recipes: crate::chat::Recipes,
    focus_chat: bool,
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
        Self::with_state(AppState::new())
    }
    fn with_state(state: AppState) -> Self {
        Self {
            state,
            people: crate::people::Panel::default(),
            organization: Default::default(),
            trackers: Default::default(),
            cache: CommonMarkCache::default(),
            transcript_cache: Default::default(),
            document_preview: Default::default(),
            notes_cache: Default::default(),
            meetings_cache: Default::default(),
            calendar_rows: Default::default(),
            tasks_cache: Default::default(),
            meeting_layout: Default::default(),
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
            main_hidden: false,
            main_focused: false,
            home: true,
            applied_zoom: None,
            chat_page: false,
            ask_text: String::new(),
            recipes: Default::default(),
            focus_chat: false,
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
        self.recording_reason().is_none()
    }
    fn recording_reason(&self) -> Option<std::borrow::Cow<'static, str>> {
        self.recording_block()
            .map(std::borrow::Cow::Borrowed)
            .or_else(|| {
                (!self.state.can_prepare_recording()).then(|| {
                    self.state
                        .busy_reason()
                        .unwrap_or_else(|| "Дождитесь завершения текущей операции".into())
                        .into()
                })
            })
    }
    fn recording_block(&self) -> Option<&'static str> {
        if let Some(reason) = self.state.permissions.blocked() {
            Some(reason)
        } else if !self.state.ready {
            Some("Библиотека загружается")
        } else if !self
            .state
            .quickstart
            .models
            .speech_ready(&self.state.record_model)
        {
            Some("Дождитесь подготовки распознавания")
        } else if self.state.quickstart.probe_busy {
            Some("Дождитесь проверки звука")
        } else if !self.state.can_prepare_recording() {
            None
        } else if !cfg!(any(windows, target_os = "macos", test)) {
            Some("Запись доступна на macOS и Windows")
        } else {
            None
        }
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
                    self.import_picker.is_none()
                        && self.state.can_prepare_import()
                        && self
                            .state
                            .quickstart
                            .models
                            .speech_ready(&self.state.record_model),
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
        if crate::permissions::panel(
            ui,
            &self.state.permissions,
            false,
            self.state.recording.busy(),
        ) {
            self.state.permissions.check_requested = true;
        }
        if !self.quickstart.open
            && (self
                .state
                .quickstart
                .models
                .missing(&self.state.settings.model)
                .is_some()
                || self.state.downloading)
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
        let mut calendar_settings = false;
        let recording_reason = self.recording_reason();
        self.meetings_cache.sync(&self.state, &self.organization);
        let count = self.meetings_cache.count;
        egui::ScrollArea::vertical()
            .id_salt("meetings")
            .show_viewport(ui, |ui, viewport| {
                let content_top = ui.cursor().min.y;
                if !self.state.settings.calendars.is_empty() {
                    let id = ui.make_persistent_id("calendar-expanded");
                    let mut expanded = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
                    let action = crate::calendar_ui::home(
                        ui,
                        crate::calendar_ui::Home {
                            revision: self.state.calendar_revision,
                            events: &self.state.calendar_events,
                            reports: &self.state.calendar_reports,
                            error: self.state.calendar_error.as_deref(),
                            loading: self.state.calendar_reports.is_empty()
                                && self.state.calendar_error.is_none(),
                            now: chrono::Utc::now(),
                            reason: recording_reason.as_deref(),
                        },
                        &mut expanded,
                        &mut self.calendar_rows,
                    );
                    ui.data_mut(|d| d.insert_temp(id, expanded));
                    match action {
                        crate::calendar_ui::HomeAction::Start(event) => start_event = Some(event),
                        crate::calendar_ui::HomeAction::Join(event) => {
                            self.state.join_event(&event)
                        }
                        crate::calendar_ui::HomeAction::Settings => calendar_settings = true,
                        crate::calendar_ui::HomeAction::None => {}
                    }
                }
                ui.spacing_mut().item_spacing.y = 3.0;
                let origin = ui.cursor().min;
                let list_viewport = viewport.translate(egui::vec2(0.0, content_top - origin.y));
                ui.set_min_size(egui::vec2(
                    ui.available_width(),
                    self.meetings_cache.layout.total(),
                ));
                for index in self.meetings_cache.layout.visible(list_viewport) {
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
                        MeetingEntry::Meeting(index, time) => {
                            let row = &self.state.rows[*index];
                            let (response, _) = theme::meeting_row(
                                ui,
                                row.meta.id,
                                time,
                                &row.meta.title,
                                row.meta.folder.as_deref().unwrap_or(""),
                                None,
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
                        empty.then_some((
                            "Новая запись",
                            self.state.can_prepare_recording()
                                && self.state.record_options_pending.is_none(),
                        )),
                    ) {
                        self.state.toggle_recording();
                    }
                }
                if self.state.listing {
                    crate::theme::spinner(ui);
                }
            });
        if calendar_settings {
            self.show_settings = true;
            self.settings_panel.section = crate::settings::Section::Calendars;
        }
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
        // The place decides what the next question is about. The dialog itself
        // changes only when a question is sent, so a look at a cited meeting
        // keeps the answer on the chat page.
        let target = if self.chat_page {
            None
        } else if note {
            self.state
                .selected
                .as_ref()
                .and_then(|p| self.state.documents.get(p))
                .map(|d| (d.data.meeting.clone(), d.data.meta.title.clone()))
        } else {
            None
        };
        let scope_path = if self.chat_page {
            self.state
                .chat
                .scope
                .as_ref()
                .map(|m| m.path().to_path_buf())
        } else {
            target.as_ref().map(|(m, _)| m.path().to_path_buf())
        };
        let fresh = self.state.chat.folder != self.state.folder_filter
            || (!self.chat_page
                && self.state.chat.scope.as_ref().map(|m| m.path()) != scope_path.as_deref());
        let live = self.state.recording.phase == Phase::Recording
            && scope_path.is_some()
            && scope_path.as_deref() == self.state.recording.path.as_deref();
        let available = !self.state.recording.busy() || live;
        if !available {
            self.focus_chat = false;
        }
        let can_ask = self.state.ready
            && !self.state.chat.busy
            && available
            && (live || !self.state.unsaved())
            && !self.state.settings_pending;
        let mut ask = false;
        if available {
            ui.add_enabled_ui(can_ask, |ui| {
                ask |= self
                    .recipes
                    .show(ui, &mut self.ask_text, &self.state.settings.library);
                if (fresh || self.state.chat.messages.is_empty())
                    && !(note && self.meeting_layout.compact && self.meeting_layout.hide_prompts)
                    && !self.state.chat.busy
                    && let Some(prompt) = theme::prompt_chips(
                        ui,
                        crate::chat::suggestions(scope_path.is_some(), live),
                    )
                {
                    self.ask_text = prompt;
                    ask = true;
                }
            });
        }
        let mut stop = false;
        let bar = theme::bar(p).show(ui, |ui| {
            if let Some(status) = self
                .state
                .permissions
                .recording_status(&self.state.recording)
            {
                theme::recording_status(ui, &status.text, status.warning, status.progress);
                if self.state.permissions.recording_help(&self.state.recording) {
                    crate::permissions::open_settings(ui);
                }
                if let Some(next) = self.state.next_meeting(chrono::Utc::now())
                    && crate::calendar_ui::next_meeting(ui, &next)
                {
                    self.state.dismiss_calendar_reminder();
                }
            }
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
                        crate::theme::spinner(ui);
                    }
                    let elapsed = timestamp(self.state.recording.elapsed().as_millis() as u64);
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
                    if !busy {
                        let options =
                            theme::icon_button(ui, theme::icons::CHEVRON_DOWN, "Параметры записи");
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(egui::Id::unique("record-options-rect"), options.rect)
                        });
                        if options.clicked() {
                            self.state.open_record_options();
                        }
                    }
                    if stopping {
                        stop = theme::button(ui, "Стоп", ButtonStyle::Secondary).clicked();
                    } else if !busy
                        && ui
                            .add_enabled(
                                self.state.can_prepare_recording()
                                    && self.state.record_options_pending.is_none(),
                                theme::styled_button(
                                    p,
                                    if self.state.record_options_pending.is_some() {
                                        "Проверяю источник…"
                                    } else {
                                        "Новая запись"
                                    },
                                    ButtonStyle::Primary,
                                )
                                .min_size(egui::vec2(0.0, theme::CTA_HEIGHT)),
                            )
                            .on_disabled_hover_text(
                                self.recording_reason().unwrap_or_default().into_owned(),
                            )
                            .clicked()
                    {
                        self.state.toggle_recording();
                    }
                    if !self.ask_text.trim().is_empty() && !self.ask_text.starts_with('/') {
                        ask |= ui
                            .add_enabled(
                                can_ask,
                                crate::theme::styled_button(
                                    crate::theme::Palette::of(ui),
                                    "Спросить",
                                    crate::theme::ButtonStyle::Secondary,
                                ),
                            )
                            .on_disabled_hover_text("Дождитесь сохранения встречи")
                            .clicked();
                    }
                    if !available {
                        return;
                    }
                    let response = ui.add_sized(
                        [ui.available_width().max(24.0), 32.0],
                        crate::theme::text_field(&mut self.ask_text)
                            .id(egui::Id::unique("chat-input"))
                            .frame(egui::Frame::NONE)
                            .align(egui::Align2::LEFT_CENTER)
                            .hint_text(if busy {
                                "Спросить по текущей записи…".to_owned()
                            } else if note || (self.chat_page && self.state.chat.scope.is_some()) {
                                "Спросить про эту встречу…".to_owned()
                            } else if self.state.folder_filter.is_some() {
                                "Спросить по папке…".to_owned()
                            } else {
                                "Спросить по всем встречам…".to_owned()
                            }),
                    );
                    if self.focus_chat {
                        response.request_focus();
                        self.focus_chat = false;
                    }
                    ask |= !self.ask_text.starts_with('/')
                        && response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
        if ask && !self.ask_text.trim().is_empty() && can_ask {
            // A dialog belongs to one folder, also when asked from the chat page.
            if self.state.chat.folder != self.state.folder_filter {
                self.state.chat.messages.clear();
                self.state.chat.folder = self.state.folder_filter.clone();
            }
            if !self.chat_page {
                let (scope, title) = target.unzip();
                self.state.chat.open(scope, title.unwrap_or_default());
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
        let can_toggle = self.state.can_toggle_tasks();
        let mut send_task = None;
        let mut toggle = None;
        let mut open = None;
        let _layout = ui
            .scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical().id_salt("all_tasks").show_rows(
                    ui,
                    48.0,
                    self.tasks_cache.entries.len().max(1),
                    |ui, range| {
                        if self.tasks_cache.entries.is_empty() && !self.state.tasks_busy {
                            ui.weak("Нет задач по выбранным фильтрам");
                        }
                        for index in range {
                            let Some(entry) = self.tasks_cache.entries.get(index) else {
                                continue;
                            };
                            let task = match entry {
                                TaskEntry::Header(title) => {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), 48.0),
                                        egui::Sense::hover(),
                                    );
                                    let mut header = ui.new_child(
                                        egui::UiBuilder::new()
                                            .id_salt(("task-group", index))
                                            .max_rect(rect),
                                    );
                                    header.add_space(24.0);
                                    header
                                        .add(
                                            egui::Label::new(
                                                RichText::new(title)
                                                    .font(theme::semibold(theme::SMALL))
                                                    .color(Palette::of(ui).tertiary),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(title);
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
                                #[cfg(test)]
                                ui.ctx().data_mut(|data| {
                                    data.insert_temp(
                                        egui::Id::unique((
                                            "task-row-rect",
                                            task.path.as_path(),
                                            task.line,
                                        )),
                                        rect,
                                    )
                                });
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
                                                RichText::new(metadata)
                                                    .size(12.0)
                                                    .color(p.tertiary),
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
                            crate::theme::spinner(ui);
                        }
                    },
                )
            })
            .inner;
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::unique("task-list-model"),
                (_layout.inner_rect, _layout.state.clamped_offset().y),
            )
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
        let compact_header = self.meeting_layout.compact;
        if compact_header {
            ui.spacing_mut().item_spacing.y = 4.0;
        }
        let Some(path) = self.state.selected.clone() else {
            self.home = true;
            return;
        };
        let scroll_y = self.meeting_layout.scroll_y;
        let metadata_visible = self.meeting_layout.metadata_at_top(
            &path,
            self.state.tab,
            self.enhanced_preview,
            scroll_y,
        );
        let folder_enabled = self.organization.meeting_editable(&self.state);
        let skipped_summary = self.state.skipped_summary_notice(&path);
        let prosody_status = self.state.prosody_status(&path);
        let audio_recalculating = self.state.audio_recalculating();
        let can_toggle_tasks = self.state.can_toggle_tasks();
        let readiness = self.state.documents.get(&path).map(|doc| {
            crate::state::summary_provider(
                self.state.llm_configured,
                &self.state.llm,
                doc.data.meta.mode,
            )
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
        });

        let mut open_ai = false;
        let mut ktalk_sync = false;
        let can_enhance = self.state.enhancing.is_none()
            && !self.state.recording.busy()
            && !self.state.unsaved()
            && !self.state.settings_pending;
        let locked =
            self.state.recording.busy() && self.state.recording.path.as_ref() == Some(&path);
        let global_allowed = !self.state.unsaved();
        let speaker_busy = self.state.documents.values().any(|d| d.speaker_ui.busy);
        let ktalk_busy = self.state.ktalk.editing(&path);
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
            crate::theme::spinner(ui);
            return;
        };
        let start = document.data.meta.started_at.with_timezone(&chrono::Local);
        let participants = document
            .data
            .meta
            .calendar_event
            .as_ref()
            .map(|e| e.attendees.len())
            .unwrap_or_else(|| crate::speakers::timeline(document).timeline.speakers.len());
        let metadata = format!(
            "{} · {}\nПапка: {}",
            start.format("%d.%m.%Y · %H:%M"),
            crate::wording::count(participants as u64, crate::wording::PARTICIPANTS),
            document.data.meta.folder.as_deref().unwrap_or("Без папки")
        );
        let mut speaker_edit = None;
        let mut rediarize = None;
        let unavailable = if self.state.recording.busy() {
            Some("Доступно после завершения записи")
        } else if !document.data.system_audio {
            Some("Аудио этой встречи не сохранено, пересчитать собеседников нельзя")
        } else if document.speaker_ui.recalculating {
            Some("Пересчитываю собеседников…")
        } else if audio_recalculating
            || document.saving
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
            && !ktalk_busy
            && !document.saving
            && !speaker_busy
            && document.attachments_pending == 0
            && self.state.enhancing.is_none();
        let previous_template = document.template.clone();
        let mut document_toggle = None;
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
            if !compact_header && theme::button(ui, "Встречи", ButtonStyle::Ghost).clicked()
            {
                self.home = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let menu_response =
                    theme::icon_button(ui, theme::icons::ELLIPSIS, "Действия встречи");
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::unique("meeting-menu-rect"), menu_response.rect)
                });
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
                                // Always in the compact menu: the row beside the tabs
                                // can run out of width before it reaches the folder.
                                if compact_header {
                                    ui.add_enabled_ui(false, |ui| {
                                        theme::menu_item(
                                            ui,
                                            theme::icons::CALENDAR,
                                            &start.format("%d.%m.%Y · %H:%M").to_string(),
                                            false,
                                            false,
                                        );
                                        theme::menu_item(
                                            ui,
                                            theme::icons::USERS,
                                            &crate::wording::count(
                                                participants as u64,
                                                crate::wording::PARTICIPANTS,
                                            ),
                                            false,
                                            false,
                                        );
                                    });
                                    self.organization.meeting(
                                        ui,
                                        &document.data.meeting,
                                        &document.data.meta,
                                        folder_enabled,
                                        true,
                                    );
                                    ui.separator();
                                }
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
                                if self.state.tab == Tab::Transcript
                                    && ui
                                        .add_enabled_ui(editable && !self.state.ktalk.busy, |ui| {
                                            theme::menu_item(ui, "", "Имена из Толка", false, false)
                                        })
                                        .inner
                                        .clicked()
                                {
                                    ktalk_sync = true;
                                    ui.close();
                                }
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
                                            template_name(&self.state.settings.library, name),
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
                                ui.separator();
                                ui.weak(meeting_audio_status(document, locked));
                                if compact_header && self.state.tab == Tab::Transcript {
                                    ui.separator();
                                    let (count, edit) = crate::speakers::count_control(
                                        ui,
                                        document,
                                        unavailable,
                                        editable,
                                    );
                                    rediarize = count;
                                    speaker_edit = edit;
                                }
                            });
                    });
                // Right-to-left layout: reverse so pinned buttons read in menu order.
                for &item in pinned.iter().rev().filter(|_| !compact_header) {
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
                if compact_header {
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), theme::COMPACT_HEIGHT),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&document.data.meta.title)
                                        .font(theme::semibold(theme::SUBHEADING)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(format!("{}\n{}", document.data.meta.title, metadata));
                        },
                    );
                }
            });
        });
        if let Some(status) = prosody_status {
            match status {
                Ok(()) => {
                    ui.weak("Пересчитываю речевые показатели…");
                }
                Err(error) => {
                    ui.small(format!(
                        "Не удалось пересчитать речевые показатели. {error}"
                    ));
                }
            }
        }
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
        let compact = compact_header;
        if !compact {
            ui.add_space(24.0);
            let mut title = egui::text::LayoutJob::simple(
                document.data.meta.title.clone(),
                theme::semibold(theme::TITLE),
                Palette::of(ui).text,
                ui.available_width(),
            );
            title.wrap.max_rows = 2;
            ui.label(title).on_hover_text(&document.data.meta.title);
            ui.add_space(12.0);
        }
        if document.template != previous_template {
            document.template_pending = !document.data.enhanced.trim().is_empty();
        }
        if !compact {
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
                    RichText::new(crate::wording::count(
                        participants as u64,
                        crate::wording::PARTICIPANTS,
                    ))
                    .small()
                    .color(Palette::of(ui).muted),
                );
                // Folder assignment keeps its existing atomic save and sync-conflict handling.
                self.organization.meeting(
                    ui,
                    &document.data.meeting,
                    &document.data.meta,
                    folder_enabled,
                    false,
                );
            });
        }
        ui.add_space(if compact { 4.0 } else { 16.0 });
        let Some(document) = self.state.documents.get_mut(&path) else {
            return;
        };
        let previous_template = document.template.clone();
        ui.horizontal_wrapped(|ui| {
            theme::segmented(
                ui,
                &mut self.state.tab,
                &[
                    (Tab::Enhanced, "Конспект"),
                    (Tab::Transcript, "Транскрипт"),
                    (Tab::Notes, "Заметки"),
                ],
            );
            if compact && metadata_visible {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(start.format("%d.%m · %H:%M").to_string())
                                .small()
                                .color(Palette::of(ui).muted),
                        )
                        .truncate(),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(crate::wording::count(
                                participants as u64,
                                crate::wording::PARTICIPANTS,
                            ))
                            .small()
                            .color(Palette::of(ui).muted),
                        )
                        .truncate(),
                    );
                    self.organization.meeting(
                        ui,
                        &document.data.meeting,
                        &document.data.meta,
                        folder_enabled,
                        false,
                    );
                });
            }
            if !compact && self.state.tab == Tab::Enhanced {
                ui.add_enabled_ui(can_enhance, |ui| {
                    theme::select("document-template")
                        .width(180.0)
                        .selected_text(template_value(
                            ui,
                            &template_name(&self.state.settings.library, &document.template),
                        ))
                        .show_ui(ui, |ui| {
                            for name in &self.state.templates {
                                theme::select_value(
                                    ui,
                                    &mut document.template,
                                    name.clone(),
                                    template_name(&self.state.settings.library, name),
                                );
                            }
                        });
                });
            }
        });
        if document.template != previous_template {
            document.template_pending = !document.data.enhanced.trim().is_empty();
        }
        if self.state.tab == Tab::Enhanced
            && document.template_pending
            && document.generation.is_none()
        {
            theme::callout(ui, theme::icons::FILE_TEXT, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Конспект создан по другому шаблону");
                    match readiness.as_ref().expect("open document") {
                        Ok(()) => {
                            enhance |= ui
                                .add_enabled(
                                    can_enhance,
                                    theme::styled_button(
                                        Palette::of(ui),
                                        if ui.ctx().content_rect().width() < 900.0 {
                                            "Пересоздать".into()
                                        } else {
                                            format!(
                                                "Пересоздать по шаблону «{}»",
                                                template_name(
                                                    &self.state.settings.library,
                                                    &document.template
                                                )
                                            )
                                        }
                                        .as_str(),
                                        ButtonStyle::Primary,
                                    ),
                                )
                                .clicked();
                        }
                        Err(reason) => {
                            ui.weak(reason);
                        }
                    }
                    if theme::button(ui, "Не сейчас", ButtonStyle::Ghost).clicked() {
                        document.template_pending = false;
                    }
                });
            });
        }
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
        if ktalk_sync {
            self.state.ktalk_manual(&path, None);
        }
        if self.state.tab == Tab::Transcript {
            crate::ktalk::transcript_ui(ui, &mut self.state, &path);
        }
        self.organization.meeting_notices(ui, &self.state);
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
        if document.generation.is_none()
            && let Some(notice) = document.summary_stale_notice()
        {
            theme::callout(ui, theme::icons::INFO, |ui| {
                ui.horizontal(|ui| {
                    ui.weak(notice);
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
                crate::theme::spinner(ui);
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
        if self.state.tab == Tab::Enhanced
            && !locked
            && let Some(notice) = &skipped_summary
        {
            let (settings, create) = summary_skip_callout(ui, notice, can_enhance);
            open_ai |= settings;
            enhance |= create;
        }
        let mut images = Vec::new();
        let minimum = if self.state.tab == Tab::Transcript {
            self.transcript_cache
                .rows
                .offset(document.data.transcript.segments.len().min(3))
                .max(96.0)
        } else {
            96.0
        };
        self.meeting_layout
            .document_height(ui.available_height(), minimum);
        self.transcript_cache.compact = compact;
        if self.state.tab == Tab::Transcript {
            if !compact {
                let (count, edit) =
                    crate::speakers::count_control(ui, document, unavailable, editable);
                rediarize = count;
                speaker_edit = edit;
            }
            speaker_edit = transcript_view(
                ui,
                &mut self.transcript_cache,
                document,
                locked,
                self.state
                    .settings
                    .voice_library
                    .then_some(&self.state.voices),
                editable || locked && self.state.recording.phase == Phase::Recording,
                global_allowed && !locked,
            )
            .or(speaker_edit);
            self.meeting_layout.scroll_y = self.transcript_cache.scroll_y;
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
            if document.data.enhanced.is_empty()
                && document.generation.is_none()
                && !locked
                && skipped_summary.is_none()
            {
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
            let (attached, toggle) = notes_editor(
                ui,
                document,
                &mut self.notes_cache,
                &path,
                &mut self.image_viewer,
                can_toggle_tasks,
            );
            images = attached;
            document_toggle = toggle;
            self.meeting_layout.scroll_y = self.notes_cache.scroll_y;
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
            let scroll = egui::ScrollArea::vertical()
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
            self.meeting_layout.scroll_y = scroll.state.clamped_offset().y;
        } else {
            self.document_preview.prepare(document, &path);
            document_toggle = ui
                .push_id(&path, |ui| {
                    enhanced_document(
                        ui,
                        &mut self.cache,
                        &mut self.document_preview,
                        &mut self.image_viewer,
                        can_toggle_tasks,
                    )
                })
                .inner;
            self.meeting_layout.scroll_y = self.document_preview.scroll_y;
        }
        if let Some((notes, task, completed)) = document_toggle {
            self.state.toggle_document_task(task, completed, notes);
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
        if open_ai {
            self.show_settings = true;
            self.settings_panel.section = crate::settings::Section::Ai;
        }
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
        if self.state.error.as_deref() == Some(crate::permissions::MICROPHONE_DENIED)
            && self.state.permissions.blocked().is_none()
        {
            self.state.error = None;
        }
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
                    undo_title: None,
                    undo_ktalk: None,
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
        while let Some((undo, segments)) = self.state.ktalk.successes.pop_front() {
            self.toasts.retain(|toast| {
                toast
                    .undo_ktalk
                    .as_ref()
                    .is_none_or(|old| old.sync.meeting != undo.sync.meeting)
            });
            push_toast(
                &mut self.toasts,
                Toast {
                    undo_title: None,
                    undo_ktalk: Some(undo),
                    at: Instant::now(),
                    text: format!(
                        "Имена из Толка: {}",
                        crate::wording::count(segments as u64, crate::wording::UTTERANCES)
                    ),
                    path: None,
                    retry_settings: false,
                    level: ToastLevel::Success,
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
                    undo_title: None,
                    undo_ktalk: None,
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
        if focused
            && !self.home
            && !self.chat_page
            && !self.people.active()
            && !self.state.tasks_open
            && !self.show_settings
            && let Some(index) = self.state.title_changes.iter().position(|undo| {
                self.state
                    .selected
                    .as_ref()
                    .is_some_and(|path| undo.meeting.path() == path)
            })
            && let Some(undo) = self.state.title_changes.remove(index)
        {
            push_toast(
                &mut self.toasts,
                Toast {
                    at: Instant::now(),
                    text: format!(
                        "Новое название: {}\nПредыдущее: {}",
                        undo.generated, undo.previous
                    ),
                    undo_title: Some(undo),
                    undo_ktalk: None,
                    path: None,
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
        self.toasts.retain(|toast| {
            toast
                .undo_ktalk
                .as_ref()
                .is_none_or(|undo| self.state.ktalk_sync_current(undo))
        });
        self.state.ktalk.undo_visible = self
            .toasts
            .iter()
            .filter_map(|toast| toast.undo_ktalk.as_ref())
            .map(|undo| undo.sync)
            .collect();
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
                let mut restore_title = None;
                let mut restore_ktalk = None;
                for (index, toast) in self.toasts.iter().enumerate() {
                    let response = theme::toast(
                        ui,
                        egui::Id::unique(("toast", toast.at)),
                        toast.level,
                        &toast.text,
                        toast.count,
                        if toast.undo_title.is_some() {
                            Some("Вернуть название")
                        } else if toast
                            .undo_ktalk
                            .as_ref()
                            .is_some_and(|undo| self.state.ktalk_can_undo(undo))
                        {
                            Some("Отменить")
                        } else if toast.retry_settings {
                            Some("Повторить")
                        } else {
                            toast.path.as_ref().map(|_| "Показать в папке")
                        },
                    );
                    if response.close {
                        dismiss = Some(index);
                    }
                    if response.action {
                        if let Some(undo) = &toast.undo_title {
                            restore_title = Some(undo.clone());
                            dismiss = Some(index);
                        }
                        if let Some(undo) = &toast.undo_ktalk {
                            restore_ktalk = Some(undo.clone());
                            dismiss = Some(index);
                        }
                        reveal = toast.path.clone();
                        retry |= toast.retry_settings;
                    }
                }
                if let Some(undo) = restore_ktalk {
                    self.state.ktalk.undo_pending = Some(undo);
                }
                if let Some(undo) = restore_title {
                    self.state.undo_title(undo);
                }
                if retry {
                    self.state
                        .settings_edited(false, self.state.settings_autosave.llm);
                }
                if let Some(index) = dismiss {
                    if let Some(undo) = &self.toasts[index].undo_ktalk {
                        self.state.ktalk.undo_visible.remove(&undo.sync);
                    }
                    self.toasts.remove(index);
                }
                if let Some(path) = reveal {
                    self.state.send(Command::Reveal(path));
                }
                if let Some(error) = self.state.error.clone() {
                    theme::bar(Palette::of(ui)).show(ui, |ui| {
                        crate::permissions::blocked_action(ui, Some(&error));
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
        let mut open_ai = false;
        let mut open_recording_settings = false;
        let can_start = cfg!(any(windows, target_os = "macos"))
            && self.can_record()
            && !self.state.inventory_pending
            && !self.state.record_source_missing;
        theme::modal(ctx,"Новая запись").open(&mut open).show_footer(ctx,|ui| {
            if crate::permissions::panel(ui, &self.state.permissions, false, self.state.recording.busy()) { self.state.permissions.check_requested = true; }
            if let Some(reason) = &self.state.record_options_notice { ui.weak(reason); }
            theme::form_field(ui,"Название",Some("Можно оставить пустым: название появится после конспекта."),|ui|ui.add(theme::text_field(&mut self.state.title).desired_width(f32::INFINITY)));
            theme::form_field(ui,"Распознавание",None,|ui| model_selector(ui,"record-model",&mut self.state.record_model));
            if !self.state.quickstart.models.speech_ready(&self.state.record_model) { ui.weak("Модель не установлена."); open_recording_settings |= ui.link("Настройки записи").clicked(); }
            let mode_description=mode_label(self.state.record_mode);
            theme::form_field(ui,"Режим",Some(mode_description),|ui| {self.state.record_mode_edited |= mode_selector(ui,&mut self.state.record_mode);});
            let readiness = self.state.summary_readiness();
            ui.horizontal_wrapped(|ui| {
                let text = match &readiness { Ok(()) => "Конспект создастся автоматически после Стоп".to_owned(), Err(reason) => format!("Без автоконспекта: {reason}") };
                ui.label(RichText::new(text).size(theme::SMALL).color(Palette::of(ui).muted));
                if readiness.is_err() { open_ai |= ui.link("Настройки AI").clicked(); }
            });
            theme::form_field(ui,"Источник звука",None,|ui| {
                ui.horizontal(|ui| {
                    let mut source_changed=false;
                    crate::theme::select("audio_session").width((ui.available_width()-36.0).max(32.0)).selected_text(if self.state.record_source_missing {"Выберите источник записи".to_owned()} else {self.state.inventory.sessions.iter().find(|s|Some(s.pid)==self.state.process_id).map_or_else(|| if self.state.process_id.is_some() || self.state.record_source_missing {"Выберите источник записи".to_owned()} else {"Весь системный звук".to_owned()},|s|format!("{} (PID {})",s.exe.as_deref().unwrap_or(&s.name),s.pid))}).show_ui(ui,|ui| {
                        if theme::select_value(ui,&mut self.state.process_id,None,"Весь системный звук").clicked() { source_changed=true; }
                        for session in &self.state.inventory.sessions {if session.pid!=0 && theme::select_value(ui,&mut self.state.process_id,Some(session.pid),format!("{} (PID {}){}",session.exe.as_deref().unwrap_or(&session.name),session.pid,if session.active{" · активна"}else{""})).clicked() { source_changed=true; }}
                    });
                    if source_changed { self.state.confirm_record_source(); }
                    if ui.add_enabled_ui(!self.state.inventory_pending,|ui|theme::icon_button(ui,theme::icons::REFRESH_CW,"Обновить аудиосессии")).inner.clicked(){self.state.inventory_pending=true;self.state.send(Command::Inventory);}
                });
            });
            let microphone=self.state.inventory.devices.iter().find(|device|device.channel==notetaker_capture::Channel::Mic && self.state.record_microphone_id.as_ref().map_or(device.is_default,|id|id==&device.id)).map_or("недоступен",|device|device.name.as_str());
            let output=self.state.inventory.devices.iter().find(|device|device.channel==notetaker_capture::Channel::System && device.is_default).map_or("недоступен",|device|device.name.as_str());
            ui.small(format!("Микрофон: {microphone}. Выход: {output}."));
            if self.state.inventory_pending{crate::theme::spinner(ui);}
            if let Some(error)=&self.state.inventory_error{theme::callout(ui,theme::icons::CIRCLE_ALERT,|ui|{ui.small(error);});}
            for warning in &self.state.inventory.warnings {theme::callout(ui,theme::icons::INFO,|ui|{ui.small(warning);});}
            #[cfg(target_os="macos")]
            theme::callout(ui,theme::icons::INFO,|ui|{ui.small("macOS: весь системный звук, кроме notetaker. Нужны разрешения Microphone и System Audio Recording.");});
            #[cfg(not(any(windows,target_os="macos")))]
            theme::callout(ui,theme::icons::INFO,|ui|{ui.small("Захват звука доступен на Windows и macOS. На этой ОС работают библиотека, поиск, правки и конспекты.");});
            if self.state.quickstart.models.missing(&self.state.settings.model).is_some(){crate::quickstart::model_status(ui,&mut self.state);}else{
                ui.horizontal(|ui|{let (rect,_)=ui.allocate_exact_size(egui::vec2(16.0,20.0),egui::Sense::hover());theme::paint_icon(ui,rect.center(),theme::icons::CHECK,16.0,Palette::of(ui).muted);ui.label(RichText::new("Все модели на месте").size(12.0).color(Palette::of(ui).muted));});
            }
        },|ui| {
            theme::footer(ui,|ui| {
                start_record=ui.add_enabled(can_start,theme::styled_button(Palette::of(ui),"Начать запись",ButtonStyle::Primary).icon(theme::icons::MIC)).clicked();
                cancel=theme::button(ui,"Отмена",ButtonStyle::Ghost).clicked();
            });
        });
        if open_ai {
            self.show_settings = true;
            self.settings_panel.section = crate::settings::Section::Ai;
            open = false;
        }
        if open_recording_settings {
            self.show_settings = true;
            self.settings_panel.section = crate::settings::Section::Recording;
            open = false;
        }
        if start_record {
            self.state.start_recording();
            self.show_record = false;
        }
        if cancel {
            open = false;
        }
        self.show_record &= open;
        if !self.show_record {
            self.state.record_options_pending = None;
            self.state.record_source_guard = None;
            self.state.record_dialog_requested = false;
        }
    }
    fn set_main_visible(&mut self, ctx: &egui::Context, visible: bool) {
        self.main_hidden = !visible;
        if !visible {
            self.main_rect = None;
        }
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Visible(visible),
        );
    }
    fn update_main_rect(&mut self, viewport: &egui::ViewportInfo) {
        let focused = viewport.focused.unwrap_or(false);
        if focused && !self.main_focused {
            self.main_hidden = false;
        }
        self.main_focused = focused;
        self.main_rect = visible_main_rect(
            viewport.outer_rect,
            self.main_hidden,
            viewport.minimized.unwrap_or(false),
        );
    }
    fn show_main(&mut self, ctx: &egui::Context, focus: bool) {
        self.set_main_visible(ctx, true);
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Minimized(false),
        );
        if focus {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
    }
    fn exit(&mut self) {
        self.settings_panel.blur(&mut self.state);
        self.quickstart.blur(&mut self.state);
        self.state.exit();
    }
    fn exit_dialog(&mut self, ctx: &egui::Context, now: Instant) {
        if !self.state.exit_requested || self.state.can_exit() {
            return;
        }
        theme::modal(ctx, "Завершаю запись и сохраняю…")
            .closable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                if let Some(status) = self.state.recording.status() {
                    theme::recording_status(ui, &status.text, status.warning, status.progress);
                } else {
                    ui.horizontal(|ui| { theme::spinner(ui); ui.weak("Сохраняю изменения и завершаю операции…"); });
                }
                if let Some(error) = self.state.error.as_ref().or(self.state.settings_autosave.error.as_ref()) {
                    theme::callout(ui, theme::icons::TRIANGLE_ALERT, |ui| { ui.label(error); });
                }
                if self.state.shutdown.can_force(now) {
                    ui.weak("Несохранённые правки будут потеряны. Сохранённое аудио можно дораспознать после запуска кнопкой «Распознать из аудио».");
                    theme::footer(ui, |ui| {
                        if theme::button(ui, "Выйти сейчас", ButtonStyle::Primary).clicked()
                            && self.state.shutdown.force(now)
                        {
                            #[cfg(not(test))]
                            std::thread::spawn(|| {
                                std::thread::sleep(Duration::from_secs(30));
                                notetaker_runtime::process::terminate_all();
                                std::process::exit(0);
                            });
                            #[cfg(not(test))]
                            notetaker_runtime::process::terminate_all();
                            ctx.request_repaint();
                        }
                    });
                }
            });
        ctx.request_repaint_after(Duration::from_millis(100));
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
    fn interaction_blocked(&self, ctx: &egui::Context) -> bool {
        self.state.exit_requested
            || self.show_settings
            || self.state.ktalk.preview_open
            || self.state.ktalk.merge.is_some()
            || self.show_record
            || self.quickstart.open
            || self.rename.is_some()
            || self.confirm_delete
            || self.confirm_regenerate
            || self.image_viewer.is_some()
            || ctx.memory(|m| m.top_modal_layer().is_some())
    }
    fn calendar_reminder_overlay(&mut self, ctx: &egui::Context, bounds: egui::Rect) {
        if self.interaction_blocked(ctx) {
            return;
        }
        let event = self.state.idle_calendar_reminder().cloned();
        let ending = match self.state.call_prompt {
            Some(CallPrompt::Stop { countdown, .. }) => Some(countdown.remaining),
            _ => None,
        };
        if event.is_none() && ending.is_none() {
            return;
        }
        let bounds = bounds.shrink(theme::SPACE[3]);
        let width = bounds.width().clamp(1.0, 340.0);
        let mut action = crate::calendar_ui::Action::None;
        let mut ending_action = crate::detection::StopAction::None;
        egui::Area::new(egui::Id::unique("calendar-reminder"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(bounds.right() - width, bounds.top()))
            .constrain_to(bounds)
            .show(ctx, |ui| {
                ui.set_width(width);
                if let Some(remaining) = ending {
                    // The recording status row below already shows the next meeting.
                    ending_action = crate::detection::stop_card(ui, remaining, None);
                } else if let Some(event) = &event {
                    action =
                        crate::calendar_ui::reminder(ui, event, self.recording_reason().as_deref());
                }
            });
        self.calendar_action(ctx, action);
        self.call_end_action(ending_action);
    }
    fn call_end_action(&mut self, action: crate::detection::StopAction) {
        match action {
            crate::detection::StopAction::Stop => self.state.accept_call(),
            crate::detection::StopAction::Continue => self.state.dismiss_call(),
            crate::detection::StopAction::DismissNext => self.state.dismiss_calendar_reminder(),
            crate::detection::StopAction::None => {}
        }
    }
    fn calendar_action(&mut self, ctx: &egui::Context, action: crate::calendar_ui::Action) {
        match action {
            crate::calendar_ui::Action::Start => {
                let Some(event) = self.state.calendar_reminder().cloned() else {
                    return;
                };
                self.home = false;
                self.state.start_event(event);
                if self.state.recording.phase == Phase::Starting {
                    self.show_main(ctx, false);
                }
            }
            crate::calendar_ui::Action::Join => {
                if let Some(event) = self.state.calendar_reminder().cloned() {
                    self.state.join_event(&event);
                }
            }
            crate::calendar_ui::Action::Dismiss => self.state.dismiss_calendar_reminder(),
            crate::calendar_ui::Action::None => {}
        }
    }
    fn start_hint_visible(&self, ctx: &egui::Context) -> bool {
        crate::detection::start_visible(
            self.interaction_blocked(ctx),
            self.state
                .quickstart
                .models
                .speech_ready(&self.state.record_model),
        )
    }
    fn call_notification(&mut self, ctx: &egui::Context) {
        let calendar = self.state.idle_calendar_reminder().cloned().filter(|_| {
            !self.interaction_blocked(ctx)
                && ctx.input(|i| {
                    !i.viewport().focused.unwrap_or(true) || i.viewport().minimized.unwrap_or(false)
                })
        });
        let prompt = self.state.call_prompt.clone().filter(|prompt| {
            if matches!(prompt, CallPrompt::Start { .. }) {
                self.start_hint_visible(ctx)
            } else {
                self.interaction_blocked(ctx)
                    || ctx.input(|input| {
                        !input.viewport().focused.unwrap_or(true)
                            || input.viewport().minimized.unwrap_or(false)
                    })
            }
        });
        if prompt.is_none() && calendar.is_none() {
            return;
        }
        let mut ending_action = crate::detection::StopAction::None;
        let ending = matches!(prompt, Some(CallPrompt::Stop { .. }));
        // The countdown hides the recording indicator, so it shows its line.
        let next = ending
            .then(|| self.state.next_meeting(chrono::Utc::now()))
            .flatten();
        let mut accept = false;
        let mut join = false;
        let mut dismiss = false;
        let builder = egui::ViewportBuilder::default();
        let main = self.main_rect;
        let hidden = self.main_hidden;
        let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("call-notification"),
            builder
                .with_title("notetaker · звонок")
                .with_inner_size([340.0, if next.is_some() { 168.0 } else { 140.0 }])
                .with_resizable(false)
                .with_decorations(false)
                .with_transparent(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_active(false),
            |ui, _| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, Palette::of(ui).card);
                if let Some(CallPrompt::Stop { countdown, .. }) = &prompt {
                    ending_action =
                        crate::detection::stop_card(ui, countdown.remaining, next.as_ref());
                } else if let Some(event) = &calendar {
                    match crate::calendar_ui::reminder(
                        ui,
                        event,
                        self.recording_reason().as_deref(),
                    ) {
                        crate::calendar_ui::Action::Start => accept = true,
                        crate::calendar_ui::Action::Join => join = true,
                        crate::calendar_ui::Action::Dismiss => dismiss = true,
                        crate::calendar_ui::Action::None => {}
                    }
                } else if let Some(CallPrompt::Start {
                    app, meeting_title, ..
                }) = &prompt
                {
                    let response = crate::detection::start_card(
                        ui,
                        app,
                        meeting_title.as_deref(),
                        self.recording_reason().as_deref(),
                    );
                    accept = response.0;
                    dismiss = response.1;
                }
                let size = ui.min_size();
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                if let Some(position) = notification_position(main, hidden, minimized, size) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
                }
                if ui.input(|input| input.viewport().close_requested()) {
                    dismiss = !ending;
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            },
        );
        self.call_end_action(ending_action);
        if !ending && calendar.is_some() {
            self.calendar_action(
                ctx,
                if accept {
                    crate::calendar_ui::Action::Start
                } else if join {
                    crate::calendar_ui::Action::Join
                } else if !ending && dismiss {
                    crate::calendar_ui::Action::Dismiss
                } else {
                    crate::calendar_ui::Action::None
                },
            );
        } else if !ending && accept {
            self.state.accept_call();
        } else if dismiss {
            self.state.dismiss_call();
        }
    }
    fn indicator(&mut self, ctx: &egui::Context) {
        let next = self.state.next_meeting(chrono::Utc::now());
        let show = ctx.input(|input| {
            indicator_visible(
                self.state.recording.phase,
                input.viewport().minimized.unwrap_or(false),
                input.viewport().focused.unwrap_or(false),
                matches!(self.state.call_prompt, Some(CallPrompt::Stop { .. })),
                next.is_some(),
            )
        });
        if !show {
            self.indicator.closed();
            return;
        }
        let mut stop = false;
        let mut open = false;
        let mut dismiss = false;
        let mut builder = egui::ViewportBuilder::default();
        if let Some(position) = self.indicator.spawn {
            builder = builder.with_position(position);
        }
        let indicator = &mut self.indicator;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("recording-indicator"),
            builder
                .with_title("notetaker · запись")
                .with_inner_size(if next.is_some() {
                    [320.0, 94.0]
                } else {
                    [240.0, 54.0]
                })
                .with_resizable(false)
                .with_decorations(false)
                .with_transparent(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_active(false),
            |ui, _| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, Palette::of(ui).card);
                let size;
                (open, stop, dismiss, size) = recording_pill(
                    ui,
                    self.state.recording.elapsed(),
                    self.state.recording.level(),
                    self.state.recording.phase,
                    next.as_ref(),
                );
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
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
            self.show_main(ctx, true);
        }
        if stop {
            self.state.toggle_recording();
        }
        if dismiss {
            self.state.dismiss_calendar_reminder();
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
            if desktop
                .set_reminder(self.state.calendar_reminder().is_some())
                .is_err()
            {
                notetaker_runtime::diagnostics::event("tray reminder update failed");
            }
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
                        self.show_main(ctx, true);
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
        ctx.input(|input| self.update_main_rect(input.viewport()));
        #[cfg(any(windows, target_os = "macos"))]
        self.desktop(ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.state.can_exit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            #[cfg(any(windows, target_os = "macos"))]
            if self.desktop.is_some() && !self.state.exit_requested {
                self.settings_panel.blur(&mut self.state);
                self.quickstart.blur(&mut self.state);
                self.set_main_visible(ctx, false);
            } else {
                self.exit();
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            self.exit();
        }
        if self.state.record_dialog_requested {
            self.show_main(ctx, false);
        }
        if self.state.can_exit() {
            #[cfg(not(test))]
            notetaker_runtime::process::terminate_all();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.state.exit_requested {
            self.show_main(ctx, false);
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.state.shutdown.deadline_passed(Instant::now()) {
            #[cfg(not(test))]
            {
                notetaker_runtime::process::terminate_all();
                std::process::exit(0);
            }
        }
        if self.state.recording.busy()
            || self.state.call_prompt.is_some()
            || self.state.calendar_reminder().is_some()
        {
            // Allow the UI to create the indicator when a hotkey starts a
            // recording while the root window is hidden.
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
        if let Some(delay) = next_state_wake(&self.state, Instant::now()) {
            ctx.request_repaint_after(delay);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
        for url in take_urls(ui.ctx()) {
            open_url(url);
        }
    }
}
impl NotetakerApp {
    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.state.exit_requested {
            egui::Frame::central_panel(ui.style())
                .show(ui, |ui| ui.set_min_size(ui.available_size()));
            self.exit_dialog(&ctx, Instant::now());
            return;
        }
        if std::mem::take(&mut self.state.permissions.show_requested) {
            self.show_main(&ctx, true);
        }
        if cfg!(any(target_os = "macos", windows)) {
            self.state.permissions.on_focus(
                ctx.input(|input| {
                    input.focused
                        || input
                            .raw
                            .viewports
                            .values()
                            .any(|viewport| viewport.focused == Some(true))
                }),
                self.state.permissions.query,
            );
        }
        if self.state.permissions.check_requested
            && !self.state.recording.busy()
            && !self.state.quickstart.probe_busy
        {
            self.state.permissions.check_requested = false;
            self.state.permissions.refresh();
            self.quickstart.start_probe(&mut self.state);
        }
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
        if !self.interaction_blocked(&ctx)
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::J))
        {
            self.focus_chat = true;
            self.state.chat.open = true;
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
            let reminder_bounds = ui.available_rect_before_wrap();
            self.toast_anchor = Some(egui::pos2(
                ui.available_rect_before_wrap().center().x,
                ctx.content_rect().bottom() - 12.0,
            ));
            self.settings_dialog(ui);
            self.calendar_reminder_overlay(&ctx, reminder_bounds);
            self.notifications(&ctx);
            self.indicator(&ctx);
            self.call_notification(&ctx);
            self.quickstart.show(&ctx, &mut self.state);
            return;
        }
        // This height excludes all mode-dependent margins and bottom-panel content.
        self.meeting_layout.update(ui.available_height());
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
        let reminder_bounds = ui.available_rect_before_wrap();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(
                if !self.home
                    && !self.chat_page
                    && !self.people.active()
                    && !self.state.tasks_open
                    && self.meeting_layout.compact
                {
                    8
                } else if ctx.content_rect().height() < 600.0 {
                    16
                } else if ctx.content_rect().width() < 900.0 {
                    24
                } else {
                    48
                },
            ))
            .show(ui, |ui| {
                ui.add_enabled_ui(!self.state.settings_pending, |ui| {
                    if !self.home
                        && self.state.permissions.blocked().is_some()
                        && crate::permissions::panel(
                            ui,
                            &self.state.permissions,
                            false,
                            self.state.recording.busy(),
                        )
                    {
                        self.state.permissions.check_requested = true;
                    }
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
        self.calendar_reminder_overlay(&ctx, reminder_bounds);
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
        if self.state.record_dialog_requested {
            self.show_record = true;
            self.state.record_dialog_requested = false;
        }
        if self.state.recording.busy() {
            self.show_record = false;
        }
        if self.show_record {
            self.recording_dialog(&ctx);
        }
        self.indicator(&ctx);
        self.call_notification(&ctx);
        let onboarding_open = self.quickstart.open;
        crate::ktalk::windows(&ctx, &mut self.state);
        self.quickstart.show(&ctx, &mut self.state);
        if self.state.permissions.show_requested {
            ctx.request_repaint();
        }
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
/// eframe is built without its link opener, so every hyperlink and `open_url` call
/// of the frame is collected here and handed to the system.
fn take_urls(ctx: &egui::Context) -> Vec<String> {
    let mut urls = Vec::new();
    ctx.output_mut(|output| {
        output.commands.retain(|command| match command {
            egui::OutputCommand::OpenUrl(open) => {
                urls.push(open.url.clone());
                false
            }
            _ => true,
        });
    });
    // Links come from summaries and trackers too: only web addresses leave the app.
    urls.retain(|url| {
        ["https://", "http://", "mailto:"]
            .iter()
            .any(|scheme| url.starts_with(scheme))
    });
    urls
}
fn open_url(url: String) {
    // The system opener may block for a moment; keep the frame responsive.
    std::thread::spawn(move || {
        #[cfg(windows)]
        let _ = notetaker_runtime::windows::open_url(&url);
        #[cfg(unix)]
        let _ = notetaker_runtime::unix::open_url(&url);
    });
}
fn next_state_wake(state: &AppState, now: Instant) -> Option<Duration> {
    next_state_wake_at(state, now, chrono::Utc::now())
}
fn next_state_wake_at(
    state: &AppState,
    now: Instant,
    utc: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let mut deadlines = Vec::new();
    if !state.ktalk.busy
        && !state.settings_pending
        && !state.ktalk.preview_open
        && !state.settings_autosave.pending
        && (state.ktalk.load_root.is_some()
            || state.ktalk.undo_pending.is_some()
            || state.settings.voice_library && state.ktalk.enroll_pending.is_some())
    {
        deadlines.push(Duration::ZERO);
    }
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
    }
    let auto_names = state
        .ktalk
        .settings
        .as_ref()
        .is_some_and(notetaker_llm::ktalk::Settings::auto_sync);
    if !state.ktalk.busy && state.ktalk.saved {
        for task in state.ktalk.tasks.values().filter(|task| {
            auto_names && task.progress.recorded_here && !task.progress.done
                || task.progress.enrol_pending
        }) {
            let delay = task
                .progress
                .next
                .unwrap_or(utc)
                .signed_duration_since(utc)
                .to_std()
                .unwrap_or(Duration::ZERO);
            deadlines.push(delay.max(Duration::from_secs(1)));
        }
    }
    if !state.settings.calendars.is_empty()
        && let Some(midnight) = crate::calendar_ui::next_local_midnight(utc)
        && let Ok(delay) = (midnight - utc).to_std()
    {
        deadlines.push(delay);
    }
    for event in &state.calendar_events {
        for time in [
            event.start - chrono::Duration::minutes(5),
            event.start - chrono::Duration::minutes(1),
            event.start,
            event.end,
        ] {
            if time > utc
                && let Ok(delay) = (time - utc).to_std()
            {
                deadlines.push(delay);
            }
        }
    }
    if state.calendar_reminder().is_some() {
        deadlines.push(Duration::from_secs(30));
    }
    deadlines
        .into_iter()
        .min()
        .map(|delay| delay.max(Duration::from_millis(16)))
}

/// Clicking free space opens the main window; dragging it moves the indicator.
fn recording_pill(
    ui: &mut egui::Ui,
    elapsed: Duration,
    level: f32,
    phase: Phase,
    next: Option<&crate::calendar_ui::NextMeeting>,
) -> (bool, bool, bool, egui::Vec2) {
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
    let mut dismiss = false;
    let frame = theme::bar(p)
        .corner_radius(12)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(
                (if next.is_some() { 298.0_f32 } else { 218.0_f32 }).min(ui.available_width()),
            );
            ui.horizontal(|ui| {
                if phase != Phase::Recording {
                    theme::spinner(ui);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(if phase == Phase::Starting {
                                "Начинаю запись"
                            } else {
                                "Распознаю запись"
                            })
                            .font(theme::medium(theme::SMALL))
                            .color(p.muted),
                        )
                        .truncate(),
                    );
                    return;
                }
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
            if let Some(next) = next {
                dismiss = crate::calendar_ui::next_meeting(ui, next);
            }
        });
    (
        background.clicked(),
        stop,
        dismiss,
        frame.response.rect.size(),
    )
}

pub(crate) fn template_name(root: &std::path::Path, id: &str) -> String {
    match notetaker_llm::Template::load(root, id) {
        Ok(template) => template.name,
        Err(_) => "Ошибка чтения шаблона".into(),
    }
}
fn template_value(ui: &egui::Ui, name: &str) -> egui::text::LayoutJob {
    let p = Palette::of(ui);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        theme::icons::FILE_TEXT,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::new(theme::BUTTON, egui::FontFamily::Name("icons".into())),
            color: p.muted,
            ..Default::default()
        },
    );
    job.append(
        &format!(" {name}"),
        0.0,
        egui::TextFormat {
            font_id: theme::medium(theme::BUTTON),
            color: p.text,
            ..Default::default()
        },
    );
    job
}
fn note_line_key(line: &str) -> &str {
    let line = line
        .trim()
        .trim_start_matches(['#', '-', '*', '+', '>', ' '])
        .trim();
    line.strip_prefix("[ ] ")
        .or_else(|| line.strip_prefix("[x] "))
        .or_else(|| line.strip_prefix("[X] "))
        .unwrap_or(line)
}

/// Only exact source lines are attributed to the author. Paraphrases remain AI text.
type DocumentRun = (bool, String, Vec<Option<(bool, usize)>>);
fn document_runs(notes: &str, enhanced: &str) -> Vec<DocumentRun> {
    let originals: std::collections::BTreeSet<_> = notes
        .lines()
        .map(note_line_key)
        .filter(|s| !s.is_empty())
        .collect();
    let enhanced_lines: std::collections::BTreeSet<_> =
        enhanced.lines().map(note_line_key).collect();
    let missing: Vec<_> = notes
        .lines()
        .enumerate()
        .filter(|(_, line)| !enhanced_lines.contains(note_line_key(line)))
        .collect();
    let missing_text = missing
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    let mut runs: Vec<DocumentRun> = Vec::new();
    if !missing_text.trim().is_empty() {
        let mut sources: Vec<_> = missing
            .iter()
            .map(|(index, _)| Some((true, index + 1)))
            .collect();
        sources.push(None);
        runs.push((true, format!("{missing_text}\n\n"), sources));
    }
    for (index, line) in enhanced.split_inclusive('\n').enumerate() {
        let own = originals.contains(note_line_key(line));
        if let Some((last_own, text, sources)) = runs.last_mut()
            && *last_own == own
        {
            text.push_str(line);
            sources.push(Some((false, index + 1)));
        } else {
            runs.push((own, line.into(), vec![Some((false, index + 1))]));
        }
    }
    runs
}
type DocumentToggle = (bool, notetaker_core::Task, bool);

fn bind_document_task(
    document: &Document,
    mut task: notetaker_core::Task,
    notes: bool,
) -> notetaker_core::Task {
    task.path = document.data.meeting.path().into();
    task.meeting_id = document.data.meta.id;
    task.meeting_title = document.data.meta.title.clone();
    task.revision = if notes {
        document.data.revisions.notes
    } else {
        document.data.revisions.enhanced
    };
    task
}
fn document_tasks(
    document: &Document,
) -> std::collections::BTreeMap<(bool, usize), notetaker_core::TaskLine> {
    [
        (true, document.data.notes.as_str()),
        (
            false,
            document
                .generation
                .as_ref()
                .unwrap_or(&document.data.enhanced)
                .as_str(),
        ),
    ]
    .into_iter()
    .flat_map(|(notes, text)| {
        notetaker_core::parse_task_lines(text, document.data.meta.started_at.date_naive())
            .into_iter()
            .map(move |mut line| {
                line.task = bind_document_task(document, line.task, notes);
                ((notes, line.task.line), line)
            })
    })
    .collect()
}

#[derive(Default)]
struct DocumentPreview {
    scroll_y: f32,
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
    source: Option<(bool, notetaker_core::Task)>,
    indent: f32,
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
        let tasks = document_tasks(document);
        let lines: Vec<_> = runs
            .iter()
            .flat_map(|(own, text, sources)| {
                text.lines()
                    .enumerate()
                    .map(move |(index, line)| (*own, line, sources.get(index).copied().flatten()))
            })
            .collect();
        self.blocks.clear();
        let mut index = 0;
        while let Some(&(own, line, source)) = lines.get(index) {
            if line.trim().is_empty() {
                index += 1;
                continue;
            }
            if let Some((key, task)) =
                source.and_then(|key| tasks.get(&key).map(|task| (key, task)))
            {
                let body = task
                    .task
                    .source
                    .get(task.marker.end..)
                    .unwrap_or_default()
                    .trim_start();
                let prefix = task
                    .task
                    .source
                    .get(..task.marker.start)
                    .unwrap_or_default();
                self.blocks.push(DocumentBlock {
                    own,
                    source: Some((key.0, task.task.clone())),
                    indent: prefix
                        .chars()
                        .take_while(|ch| ch.is_whitespace() || *ch == '>')
                        .count() as f32
                        * 6.0,
                    kind: DocumentBlockKind::Item(
                        Some(task.task.completed),
                        PreparedMarkdown::new(body, path),
                    ),
                });
                index += 1;
            } else if let Some((level, title)) = note_heading(line) {
                self.blocks.push(DocumentBlock {
                    own,
                    source: None,
                    indent: 0.0,
                    kind: DocumentBlockKind::Heading(level, title.into()),
                });
                index += 1;
            } else if let Some((_, task, body)) = note_item(line)
                && lines
                    .get(index + 1)
                    .is_none_or(|(_, next, _)| !next.starts_with([' ', '\t']))
            {
                self.blocks.push(DocumentBlock {
                    own,
                    source: None,
                    indent: 0.0,
                    kind: DocumentBlockKind::Item(task, PreparedMarkdown::new(body, path)),
                });
                index += 1;
            } else {
                let start = index;
                // Keep a Markdown block intact, including fences and attribution changes.
                while let Some((_, _, source)) = lines.get(index) {
                    if index > start && source.is_some_and(|key| tasks.contains_key(&key)) {
                        break;
                    }
                    index += 1;
                }
                let run = &lines[start..index];
                let text = run
                    .iter()
                    .map(|(_, line, _)| *line)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.blocks.push(DocumentBlock {
                    own: run.iter().all(|(own, _, _)| *own),
                    source: None,
                    indent: 0.0,
                    kind: DocumentBlockKind::Markdown(PreparedMarkdown::new(&text, path)),
                });
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
    can_toggle: bool,
) -> Option<DocumentToggle> {
    let mut toggle = None;
    let p = Palette::of(ui);
    prepared.layout.sync(
        prepared.revision,
        prepared.blocks.len(),
        ui.available_width(),
        ui.ctx().pixels_per_point(),
        36.0,
    );
    let scroll = egui::ScrollArea::vertical()
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
                let Some(block) = prepared.blocks.get(index) else {
                    continue;
                };
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
                            ui.add_space(block.indent);
                            if let (Some(completed), Some((notes, source))) = (task, &block.source)
                            {
                                let mut completed = *completed;
                                let response = ui.add_enabled(
                                    can_toggle,
                                    theme::Checkbox::without_text(&mut completed),
                                );
                                if response.clicked() {
                                    response.request_focus();
                                }
                                if response.changed() {
                                    toggle = Some((*notes, source.clone(), completed));
                                }
                            } else {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(14.0, 22.0),
                                    egui::Sense::hover(),
                                );
                                note_marker(ui.painter(), rect.center(), *task, ink);
                            }
                            markdown_preview(ui, cache, markdown, viewer, *task == Some(true));
                        });
                    }
                    DocumentBlockKind::Markdown(markdown) => {
                        markdown_preview(ui, cache, markdown, viewer, false);
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
    prepared.scroll_y = scroll.state.clamped_offset().y;
    toggle
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
fn indicator_visible(
    phase: Phase,
    minimized: bool,
    focused: bool,
    countdown: bool,
    next: bool,
) -> bool {
    (phase == Phase::Recording || next && phase != Phase::Idle)
        && (minimized || !focused)
        && !countdown
}
fn visible_main_rect(
    main: Option<egui::Rect>,
    hidden: bool,
    minimized: bool,
) -> Option<egui::Rect> {
    main.filter(|rect| {
        !hidden && !minimized && rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0
    })
}
// egui 0.37 has no monitor origin; a partly off-screen main window is outside this placement guarantee.
fn notification_position(
    main: Option<egui::Rect>,
    hidden: bool,
    minimized: bool,
    size: egui::Vec2,
) -> Option<egui::Pos2> {
    let inside = visible_main_rect(main, hidden, minimized)?.shrink(theme::SPACE[3]);
    (size.is_finite()
        && size.x > 0.0
        && size.y > 0.0
        && inside.width() >= size.x
        && inside.height() >= size.y)
        .then(|| inside.right_bottom() - size)
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
            .map(|(range, _)| text.get(range).unwrap_or_default().to_owned())
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
    completed: bool,
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
            && let Some(uri) = image_uris.get(index)
        {
            let Some(uri) = uri else {
                if let Some(text) = image_text.get(index) {
                    ui.label(text);
                }
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
    theme::muted_strikethrough(ui, completed, |ui| {
        CommonMarkViewer::new()
            .render_html_fn(Some(&render))
            .show(ui, cache, &prepared.markdown);
    });
    if let Some(uri) = clicked.get().and_then(|index| prepared.uris.get(index)) {
        *viewer = uri.clone();
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
    if body.starts_with(['[', '`', '~']) {
        return None;
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

fn note_layout(
    text: &str,
    p: Palette,
    width: f32,
    tasks: &[notetaker_core::TaskLine],
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    let body = egui::TextFormat {
        font_id: egui::FontId::proportional(theme::BODY),
        color: p.text,
        line_height: Some(24.0),
        ..Default::default()
    };
    let mut fenced = false;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
        }
        if let Some(task) = tasks.iter().find(|task| task.task.line == index + 1) {
            // Preserve the original prefix and all character offsets in the editor.
            let prefix = line.get(..task.marker.start).unwrap_or_default();
            let marker = line.get(task.marker.clone()).unwrap_or_default();
            let text = line.get(task.marker.end..).unwrap_or_default();
            job.append(prefix, 0.0, body.clone());
            job.append(
                marker,
                0.0,
                egui::TextFormat {
                    color: egui::Color32::TRANSPARENT,
                    ..body.clone()
                },
            );
            let mut format = body.clone();
            if task.task.completed {
                format.color = p.muted;
                format.strikethrough = egui::Stroke::new(1.0, p.muted);
            }
            job.append(text, 0.0, format);
        } else if !fenced && let Some((level, title)) = note_heading(line) {
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
    scroll_y: f32,
    key: Option<(u64, u32, u32, bool)>,
    galley: Option<std::sync::Arc<egui::Galley>>,
    markers: Vec<(egui::Pos2, Option<notetaker_core::Task>)>,
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
        if self.key == Some(key)
            && let Some(galley) = &self.galley
        {
            return galley.clone();
        }
        let tasks = notetaker_core::parse_task_lines(text, chrono::Utc::now().date_naive());
        let galley = ui
            .fonts_mut(|fonts| fonts.layout_job(note_layout(text, Palette::of(ui), width, &tasks)));
        self.markers.clear();
        let mut offset = 0;
        let mut fenced = false;
        for (line_index, line) in text.split_inclusive('\n').enumerate() {
            if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
                fenced = !fenced;
            }
            let task = tasks.iter().find(|task| task.task.line == line_index + 1);
            if let Some(task) = task {
                let prefix = line
                    .get(..task.marker.start)
                    .unwrap_or_default()
                    .chars()
                    .count();
                let rect = galley.pos_from_cursor(egui::text::CCursor::new(offset + prefix));
                self.markers.push((
                    egui::pos2(rect.left() + 7.0, rect.center().y),
                    Some(task.task.clone()),
                ));
            } else if !fenced && note_item(line).is_some() {
                let rect = galley.pos_from_cursor(egui::text::CCursor::new(offset));
                self.markers
                    .push((egui::pos2(rect.left() + 7.0, rect.center().y), None));
            }
            offset += line.chars().count();
        }
        self.galley = Some(galley.clone());
        self.key = Some(key);
        galley
    }
}

fn notes_editor(
    ui: &mut egui::Ui,
    document: &mut Document,
    cache: &mut NotesCache,
    path: &std::path::Path,
    viewer: &mut Option<String>,
    can_toggle: bool,
) -> (Vec<ImageInput>, Option<DocumentToggle>) {
    let mut toggle = None;
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
    let scroll = egui::ScrollArea::vertical()
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
            for (pos, task) in cache
                .markers
                .iter()
                .skip(first)
                .take_while(|(pos, _)| pos.y <= bottom)
            {
                if let Some(task) = task {
                    let rect = egui::Rect::from_center_size(
                        *pos + output.galley_pos.to_vec2(),
                        egui::vec2(18.0, 22.0),
                    );
                    let mut checked = task.completed;
                    let mut row = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("notes-task", task.line))
                            .max_rect(rect),
                    );
                    let response =
                        row.add_enabled(can_toggle, theme::Checkbox::without_text(&mut checked));
                    if response.clicked() {
                        response.request_focus();
                    }
                    // The editor keeps its text path; only a clean document may atomically toggle.
                    if can_toggle && response.changed() {
                        toggle = Some((
                            true,
                            bind_document_task(document, task.clone(), true),
                            checked,
                        ));
                    }
                } else {
                    note_marker(
                        &painter,
                        *pos + output.galley_pos.to_vec2(),
                        None,
                        Palette::of(ui).muted,
                    );
                }
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
    cache.scroll_y = scroll.state.clamped_offset().y;
    (images, toggle)
}
#[derive(Default)]
struct TranscriptCache {
    scroll_y: f32,
    compact: bool,
    key: Option<(Instant, notetaker_core::Revision, usize, bool, u64, bool)>,
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
            document.speaker_ui.cache_revision,
            self.compact,
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
                        self.compact,
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
                            if cache.compact {
                                ui.spacing_mut().item_spacing.y = 0.0;
                            }
                            ui.horizontal(|ui| {
                                let name_response = ui.add(
                                    egui::Label::new(
                                        RichText::new(if me {
                                            "Я".to_owned()
                                        } else {
                                            crate::speakers::display_name(
                                                &document.data.transcript,
                                                &segment.speaker.0,
                                            )
                                        })
                                        .font(theme::medium(13.0))
                                        .color(p.text),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                crate::speakers::voice_note(
                                    ui,
                                    &document.data.transcript,
                                    &segment.speaker.0,
                                );
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
                                if editable && (!live || segment.speaker.0.starts_with("live-")) {
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
                                                (&name_response, row_hover),
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
            let measured = response.rect.height() + if cache.compact { 4.0 } else { 16.0 };
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
    cache.scroll_y = _viewport.state.clamped_offset().y;
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::unique("transcript-viewport-width"),
            _viewport.inner_rect.width(),
        );
        data.insert_temp(
            egui::Id::unique("transcript-viewport"),
            _viewport.inner_rect,
        );
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
    fn task_rows_follow_the_fixed_stride_across_scroll_offsets_and_group_headers() {
        use super::*;
        let (mut state, _commands, _events) = crate::test_ui::state();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meeting");
        state.tasks = (0..200)
            .map(|index| notetaker_core::Task {
                meeting_id: uuid::Uuid::now_v7(),
                path: path.clone(),
                meeting_title: "Meeting".into(),
                line: index + 1,
                text: format!("Task {index:03}"),
                owner: format!("Group {:02}", index / 10),
                requester: String::new(),
                due_text: String::new(),
                due: None,
                completed: false,
                source: String::new(),
                issue_url: None,
                revision: Default::default(),
            })
            .collect();
        let mut app = NotetakerApp::with_state(state);
        app.sync_tasks_cache();
        let target = 20;
        let row = app
            .tasks_cache
            .entries
            .iter()
            .position(|entry| matches!(entry, TaskEntry::Task(index) if *index == target))
            .unwrap();
        assert_eq!(row, 23, "the target follows three group headers");
        let id = egui::Id::unique(("task-row-rect", path.as_path(), target + 1));
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut frame = 0;
        let mut draw = |events| {
            ctx.data_mut(|data| data.remove::<egui::Rect>(id));
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 900.0),
                    )),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    app.tasks_screen(ui);
                },
            )
            .drop_without_applying_deltas();
            frame += 1;
        };
        for _ in 0..3 {
            draw(vec![]);
        }
        let viewport = ctx
            .data(|data| data.get_temp::<(egui::Rect, f32)>(egui::Id::unique("task-list-model")))
            .unwrap()
            .0;
        let mut offsets = Vec::new();
        for delta in [400.0, 100.0] {
            draw(vec![
                egui::Event::PointerMoved(viewport.center()),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -delta),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            for _ in 0..40 {
                draw(vec![]);
            }
            let (viewport, offset) = ctx
                .data(|data| {
                    data.get_temp::<(egui::Rect, f32)>(egui::Id::unique("task-list-model"))
                })
                .unwrap();
            let rect = ctx
                .data(|data| data.get_temp::<egui::Rect>(id))
                .expect("target must be laid out in this frame");
            assert!(viewport.intersects(rect));
            let expected = viewport.top() + row as f32 * 48.0 - offset;
            assert!(
                (rect.top() - expected).abs() < 0.1,
                "task row at {} instead of {expected}, scroll={offset}",
                rect.top()
            );
            assert_eq!(rect.height(), 48.0);
            offsets.push(offset);
        }
        assert!(offsets[0] > 300.0 && offsets[1] - offsets[0] > 48.0);
    }
    #[test]
    fn live_echo_replacement_invalidates_caches_even_when_turn_count_is_unchanged() {
        use super::*;
        let (_dir, mut document, path) = synthetic_document(1);
        document.data.transcript.segments[0].channel = notetaker_core::Channel::Mic;
        document.data.transcript.segments[0].speaker.0 = "me".into();
        let old_id = document.data.transcript.segments[0].id;
        let changed_at = document.changed_at;
        let mut system = document.data.transcript.segments[0].clone();
        system.id = uuid::Uuid::now_v7();
        system.channel = notetaker_core::Channel::System;
        system.speaker.0 = "s1".into();
        let new_id = system.id;
        let old_timeline = crate::speakers::timeline(&mut document);
        assert_eq!(old_timeline.remote_count, 0);
        let mut cache = TranscriptCache::default();
        cache.sync(&document, &path, 900.0, 1.0, true);
        let (mut state, _commands, events) = crate::test_ui::state();
        state.recording.phase = Phase::Recording;
        state.recording.path = Some(path.clone());
        state.documents.insert(path.clone(), document);
        events
            .send(crate::worker::Event::Pipeline(
                notetaker_pipeline::PipelineEvent::LiveSegments(vec![system]),
            ))
            .unwrap();
        state.poll();
        let document = state.documents.get_mut(&path).unwrap();
        assert_eq!(document.data.transcript.segments.len(), 1);
        assert_eq!(
            document.changed_at, changed_at,
            "cache invalidation must not delay note autosave"
        );
        cache.sync(document, &path, 900.0, 1.0, true);
        assert!(!cache.indices.contains_key(&old_id));
        assert_eq!(cache.indices.get(&new_id), Some(&0));
        let new_timeline = crate::speakers::timeline(document);
        assert_eq!(new_timeline.remote_count, 1);
        assert!(!std::sync::Arc::ptr_eq(&old_timeline, &new_timeline));
    }
    #[test]
    fn middle_jump_tracks_the_target_until_lazy_heights_settle() {
        use super::*;
        for size in [egui::vec2(1000.0, 600.0), egui::vec2(640.0, 427.0)] {
            let (_dir, mut document, path) = synthetic_document(5000);
            let target = document.data.transcript.segments[2499].id;
            document.speaker_ui.scroll_to = Some(target);
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let mut cache = TranscriptCache::default();
            for frame in 0..40 {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        time: Some(frame as f64 * 0.1),
                        ..Default::default()
                    },
                    |ui| {
                        if size.y < 500.0 {
                            ui.add_space(300.0);
                        }
                        transcript_view(ui, &mut cache, &mut document, false, None, true, true);
                    },
                )
                .drop_without_applying_deltas();
            }
            assert!(
                document.speaker_ui.scroll_to.is_none(),
                "jump never settled at {size:?}"
            );
            let response = ctx
                .read_response(transcript_text_id(&path, target))
                .expect("target must be laid out");
            assert!(
                response.rect.intersects(ctx.content_rect()),
                "jump landed on a different turn at {size:?}"
            );
            let (anchor, _) = cache.anchor.unwrap();
            assert!((2480..=2499).contains(&cache.indices[&anchor]));
        }
    }
    #[test]
    fn document_blocks_and_note_galleys_are_reused_until_inputs_change() {
        use super::*;
        let (_dir, mut document, path) = synthetic_document(0);
        document.data.enhanced = (0..5000).map(|index| format!("- Item {index}\n")).collect();
        let mut prepared = DocumentPreview::default();
        prepared.prepare(&document, &path);
        assert_eq!(prepared.blocks.len(), 5000);
        let revision = prepared.revision;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut markdown_cache = CommonMarkCache::default();
        let mut notes = NotesCache::default();
        for _ in 0..3 {
            prepared.prepare(&document, &path);
            assert_eq!(prepared.revision, revision);
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::unique("document-layout-count"), 0_usize)
            });
            crate::test_ui::frame(&ctx, vec![], &mut |ui| {
                let first = notes.layout(ui, "## Заголовок\n- [ ] Задача", 500.0);
                let second = notes.layout(ui, "## Заголовок\n- [ ] Задача", 500.0);
                assert!(std::sync::Arc::ptr_eq(&first, &second));
                enhanced_document(ui, &mut markdown_cache, &mut prepared, &mut None, false);
            })
            .drop_without_applying_deltas();
            let count = ctx
                .data(|data| data.get_temp::<usize>(egui::Id::unique("document-layout-count")))
                .unwrap();
            assert!(
                count > 0 && count < 64,
                "Laid out {count} of 5000 document blocks"
            );
        }
        document.data.enhanced = "## Changed".into();
        document.changed();
        prepared.prepare(&document, &path);
        assert_ne!(prepared.revision, revision);
        assert_eq!(prepared.blocks.len(), 1);
        document.generation = Some(String::new());
        prepared.prepare(&document, &path);
        assert!(
            prepared.blocks.is_empty(),
            "an empty new generation must invalidate the previous summary"
        );
    }
    #[test]
    fn transcript_jump_copy_focus_and_live_tail_remain_available() {
        use super::*;
        let (_dir, mut document, path) = synthetic_document(5000);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut cache = TranscriptCache::default();
        let last = document.data.transcript.segments.last().unwrap().id;
        document.speaker_ui.scroll_to = Some(last);
        let mut draw = |ui: &mut egui::Ui| {
            transcript_view(ui, &mut cache, &mut document, false, None, true, true);
        };
        for _ in 0..40 {
            crate::test_ui::frame(&ctx, vec![], &mut draw).drop_without_applying_deltas();
        }
        assert!(ctx.read_response(transcript_text_id(&path, last)).is_some());
        ctx.memory_mut(|mem| mem.request_focus(transcript_text_id(&path, last)));
        crate::test_ui::frame(&ctx, vec![], &mut draw).drop_without_applying_deltas();
        let output = crate::test_ui::frame(
            &ctx,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                },
                egui::Event::Copy,
            ],
            &mut draw,
        );
        assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text.contains("Повторяемая реплика"))));
        output.drop_without_applying_deltas();
        assert!(document.speaker_ui.scroll_to.is_none());
        document.speaker_ui.scroll_to = Some(document.data.transcript.segments[0].id);
        for _ in 0..40 {
            ctx.run_ui(Default::default(), |ui| {
                transcript_view(ui, &mut cache, &mut document, false, None, true, true);
            })
            .drop_without_applying_deltas();
        }
        assert!(
            ctx.read_response(transcript_text_id(&path, last)).is_some(),
            "focused off-screen editor must remain alive"
        );
        assert!(document.speaker_ui.scroll_to.is_none());
        let live_ctx = egui::Context::default();
        theme::install(&live_ctx);
        let mut live_cache = TranscriptCache::default();
        for _ in 0..40 {
            crate::test_ui::frame(&live_ctx, vec![], &mut |ui| {
                transcript_view(ui, &mut live_cache, &mut document, true, None, false, false);
            })
            .drop_without_applying_deltas();
        }
        assert!(
            live_cache
                .anchor
                .is_some_and(|(id, _)| live_cache.indices[&id] > 4900),
            "live view must follow the tail"
        );
    }
    #[test]
    fn transcript_cache_invalidates_on_edits_and_preserves_anchor_when_resized() {
        use super::*;
        let (_dir, mut document, path) = synthetic_document(200);
        let mut cache = TranscriptCache::default();
        cache.sync(&document, &path, 900.0, 1.0, false);
        let id = document.data.transcript.segments[100].id;
        cache.anchor = Some((id, 0.25));
        cache.rows.set_height(0, 200.0);
        cache.sync(&document, &path, 400.0, 1.5, false);
        assert_eq!(cache.scroll_override, Some(8020.0));
        let old = crate::speakers::timeline(&mut document);
        assert!(std::sync::Arc::ptr_eq(
            &old,
            &crate::speakers::timeline(&mut document)
        ));
        document.data.transcript.segments[100].edited = true;
        document.changed();
        cache.sync(&document, &path, 400.0, 1.5, false);
        assert!(cache.times[100].contains("исправлено"));
        assert!(!std::sync::Arc::ptr_eq(
            &old,
            &crate::speakers::timeline(&mut document)
        ));
        cache.rows.set_height(10, 150.0);
        document.data.transcript.segments.swap(10, 11);
        document.changed();
        cache.sync(&document, &path, 400.0, 1.5, false);
        assert_eq!(
            cache.rows.height(11),
            150.0,
            "height follows the turn ID after reordering"
        );
        document.data.transcript.segments[11].text = "Changed text".into();
        document.changed();
        cache.sync(&document, &path, 400.0, 1.5, false);
        assert_eq!(
            cache.rows.height(11),
            80.0,
            "changed text invalidates its measured height"
        );
    }
    #[test]
    fn meetings_cache_filters_and_indexes_5000_rows_without_laying_them_out() {
        use super::*;
        let (_dir, document, _path) = synthetic_document(0);
        let (mut state, _commands, _events) = crate::test_ui::state();
        state.rows = (0..5000)
            .map(|index| {
                let mut meta = document.data.meta.clone();
                meta.folder = (index % 2 == 0).then(|| "Team".into());
                crate::worker::MeetingRow {
                    meeting: document.data.meeting.clone(),
                    meta,
                    snippet: String::new(),
                    anchor: None,
                }
            })
            .collect();
        let organization = crate::organization::Panel::default();
        let mut cache = MeetingsCache::default();
        cache.sync(&state, &organization);
        assert_eq!(cache.count, 5000);
        let range = cache.layout.visible(egui::Rect::from_min_size(
            egui::pos2(0.0, 50000.0),
            egui::vec2(900.0, 600.0),
        ));
        assert!(range.len() <= 18);
        state.folder_filter = Some("Team".into());
        cache.sync(&state, &organization);
        assert_eq!(cache.count, 2500);
        state.rows.clear();
        state.view_revision += 1;
        cache.sync(&state, &organization);
        assert_eq!(cache.count, 0);
    }
    #[test]
    fn idle_has_no_frame_timer_and_pending_edits_schedule_a_deadline() {
        use super::*;
        let (mut state, _commands, _events) = crate::test_ui::state();
        assert_eq!(next_state_wake(&state, Instant::now()), None);
        state.settings_autosave.pending = false;
        state.ready = true;
        state.quickstart.models = crate::quickstart::Models {
            gigaam: crate::quickstart::Status::Ready,
            parakeet: crate::quickstart::Status::Ready,
            auto: crate::quickstart::Status::Ready,
            diarization: crate::quickstart::Status::Ready,
            semantic: crate::quickstart::Status::Ready,
        };
        crate::quickstart::auto_download(&mut state);
        assert!(next_state_wake(&state, Instant::now()).unwrap() >= Duration::from_secs(59));
        state.ready = false;
        state.query_changed = Some(Instant::now());
        assert!(next_state_wake(&state, Instant::now()).unwrap() <= Duration::from_millis(301));
        state.query_changed = None;
        state.settings_autosave.pending = true;
        state.settings_autosave.error = Some("invalid".into());
        assert_eq!(next_state_wake(&state, Instant::now()), None);
    }
    fn synthetic_document(count: u64) -> (tempfile::TempDir, super::Document, std::path::PathBuf) {
        use super::*;
        use notetaker_core::{Channel, Library, MeetingMeta, Segment, Source, SpeakerId};
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Benchmark", chrono::Utc::now()))
            .unwrap();
        let path = meeting.path().to_owned();
        let mut data = crate::worker::DocumentData::read(meeting).unwrap();
        for index in 0..count {
            data.transcript.segments.push(Segment {
                id: uuid::Uuid::now_v7(), speaker: SpeakerId("s1".into()), channel: Channel::System,
                start_ms: index * 1000, end_ms: (index + 1) * 1000,
                text: "A realistic transcript turn with enough words to wrap on a narrow window. Повторяемая реплика для проверки виртуализации.".into(),
                source: Source::Final, edited: false, lang: None,
            });
        }
        let document = Document {
            data,
            speaker_ui: Default::default(),
            attachments_pending: 0,
            notes_cursor: None,
            inserted_cursor: None,
            dirty: Default::default(),
            saving: false,
            changed_at: Instant::now(),
            generation: None,
            template: "general".into(),
            template_pending: false,
            summary_status: None,
        };
        (dir, document, path)
    }
    #[test]
    fn synthetic_5000_turns_layout_is_bounded_by_viewport() {
        use super::*;
        let (_dir, mut document, _path) = synthetic_document(5000);
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut samples = Vec::new();
        let mut maximum = 0;
        let mut layout = TranscriptCache::default();
        for frame in 0..8 {
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::unique("transcript-layout-count"), 0_usize)
            });
            let start = Instant::now();
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    transcript_view(ui, &mut layout, &mut document, false, None, true, true);
                },
            )
            .drop_without_applying_deltas();
            let count = ctx
                .data(|data| data.get_temp::<usize>(egui::Id::unique("transcript-layout-count")))
                .unwrap();
            if frame >= 2 {
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            maximum = maximum.max(count);
            assert!(count > 0 && count <= 32, "Laid out {count} of 5000 turns");
            assert!(
                ctx.data(|data| data.get_temp::<f32>(egui::Id::unique("transcript-viewport-width")))
                    .unwrap() > 900.0,
                "virtual content must retain the full viewport width"
            );
        }
        samples.sort_by(f64::total_cmp);
        eprintln!(
            "5000-turn transcript median warm frame: {:.3} ms; max laid out: {maximum}/5000",
            samples[samples.len() / 2]
        );
    }
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
                            result = {
                                let response = super::recording_pill(
                                    ui,
                                    std::time::Duration::from_secs(48),
                                    0.3,
                                    super::Phase::Recording,
                                    None,
                                );
                                (response.0, response.1)
                            };
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
                .any(|(own, text, _)| *own && text.contains("Не забыть проверить бюджет"))
        );
        assert!(
            runs.iter()
                .any(|(own, text, _)| *own && text.contains("Переходим на новый пайплайн"))
        );
        assert!(
            runs.iter()
                .any(|(own, text, _)| !*own && text.contains("Запуск нового пайплайна в пятницу"))
        );
        let rendered: String = runs.iter().map(|(_, text, _)| text.as_str()).collect();
        assert_eq!(rendered.matches("Переходим на новый пайплайн").count(), 1);
        assert!(rendered.ends_with(enhanced));
    }

    #[test]
    fn empty_personal_notes_leave_all_generated_text_gray() {
        let runs = super::document_runs("", "## Решения\n- Готово\n");
        assert_eq!(
            runs.iter()
                .map(|(own, text, _)| (*own, text.clone()))
                .collect::<Vec<_>>(),
            vec![(false, "## Решения\n- Готово\n".into())]
        );
    }

    #[test]
    fn link_clicks_are_handed_to_the_system_opener_and_only_web_addresses_pass() {
        let ctx = egui::Context::default();
        let mut urls = Vec::new();
        ctx.run_ui(Default::default(), |ui| {
            for url in [
                "https://code.claude.com/docs",
                "file:///etc/passwd",
                "mailto:a@b.c",
            ] {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            urls = take_urls(ui.ctx());
            // Nothing is left for eframe, which is built without its own opener.
            assert!(take_urls(ui.ctx()).is_empty());
        })
        .drop_without_applying_deltas();
        assert_eq!(urls, ["https://code.claude.com/docs", "mailto:a@b.c"]);
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
            template_pending: false,
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
                        false,
                    );
                    assert_eq!(imported.0.len(), usize::from(focused));
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
                let job = super::note_layout(
                    source,
                    p,
                    170.0,
                    &notetaker_core::parse_task_lines(source, chrono::Utc::now().date_naive()),
                );
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
                                fonts.layout_job(super::note_layout(
                                    buffer.as_str(),
                                    p,
                                    width,
                                    &notetaker_core::parse_task_lines(
                                        buffer.as_str(),
                                        chrono::Utc::now().date_naive(),
                                    ),
                                ))
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
        assert!(!indicator_visible(
            Phase::Recording,
            false,
            true,
            false,
            false
        ));
        assert!(indicator_visible(
            Phase::Recording,
            true,
            true,
            false,
            false
        ));
        assert!(indicator_visible(
            Phase::Recording,
            false,
            false,
            false,
            false
        ));
        assert!(!indicator_visible(
            Phase::Recording,
            true,
            false,
            true,
            false
        ));
        for phase in [Phase::Idle, Phase::Starting, Phase::Finalizing] {
            assert!(!indicator_visible(phase, true, false, false, false));
        }
    }
    #[test]
    fn notification_position_is_inside_visible_main_and_never_reuses_hidden_geometry() {
        let size = egui::vec2(340.0, 140.0);
        for origin in [80.0, 3100.0, -1840.0] {
            let main =
                egui::Rect::from_min_size(egui::pos2(origin, 100.0), egui::vec2(1120.0, 760.0));
            let position = notification_position(Some(main), false, false, size).unwrap();
            let card = egui::Rect::from_min_size(position, size);
            assert!(main.shrink(theme::SPACE[3]).contains_rect(card));
            assert_eq!(
                position,
                main.right_bottom() - egui::vec2(theme::SPACE[3], theme::SPACE[3]) - size
            );
            assert_eq!(notification_position(Some(main), true, false, size), None);
            assert_eq!(notification_position(Some(main), false, true, size), None);
        }
        assert_eq!(notification_position(None, false, false, size), None);
        assert_eq!(
            notification_position(Some(egui::Rect::NOTHING), false, false, size),
            None
        );
        let tiny = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
        assert_eq!(notification_position(Some(tiny), false, false, size), None);
        let (state, _commands, _events) = crate::test_ui::state();
        let mut app = NotetakerApp::with_state(state);
        let mut viewport = egui::ViewportInfo {
            outer_rect: Some(egui::Rect::from_min_size(
                egui::pos2(3100.0, 100.0),
                egui::vec2(1120.0, 760.0),
            )),
            focused: Some(true),
            ..Default::default()
        };
        app.update_main_rect(&viewport);
        let ctx = egui::Context::default();
        app.set_main_visible(&ctx, false);
        assert!(app.main_hidden);
        assert!(app.main_rect.is_none());
        app.update_main_rect(&viewport);
        assert!(
            app.main_hidden && app.main_rect.is_none(),
            "stale focus does not undo hide"
        );
        viewport.focused = Some(false);
        app.update_main_rect(&viewport);
        viewport.focused = Some(true);
        app.update_main_rect(&viewport);
        assert!(
            !app.main_hidden && app.main_rect.is_some(),
            "native reopen accepts fresh geometry"
        );
        viewport.minimized = Some(true);
        app.update_main_rect(&viewport);
        assert!(app.main_rect.is_none());
        app.set_main_visible(&ctx, false);
        app.show_main(&ctx, true);
        assert!(!app.main_hidden);
        assert!(app.main_rect.is_none(), "show waits for a fresh outer rect");
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
        if self.undo_title.is_some() || self.undo_ktalk.is_some() {
            return self.focused_for >= Duration::from_secs(12);
        }
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
    if let Some(existing) = queue.iter_mut().find(|t| {
        t.undo_ktalk.as_ref().map(|undo| undo.sync)
            == toast.undo_ktalk.as_ref().map(|undo| undo.sync)
            && t.level == toast.level
            && t.text == toast.text
            && t.undo_title.as_ref().map(|undo| undo.meeting.path())
                == toast.undo_title.as_ref().map(|undo| undo.meeting.path())
    }) {
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
            undo_title: None,
            undo_ktalk: None,
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
    fn later_tolk_sync_with_the_same_message_keeps_its_own_undo() {
        let root = tempfile::tempdir().unwrap();
        let library = notetaker_core::Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new(
                "Пример",
                chrono::Utc::now(),
            ))
            .unwrap();
        let before = meeting.read_transcript().unwrap();
        let first = meeting.load_transcript().unwrap().revision;
        meeting
            .update_transcript(|text| {
                text.rename_speaker("s1", "Анна Петрова");
            })
            .unwrap();
        let second = meeting.load_transcript().unwrap().revision;
        let mut queue = VecDeque::new();
        for after in [first, second] {
            let mut message = toast(Instant::now(), ToastLevel::Success);
            message.undo_ktalk = Some(crate::ktalk::Undo {
                sync: crate::ktalk::SyncId {
                    meeting: meeting.read_meta().unwrap().id,
                    revision: after,
                },
                meeting: meeting.clone(),
                before: before.clone(),
                after,
            });
            push_toast(&mut queue, message);
        }
        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue.back().unwrap().undo_ktalk.as_ref().unwrap().after,
            second
        );
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

#[cfg(test)]
mod chat_shortcut_tests {
    use super::*;
    #[test]
    fn command_j_focuses_chat_on_home_folder_meeting_and_live_but_not_modal() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let (mut state, _commands, _events) = crate::test_ui::state();
        state.ready = true;
        let root = tempfile::tempdir().unwrap();
        state.settings.library = root.path().into();
        let library = notetaker_core::Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new("Тест", chrono::Utc::now()))
            .unwrap();
        let path = meeting.path().to_owned();
        state.documents.insert(
            path.clone(),
            crate::state::Document::new(crate::worker::DocumentData::read(meeting).unwrap()),
        );
        state.selected = Some(path.clone());
        let mut app = NotetakerApp::with_state(state);
        for scope in 0..4 {
            app.home = scope < 2;
            app.state.folder_filter = (scope == 1).then(|| "Команда".into());
            if scope == 3 {
                app.state.recording.path = Some(path.clone());
                app.state.recording.phase = Phase::Recording;
            }
            ctx.memory_mut(|m| m.surrender_focus(egui::Id::unique("chat-input")));
            let key = egui::Event::Key {
                key: egui::Key::J,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            };
            crate::test_ui::frame(&ctx, vec![key], &mut |ui| app.frame(ui))
                .drop_without_applying_deltas();
            assert!(
                ctx.memory(|m| m.has_focus(egui::Id::unique("chat-input"))),
                "scope {scope}"
            );
        }
        app.state.recording.phase = Phase::Idle;
        app.show_record = true;
        ctx.memory_mut(|m| m.surrender_focus(egui::Id::unique("chat-input")));
        crate::test_ui::frame(
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::J,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
            &mut |ui| app.frame(ui),
        )
        .drop_without_applying_deltas();
        assert!(!ctx.memory(|m| m.has_focus(egui::Id::unique("chat-input"))));
    }
}

pub(crate) fn summary_skip_callout(
    ui: &mut egui::Ui,
    notice: &(String, bool),
    can_create: bool,
) -> (bool, bool) {
    let mut settings = false;
    let mut create = false;
    theme::callout(ui, theme::icons::INFO, |ui| {
        ui.label(&notice.0);
        if notice.1 {
            create = ui
                .add_enabled(
                    can_create,
                    theme::styled_button(
                        Palette::of(ui),
                        "Создать конспект",
                        ButtonStyle::Secondary,
                    ),
                )
                .clicked();
        } else {
            settings = theme::button(ui, "Открыть настройки AI", ButtonStyle::Secondary).clicked();
        }
    });
    (settings, create)
}

#[cfg(test)]
mod calendar_review_tests {
    use super::*;
    fn app() -> (NotetakerApp, std::sync::mpsc::Receiver<Command>) {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.quickstart.models.gigaam = crate::quickstart::Status::Ready;
        state.quickstart.models.parakeet = crate::quickstart::Status::Ready;
        let now = chrono::Utc::now();
        let event = notetaker_core::calendar::CalendarEvent {
            link: notetaker_core::calendar::EventLink {
                uid: "review".into(),
                title: "Встреча".into(),
                attendees: vec![],
                call_url: Some("https://zoom.us/old".into()),
            },
            start: now + chrono::Duration::minutes(3),
            end: now + chrono::Duration::hours(1),
            all_day: false,
            organizer: None,
        };
        state.calendar_events = vec![event];
        state.update_calendar_reminder(now);
        (NotetakerApp::with_state(state), commands)
    }
    fn has_reminder_title(output: &egui::FullOutput) -> bool {
        fn contains_title(shape: &egui::epaint::Shape) -> bool {
            match shape {
                egui::epaint::Shape::Text(text) => text.galley.text() == "Встреча",
                egui::epaint::Shape::Vec(shapes) => shapes.iter().any(contains_title),
                _ => false,
            }
        }
        output
            .shapes
            .iter()
            .any(|shape| contains_title(&shape.shape))
    }
    #[test]
    fn reminder_waits_for_dialog_quickstart_and_settings_to_close() {
        let (mut app, _commands) = app();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        for blocked in 0..3 {
            app.show_record = blocked == 0;
            app.quickstart.open = blocked == 1;
            app.show_settings = blocked == 2;
            for _ in 0..2 {
                let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
                    app.calendar_reminder_overlay(ui.ctx(), bounds);
                    app.call_notification(ui.ctx());
                });
                assert!(!has_reminder_title(&output));
                output.drop_without_applying_deltas();
            }
            assert!(app.state.calendar_reminder().is_some());
        }
        app.show_settings = false;
        app.show_record = false;
        app.quickstart.open = false;
        // A generic modal uses the same condition as the chat shortcut.
        for _ in 0..2 {
            crate::test_ui::frame(&ctx, vec![], &mut |ui| {
                egui::Modal::new(egui::Id::unique("review-modal")).show(ui.ctx(), |ui| {
                    ui.label("Диалог");
                });
            })
            .drop_without_applying_deltas();
        }
        let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            app.calendar_reminder_overlay(ui.ctx(), bounds);
        });
        assert!(!has_reminder_title(&output));
        output.drop_without_applying_deltas();
        let mut shown = false;
        for _ in 0..3 {
            let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
                app.calendar_reminder_overlay(ui.ctx(), bounds);
            });
            if has_reminder_title(&output) {
                shown = true;
            }
            output.drop_without_applying_deltas();
        }
        assert!(shown);
    }
    #[test]
    fn reminder_action_resolves_the_current_link() {
        let (mut app, commands) = app();
        app.state.calendar_events[0].link.call_url = Some("https://zoom.us/new".into());
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            app.calendar_action(ui.ctx(), crate::calendar_ui::Action::Join);
            app.calendar_action(ui.ctx(), crate::calendar_ui::Action::Start);
        });
        output.drop_without_applying_deltas();
        let mut opened = false;
        let mut started = false;
        for command in commands.try_iter() {
            match command {
                Command::OpenCall(url) => {
                    assert_eq!(url, "https://zoom.us/new");
                    opened = true;
                }
                Command::Record(request) => {
                    assert_eq!(
                        request.event.unwrap().link.call_url.as_deref(),
                        Some("https://zoom.us/new")
                    );
                    started = true;
                }
                _ => {}
            }
        }
        assert!(opened && started);
    }
}

#[cfg(test)]
mod direct_action_tests {
    use super::*;
    use crate::worker::{DocumentData, Event};
    fn fixture() -> (
        NotetakerApp,
        std::sync::mpsc::Receiver<Command>,
        std::sync::mpsc::Sender<Event>,
        tempfile::TempDir,
    ) {
        let root = tempfile::tempdir().unwrap();
        let library = notetaker_core::Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new(
                "Встреча",
                chrono::Utc::now(),
            ))
            .unwrap();
        meeting.write_notes("- [ ] Личная задача").unwrap();
        meeting
            .write_enhanced("- [ ] Проверить кластер\n- [x] Готовая задача")
            .unwrap();
        let (mut state, commands, events) = crate::test_ui::state();
        state.ready = true;
        state.settings.library = root.path().into();
        state.llm_configured = true;
        let path = meeting.path().to_owned();
        state.selected = Some(path.clone());
        state
            .documents
            .insert(path, Document::new(DocumentData::read(meeting).unwrap()));
        state.templates = vec!["general".into(), "standup".into()];
        let mut app = NotetakerApp::with_state(state);
        app.home = false;
        app.enhanced_preview = true;
        (app, commands, events, root)
    }
    fn marker_click(ctx: &egui::Context, app: &mut NotetakerApp, text: &str) {
        for _ in 0..3 {
            crate::test_ui::frame(ctx, vec![], &mut |ui| app.frame(ui))
                .drop_without_applying_deltas();
        }
        let output = crate::test_ui::frame(ctx, vec![], &mut |ui| app.frame(ui));
        let text = crate::test_ui::text_rect(&output.shapes, text);
        fn squares(shape: &egui::epaint::Shape, output: &mut Vec<egui::Rect>) {
            match shape {
                egui::epaint::Shape::Rect(rect) => output.push(rect.rect),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        squares(shape, output);
                    }
                }
                _ => {}
            }
        }
        let mut rects = Vec::new();
        for shape in &output.shapes {
            squares(&shape.shape, &mut rects);
        }
        let pos = rects
            .iter()
            .filter(|rect| {
                (10.0..=24.0).contains(&rect.width())
                    && (rect.center().y - text.center().y).abs() < 12.0
                    && rect.center().x < text.left() + 24.0
                    && rect.center().x > text.left() - 40.0
            })
            .map(|rect| rect.center())
            .max_by(|a, b| a.x.total_cmp(&b.x))
            .unwrap_or_else(|| {
                panic!("visible checkbox square near {text:?}; candidates={rects:?}")
            });
        output.drop_without_applying_deltas();
        for pressed in [true, false] {
            crate::test_ui::frame(
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                &mut |ui| app.frame(ui),
            )
            .drop_without_applying_deltas();
        }
    }
    #[track_caller]
    fn finish_toggle(
        app: &mut NotetakerApp,
        commands: &std::sync::mpsc::Receiver<Command>,
        events: &std::sync::mpsc::Sender<Event>,
        fail: bool,
    ) -> (bool, bool) {
        let command = commands
            .try_iter()
            .find(|cmd| matches!(cmd, Command::ToggleTask { .. }))
            .expect("checkbox sends an atomic task edit");
        let Command::ToggleTask {
            meeting,
            task,
            completed,
            notes,
        } = command
        else {
            unreachable!()
        };
        let path = meeting.path().to_owned();
        let result = if fail {
            Err("Ошибка записи".into())
        } else {
            meeting.toggle_task_source(&task, completed, notes).unwrap();
            Ok(Box::new(DocumentData::read(meeting).unwrap()))
        };
        events.send(Event::TaskToggled { path, result }).unwrap();
        app.state.poll();
        for command in commands.try_iter() {
            if let Command::Load(meeting) = command {
                events
                    .send(Event::Loaded {
                        path: meeting.path().into(),
                        result: Ok(DocumentData::read(meeting).unwrap()),
                    })
                    .unwrap();
            }
        }
        app.state.poll();
        let library = notetaker_core::Library::open(&app.state.settings.library).unwrap();
        library.reindex_incremental().unwrap();
        events
            .send(Event::Tasks {
                root: app.state.settings.library.clone(),
                result: Ok(library.list_tasks(None, None, false).unwrap()),
            })
            .unwrap();
        app.state.poll();
        (completed, notes)
    }
    fn compact_frame(
        ctx: &egui::Context,
        app: &mut NotetakerApp,
        size: egui::Vec2,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |ui| app.frame(ui),
        )
    }
    fn open_meeting_menu(ctx: &egui::Context, app: &mut NotetakerApp, size: egui::Vec2) {
        compact_frame(ctx, app, size).drop_without_applying_deltas();
        let pos = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::unique("meeting-menu-rect")))
            .unwrap()
            .center();
        click_at(ctx, app, size, pos);
    }
    fn click_at(ctx: &egui::Context, app: &mut NotetakerApp, size: egui::Vec2, pos: egui::Pos2) {
        for pressed in [true, false] {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| app.frame(ui),
            )
            .drop_without_applying_deltas();
        }
    }
    fn scroll_document(ctx: &egui::Context, app: &mut NotetakerApp, size: egui::Vec2, delta: f32) {
        let rect = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::unique("transcript-viewport")))
            .unwrap();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events: vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, delta),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| app.frame(ui),
        )
        .drop_without_applying_deltas();
        for _ in 0..40 {
            compact_frame(ctx, app, size).drop_without_applying_deltas();
        }
    }
    fn ready_recording(app: &mut NotetakerApp) {
        app.state.quickstart.models = crate::quickstart::Models {
            gigaam: crate::quickstart::Status::Ready,
            parakeet: crate::quickstart::Status::Ready,
            auto: crate::quickstart::Status::Ready,
            diarization: crate::quickstart::Status::Ready,
            semantic: crate::quickstart::Status::Ready,
        };
    }
    #[test]
    fn primary_start_uses_quick_path_and_chevron_opens_existing_options() {
        let (mut app, commands, events, _root) = fixture();
        ready_recording(&mut app);
        app.state.settings.last_recording = Some(
            crate::recording_options::remember(
                &crate::test_ui::recording_inventory(),
                &app.state.settings,
                None,
                None,
            )
            .unwrap(),
        );
        let ctx = egui::Context::default();
        theme::install(&ctx);
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Новая запись");
        assert!(!app.show_record);
        assert!(
            commands
                .try_iter()
                .any(|command| matches!(command, Command::Inventory))
        );
        events
            .send(Event::Inventory(Ok(crate::test_ui::recording_inventory())))
            .unwrap();
        app.state.poll();
        assert!(
            commands
                .try_iter()
                .any(|command| matches!(command, Command::Record(_)))
        );
        let (mut app, _commands, _events, _root) = fixture();
        ready_recording(&mut app);
        let size = egui::vec2(1280.0, 900.0);
        compact_frame(&ctx, &mut app, size).drop_without_applying_deltas();
        let pos = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::unique("record-options-rect")))
            .unwrap()
            .center();
        click_at(&ctx, &mut app, size, pos);
        assert!(app.show_record);
    }
    #[test]
    fn countdown_card_buttons_continue_or_stop_without_overlapping_the_bar() {
        let (mut app, _commands, _events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        app.state.recording.phase = Phase::Recording;
        app.state.recording.path = app.state.selected.clone();
        app.state.call_prompt = Some(CallPrompt::Stop {
            pid: 42,
            started_at: 100,
            countdown: crate::detection::Countdown::new(Instant::now()),
        });
        let size = egui::vec2(640.0, 427.0);
        for _ in 0..3 {
            compact_frame(&ctx, &mut app, size).drop_without_applying_deltas();
        }
        let output = compact_frame(&ctx, &mut app, size);
        let button = crate::test_ui::text_rect(&output.shapes, "Продолжить запись");
        assert!(button.bottom() < app.toast_anchor.unwrap().y);
        output.drop_without_applying_deltas();
        click_at(&ctx, &mut app, size, button.center());
        assert!(app.state.call_prompt.is_none());
        assert!(!app.state.recording.stop_requested);
        app.state.call_prompt = Some(CallPrompt::Stop {
            pid: 42,
            started_at: 100,
            countdown: crate::detection::Countdown::new(Instant::now()),
        });
        let output = compact_frame(&ctx, &mut app, size);
        let button = crate::test_ui::text_rect(&output.shapes, "Остановить сейчас");
        output.drop_without_applying_deltas();
        click_at(&ctx, &mut app, size, button.center());
        assert!(app.state.recording.stop_requested);
    }
    #[test]
    fn a_new_sync_replaces_the_old_toast_and_revision_changes_remove_undo() {
        let (mut app, _commands, _events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let meeting = app.state.documents[&path].data.meeting.clone();
        let original = meeting.read_transcript().unwrap();
        let id = meeting.read_meta().unwrap().id;
        let first = meeting.load_transcript().unwrap().revision;
        let old = crate::ktalk::Undo {
            sync: crate::ktalk::SyncId {
                meeting: id,
                revision: first,
            },
            meeting: meeting.clone(),
            before: original.clone(),
            after: first,
        };
        app.state.ktalk.successes.push_back((old.clone(), 1));
        let ctx = egui::Context::default();
        theme::install(&ctx);
        crate::test_ui::frame(&ctx, vec![], &mut |ui| app.notifications(ui.ctx()))
            .drop_without_applying_deltas();
        assert_eq!(
            app.toasts
                .iter()
                .filter(|toast| toast.undo_ktalk.is_some())
                .count(),
            1
        );
        meeting
            .update_transcript(|text| {
                text.rename_speaker("me", "Анна");
            })
            .unwrap();
        let loaded = meeting.load_transcript().unwrap();
        let newer = crate::ktalk::Undo {
            sync: crate::ktalk::SyncId {
                meeting: id,
                revision: loaded.revision,
            },
            meeting: meeting.clone(),
            before: original,
            after: loaded.revision,
        };
        let document = app.state.documents.get_mut(&path).unwrap();
        document.data.transcript = loaded.value;
        document.data.revisions.transcript = loaded.revision;
        app.state.ktalk.successes.push_back((newer.clone(), 1));
        crate::test_ui::frame(&ctx, vec![], &mut |ui| app.notifications(ui.ctx()))
            .drop_without_applying_deltas();
        let toasts: Vec<_> = app
            .toasts
            .iter()
            .filter_map(|toast| toast.undo_ktalk.as_ref())
            .collect();
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].sync, newer.sync);
        app.state.ktalk.undo_visible.remove(&old.sync);
        assert!(app.state.ktalk.undo_visible.contains(&newer.sync));
        meeting
            .update_transcript(|text| {
                text.rename_speaker("me", "Другое имя");
            })
            .unwrap();
        app.state
            .documents
            .get_mut(&path)
            .unwrap()
            .data
            .revisions
            .transcript = meeting.load_transcript().unwrap().revision;
        crate::test_ui::frame(&ctx, vec![], &mut |ui| app.notifications(ui.ctx()))
            .drop_without_applying_deltas();
        assert!(app.toasts.iter().all(|toast| toast.undo_ktalk.is_none()));
    }
    #[test]
    fn welcome_has_one_preparation_line_and_disabled_hint_explains_why() {
        let (mut app, _commands, _events, _root) = fixture();
        app.home = true;
        app.quickstart.open = true;
        app.state.downloading = true;
        app.state.quickstart.models = Default::default();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        fn count(shape: &egui::epaint::Shape) -> usize {
            match shape {
                egui::epaint::Shape::Text(text) => {
                    usize::from(text.galley.text().starts_with("Подготовка к записи:"))
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().map(count).sum(),
                _ => 0,
            }
        }
        let mut lines = 0;
        for _ in 0..2 {
            let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| app.frame(ui));
            lines = output.shapes.iter().map(|shape| count(&shape.shape)).sum();
            output.drop_without_applying_deltas();
        }
        assert_eq!(lines, 1);
        crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            crate::detection::start_card(ui, "Chrome", None, Some("Дождитесь проверки звука"));
        })
        .drop_without_applying_deltas();
    }
    #[test]
    fn first_run_blocks_pending_call_until_models_and_interactions_are_ready() {
        let (mut app, _commands, _events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        app.state.quickstart.models = crate::quickstart::Models {
            auto: crate::quickstart::Status::Ready,
            gigaam: crate::quickstart::Status::Ready,
            parakeet: crate::quickstart::Status::Ready,
            ..Default::default()
        };
        app.state.call_prompt = Some(CallPrompt::Start {
            started_at: 1,
            app: "Chrome".into(),
            pid: 1,
            meeting_title: None,
        });
        app.quickstart.open = true;
        assert!(!app.start_hint_visible(&ctx));
        app.quickstart.open = false;
        app.show_settings = true;
        assert!(!app.start_hint_visible(&ctx));
        app.show_settings = false;
        app.show_record = true;
        assert!(!app.start_hint_visible(&ctx));
        app.show_record = false;
        app.state.quickstart.models = Default::default();
        assert!(!app.start_hint_visible(&ctx));
        app.state.quickstart.models.auto = crate::quickstart::Status::Ready;
        app.state.record_model = "auto".into();
        assert!(app.start_hint_visible(&ctx));
        assert!(app.state.call_prompt.is_some());
        app.state.dismiss_call();
        assert!(app.state.call_prompt.is_none());
    }
    #[test]
    fn ktalk_success_toast_queues_undo_while_busy_and_restores_the_transcript() {
        let (mut app, commands, _events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let (before, plan) = crate::ktalk_fixture::plan();
        let meeting = app.state.documents[&path].data.meeting.clone();
        meeting.write_transcript(&before).unwrap();
        meeting
            .update_meta(|meta| meta.ended_at = Some(chrono::Utc::now()))
            .unwrap();
        crate::ktalk::register_recording(&meeting).unwrap();
        let task = crate::ktalk::Task {
            meeting: meeting.clone(),
            ended: chrono::Utc::now(),
            progress: Default::default(),
        };
        app.state.ktalk_result(crate::ktalk::run(
            crate::ktalk::Job::Apply {
                task,
                plan: Box::new(plan),
                voices: false,
            },
            &notetaker_llm::OsCredentialStore,
        ));
        let ctx = egui::Context::default();
        theme::install(&ctx);
        app.state.ktalk.busy = true;
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Отменить");
        assert!(app.state.ktalk.undo_pending.is_some());
        app.state.poll_ktalk();
        assert!(app.state.ktalk.undo_pending.is_some());
        app.state
            .ktalk_result(crate::ktalk::Message::Checked("Олег Сидоров".into()));
        app.state.poll_ktalk();
        let job = commands
            .try_iter()
            .find_map(|command| {
                if let Command::Ktalk(job, _) = command {
                    Some(job)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(matches!(*job, crate::ktalk::Job::Undo(_)));
        app.state
            .ktalk_result(crate::ktalk::run(*job, &notetaker_llm::OsCredentialStore));
        assert_eq!(meeting.read_transcript().unwrap(), before);
        assert!(app.toasts.iter().all(|toast| toast.undo_ktalk.is_none()));
    }
    #[test]
    fn title_toast_undo_restores_previous_title_and_is_offered_once() {
        let (mut app, commands, events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let path = app.state.selected.clone().unwrap();
        let doc = app.state.documents.get_mut(&path).unwrap();
        let meeting = doc.data.meeting.clone();
        let old = doc.data.meta.title.clone();
        let generated = "План выпуска";
        meeting
            .update_meta(|meta| {
                meta.title = generated.into();
                meta.title_source = Some(notetaker_core::TitleSource::Generated);
            })
            .unwrap();
        doc.data.meta = meeting.read_meta().unwrap();
        app.state.title_changes.push_back(crate::state::TitleUndo {
            meeting: meeting.clone(),
            previous: old.clone(),
            generated: generated.into(),
        });
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Вернуть название");
        let Command::Rename {
            meeting,
            title,
            expected,
        } = commands
            .try_iter()
            .find(|command| matches!(command, Command::Rename { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(expected.as_deref(), Some(generated));
        meeting.rename_title(&title, expected.as_deref()).unwrap();
        events
            .send(Event::Renamed {
                path: path.clone(),
                result: Ok(title),
            })
            .unwrap();
        app.state.poll();
        assert_eq!(meeting.read_meta().unwrap().title, old);
        assert_eq!(app.state.documents[&path].data.meta.title, old);
        assert!(app.state.title_changes.is_empty());
        assert!(app.toasts.iter().all(|toast| toast.undo_title.is_none()));
    }
    #[test]
    fn compact_mode_stays_stable_at_fixed_sizes_with_changing_bar_contents() {
        let (mut app, _commands, _events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        for width in [640.0, 900.0, 1280.0] {
            for height in [
                427.0, 590.0, 600.0, 639.0, 640.0, 641.0, 680.0, 699.0, 700.0, 701.0, 760.0,
            ] {
                let mut settled = None;
                for frame in 0..12 {
                    app.state.recording.phase = match frame % 3 {
                        0 => Phase::Idle,
                        1 => Phase::Recording,
                        _ => Phase::Finalizing,
                    };
                    app.state.recording.path = app.state.selected.clone();
                    app.state.chat.busy = frame % 2 == 0;
                    if let Some(document) = app
                        .state
                        .selected
                        .as_ref()
                        .and_then(|path| app.state.documents.get_mut(path))
                    {
                        document.template_pending = frame % 2 == 0;
                    }
                    compact_frame(&ctx, &mut app, egui::vec2(width, height))
                        .drop_without_applying_deltas();
                    if frame >= 2 {
                        let expected = *settled.get_or_insert(app.meeting_layout.compact);
                        assert_eq!(
                            app.meeting_layout.compact, expected,
                            "size={width}x{height}, frame={frame}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn neutral_height_prevents_the_480_pixel_margin_loop() {
        let ctx = egui::Context::default();
        let mut layout = MeetingLayout::default();
        for _ in 0..20 {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    layout.update(ui.available_height());
                    egui::Panel::bottom("fixed-bar")
                        .exact_size(120.0)
                        .frame(egui::Frame::NONE)
                        .show(ui, |_| {});
                    assert_eq!(ui.available_height(), 480.0);
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE.inner_margin(if layout.compact { 8 } else { 48 }))
                        .show(ui, |_| {});
                },
            )
            .drop_without_applying_deltas();
            assert!(layout.compact);
        }
    }
    #[test]
    fn compact_metadata_returns_at_top_and_folder_picker_moves_to_menu() {
        let (mut app, _commands, _events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let doc = app.state.documents.get_mut(&path).unwrap();
        let template = notetaker_core::Segment {
            id: uuid::Uuid::now_v7(),
            speaker: notetaker_core::SpeakerId("me".into()),
            channel: notetaker_core::Channel::Mic,
            start_ms: 0,
            end_ms: 9000,
            text: "Реплика с текстом для прокрутки".into(),
            source: notetaker_core::Source::Final,
            edited: false,
            lang: None,
        };
        doc.data.transcript.segments = (0..30)
            .map(|index| {
                let mut segment = template.clone();
                segment.id = uuid::Uuid::now_v7();
                segment.start_ms = index * 10000;
                segment
            })
            .collect();
        doc.changed();
        app.state.tab = Tab::Transcript;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let size = egui::vec2(640.0, 427.0);
        for _ in 0..4 {
            compact_frame(&ctx, &mut app, size).drop_without_applying_deltas();
        }
        let output = compact_frame(&ctx, &mut app, size);
        crate::test_ui::text_rect(&output.shapes, "Без папки");
        output.drop_without_applying_deltas();
        assert!(app.meeting_layout.metadata_visible);
        scroll_document(&ctx, &mut app, size, -200.0);
        assert!(!app.meeting_layout.metadata_visible);
        open_meeting_menu(&ctx, &mut app, size);
        let output = compact_frame(&ctx, &mut app, size);
        let folder = crate::test_ui::text_rect(&output.shapes, "Без папки").center();
        let date = app.state.documents[&path]
            .data
            .meta
            .started_at
            .with_timezone(&chrono::Local)
            .format("%d.%m.%Y · %H:%M")
            .to_string();
        crate::test_ui::text_rect(&output.shapes, &date);
        output.drop_without_applying_deltas();
        click_at(&ctx, &mut app, size, folder);
        let opened = compact_frame(&ctx, &mut app, size);
        assert!(opened.shapes.iter().filter(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text() == "Без папки")).count() >= 2, "folder picker opens inside the meeting menu");
        opened.drop_without_applying_deltas();
        egui::Popup::close_all(&ctx);
        scroll_document(&ctx, &mut app, size, 10000.0);
        assert!(app.meeting_layout.metadata_visible);
    }
    #[test]
    fn meeting_menu_checks_audio_again_during_another_recording_after_sweep() {
        let (mut app, _commands, _events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let meeting = app.state.documents[&path].data.meeting.clone();
        let now = chrono::Utc::now();
        let expiry = now + chrono::Duration::hours(1);
        meeting
            .update_meta(|meta| {
                meta.ended_at = Some(now);
                meta.audio_expires_at = Some(expiry);
            })
            .unwrap();
        std::fs::create_dir_all(meeting.mic_audio_path().parent().unwrap()).unwrap();
        std::fs::write(meeting.mic_audio_path(), b"fixture audio").unwrap();
        app.state.documents.insert(
            path.clone(),
            Document::new(DocumentData::read(meeting.clone()).unwrap()),
        );
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let size = egui::vec2(1280.0, 900.0);
        open_meeting_menu(&ctx, &mut app, size);
        let before = compact_frame(&ctx, &mut app, size);
        crate::test_ui::text_rect(
            &before.shapes,
            &crate::wording::audio_status(true, Some(expiry)),
        );
        before.drop_without_applying_deltas();
        let library = notetaker_core::Library::open(&app.state.settings.library).unwrap();
        let other = library
            .create_meeting(notetaker_core::MeetingMeta::new("B", now))
            .unwrap();
        app.state.recording.phase = Phase::Recording;
        app.state.recording.path = Some(other.path().into());
        library
            .sweep_audio(expiry + chrono::Duration::seconds(1))
            .unwrap();
        assert!(!meeting.mic_audio_path().is_file());
        assert!(app.state.recording.busy());
        let after = compact_frame(&ctx, &mut app, size);
        crate::test_ui::text_rect(&after.shapes, "Аудио не хранится");
        after.drop_without_applying_deltas();
    }
    #[test]
    fn compact_header_has_hysteresis_and_prompt_budget_is_stable() {
        let mut layout = MeetingLayout::default();
        for (height, expected) in [
            (800.0, false),
            (670.0, false),
            (639.0, true),
            (640.0, true),
            (670.0, true),
            (700.0, true),
            (701.0, false),
            (641.0, false),
        ] {
            layout.update(height);
            assert_eq!(layout.compact, expected, "height={height}");
        }
        layout.update(300.0);
        layout.document_height(95.0, 96.0);
        assert!(layout.hide_prompts);
        layout.document_height(125.0, 96.0);
        assert!(layout.hide_prompts);
        layout.document_height(145.0, 96.0);
        assert!(!layout.hide_prompts);
        layout.document_height(110.0, 96.0);
        assert!(!layout.hide_prompts);
    }
    #[test]
    fn compact_transcript_shows_three_complete_turns_at_150_percent() {
        let (mut app, _commands, _events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let doc = app.state.documents.get_mut(&path).unwrap();
        doc.data.transcript.segments = [
            "Первый запуск должен быть коротким. Давайте оставим только самое нужное.",
            "Согласен. Запись должна работать без выбора моделей. Остальное можно подключить позже.",
            "Сделаем основной текст крупнее и проверим маленькое окно.",
            "Календарь и AI оставим необязательными. Это поможет быстрее начать первую встречу.",
        ].into_iter().enumerate().map(|(index,text)| notetaker_core::Segment {
            id:uuid::Uuid::now_v7(), speaker:notetaker_core::SpeakerId(format!("speaker_{index}")),
            channel:notetaker_core::Channel::System, start_ms:index as u64 * 10000,end_ms:index as u64 * 10000 + 9000,
            text:text.into(), source:notetaker_core::Source::Final,edited:false,lang:None,
        }).collect();
        doc.changed();
        app.state.tab = Tab::Transcript;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        for _ in 0..8 {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 427.0),
                    )),
                    ..Default::default()
                },
                |ui| app.frame(ui),
            )
            .drop_without_applying_deltas();
        }
        let viewport = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::unique("transcript-viewport")))
            .unwrap();
        for segment in app.state.documents[&path]
            .data
            .transcript
            .segments
            .iter()
            .take(3)
        {
            let response = ctx
                .read_response(transcript_text_id(&path, segment.id))
                .unwrap();
            assert!(
                response.rect.top() >= viewport.top()
                    && response.rect.bottom() <= viewport.bottom(),
                "turn={} rect={:?} viewport={viewport:?}",
                segment.start_ms,
                response.rect
            );
        }
    }
    #[test]
    fn compact_meeting_keeps_tabs_and_removes_header_template_select() {
        let (mut app, _commands, _events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        for _ in 0..4 {
            let out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 427.0),
                    )),
                    ..Default::default()
                },
                |ui| app.frame(ui),
            );
            assert!(app.meeting_layout.compact);
            for label in ["Конспект", "Транскрипт", "Заметки"] {
                let rect = crate::test_ui::text_rect(&out.shapes, label);
                assert!(rect.bottom() < 427.0);
            }
            assert!(!out.shapes.iter().any(|shape| matches!(&shape.shape,egui::epaint::Shape::Text(text) if text.galley.text().contains("Общий"))));
            out.drop_without_applying_deltas();
        }
    }
    #[test]
    fn summary_checkbox_click_space_failure_and_notes_use_atomic_source() {
        let (mut app, commands, events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        marker_click(&ctx, &mut app, "Проверить кластер");
        assert_eq!(
            finish_toggle(&mut app, &commands, &events, false),
            (true, false)
        );
        assert!(
            app.state
                .tasks
                .iter()
                .find(|t| t.text == "Проверить кластер")
                .unwrap()
                .completed
        );
        crate::test_ui::frame(
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut |ui| app.frame(ui),
        )
        .drop_without_applying_deltas();
        assert_eq!(
            finish_toggle(&mut app, &commands, &events, false),
            (false, false)
        );
        marker_click(&ctx, &mut app, "Проверить кластер");
        finish_toggle(&mut app, &commands, &events, true);
        assert!(app.state.error.as_ref().unwrap().contains("Ошибка записи"));
        marker_click(&ctx, &mut app, "Проверить кластер");
        assert_eq!(
            finish_toggle(&mut app, &commands, &events, false),
            (true, false)
        );
        app.state.error = None;
        marker_click(&ctx, &mut app, "Личная задача");
        assert_eq!(
            finish_toggle(&mut app, &commands, &events, false),
            (true, true)
        );
        let path = app.state.selected.as_ref().unwrap();
        assert!(app.state.documents[path].data.notes.contains("[x]"));
    }
    #[test]
    fn template_choice_bar_dismiss_readiness_and_manual_confirmation() {
        let (mut app, commands, _events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        crate::test_ui::click(
            &ctx,
            &mut |ui| app.frame(ui),
            &format!("{} Общий", theme::icons::FILE_TEXT),
        );
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Стендап");
        let path = app.state.selected.clone().unwrap();
        assert!(app.state.documents[&path].template_pending);
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Не сейчас");
        assert!(!app.state.documents[&path].template_pending);
        app.state.documents.get_mut(&path).unwrap().template_pending = true;
        app.state.llm.providers[0].base_url = "https://example.invalid/v1".into();
        let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| app.frame(ui));
        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::epaint::Shape::Text(text) if text.galley.text().contains("loopback"))));
        output.drop_without_applying_deltas();
        app.state.llm.providers[0].base_url = "http://localhost:11434/v1".into();
        crate::test_ui::click(
            &ctx,
            &mut |ui| app.frame(ui),
            "Пересоздать по шаблону «Стендап»",
        );
        assert!(app.confirm_regenerate);
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Пересоздать");
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd, Command::Enhance { .. }))
        );
        assert!(!app.state.documents[&path].template_pending);
    }
    #[test]
    fn nested_task_and_generated_template_use_existing_paths() {
        let (mut app, commands, events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let path = app.state.selected.clone().unwrap();
        let meeting = app.state.documents[&path].data.meeting.clone();
        meeting
            .write_enhanced("- [ ] Родитель\n  - [ ] Вложенная")
            .unwrap();
        app.state.documents.insert(
            path.clone(),
            Document::new(DocumentData::read(meeting).unwrap()),
        );
        marker_click(&ctx, &mut app, "Вложенная");
        assert_eq!(
            finish_toggle(&mut app, &commands, &events, false),
            (true, false)
        );
        assert!(
            app.state
                .tasks
                .iter()
                .find(|t| t.text == "Вложенная")
                .unwrap()
                .completed
        );
        let doc = app.state.documents.get_mut(&path).unwrap();
        doc.data.meta.summary = Some(notetaker_core::SummaryStamp::new(
            &doc.data.notes,
            &doc.data.transcript,
            &doc.template,
            &doc.data.enhanced,
        ));
        crate::test_ui::click(
            &ctx,
            &mut |ui| app.frame(ui),
            &format!("{} Общий", theme::icons::FILE_TEXT),
        );
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Стендап");
        crate::test_ui::click(
            &ctx,
            &mut |ui| app.frame(ui),
            "Пересоздать по шаблону «Стендап»",
        );
        assert!(!app.confirm_regenerate);
        assert!(
            commands
                .try_iter()
                .any(|cmd| matches!(cmd,Command::Enhance {template,..} if template=="standup"))
        );
    }
    #[test]
    fn selecting_template_without_summary_has_no_apply_bar() {
        let (mut app, _commands, _events, _root) = fixture();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let path = app.state.selected.clone().unwrap();
        app.state
            .documents
            .get_mut(&path)
            .unwrap()
            .data
            .enhanced
            .clear();
        crate::test_ui::click(
            &ctx,
            &mut |ui| app.frame(ui),
            &format!("{} Общий", theme::icons::FILE_TEXT),
        );
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Стендап");
        assert_eq!(app.state.documents[&path].template, "standup");
        assert!(!app.state.documents[&path].template_pending);
    }
    #[test]
    fn image_task_keeps_the_following_checkbox_bound_to_its_source() {
        let (mut app, commands, events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let meeting = app.state.documents[&path].data.meeting.clone();
        meeting.write_notes("").unwrap();
        let original = "> - [ ] ![Скриншот](attachments/a.png)\n> - [ ] Проверить";
        meeting.write_enhanced(original).unwrap();
        app.state.documents.insert(
            path.clone(),
            Document::new(DocumentData::read(meeting).unwrap()),
        );
        let ctx = egui::Context::default();
        theme::install(&ctx);
        marker_click(&ctx, &mut app, "Проверить");
        assert_eq!(
            finish_toggle(&mut app, &commands, &events, false),
            (true, false)
        );
        assert_eq!(
            app.state.documents[&path].data.enhanced,
            original.replacen("[ ] Проверить", "[x] Проверить", 1)
        );
        let sources: Vec<_> = app
            .document_preview
            .blocks
            .iter()
            .filter_map(|block| block.source.as_ref())
            .map(|(_, task)| task.line)
            .collect();
        assert_eq!(sources, [1, 2]);
    }

    #[test]
    fn notes_nested_ordered_quoted_markers_toggle_their_original_lines() {
        let (mut app, commands, events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let meeting = app.state.documents[&path].data.meeting.clone();
        meeting
            .write_notes("- [ ] Родитель\n  - [ ] Вложенная\n\n1. [ ] По порядку\n\n> - [ ] Цитата")
            .unwrap();
        app.state.documents.insert(
            path.clone(),
            Document::new(DocumentData::read(meeting).unwrap()),
        );
        let ctx = egui::Context::default();
        theme::install(&ctx);
        crate::test_ui::click(&ctx, &mut |ui| app.frame(ui), "Заметки");
        for (name, line) in [("Вложенная", 2), ("По порядку", 4), ("Цитата", 6)]
        {
            // Notes use a single galley; locate the checkbox from its original line snapshot.
            for _ in 0..3 {
                crate::test_ui::frame(&ctx, vec![], &mut |ui| app.frame(ui))
                    .drop_without_applying_deltas();
            }
            let task = app
                .notes_cache
                .markers
                .iter()
                .find_map(|(pos, task)| {
                    task.as_ref().filter(|task| task.line == line).map(|_| *pos)
                })
                .unwrap();
            let id = egui::Id::unique(("notes_edit", path.as_path()));
            let rect = ctx.read_response(id).unwrap().rect;
            let pos = task + rect.min.to_vec2();
            for pressed in [true, false] {
                crate::test_ui::frame(
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    &mut |ui| app.frame(ui),
                )
                .drop_without_applying_deltas();
            }
            assert_eq!(
                finish_toggle(&mut app, &commands, &events, false),
                (true, true)
            );
            assert!(
                app.state.documents[&path]
                    .data
                    .notes
                    .lines()
                    .nth(line - 1)
                    .unwrap()
                    .contains(&format!("[x] {name}"))
            );
        }
    }

    #[test]
    fn rendered_marker_snapshots_match_mixed_source_documents() {
        let (mut app, _commands, _events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let doc = app.state.documents.get_mut(&path).unwrap();
        for text in [
            "> - [ ] ![Скриншот](attachments/a.png)\n> - [ ] Проверить",
            "- [ ] Родитель\r\n  - [x] Вложенная 🦀\r\n\r\n1. [ ] Проверить [ ] текст\r\n",
            "- [ ] Повтор\n- [ ] Повтор\n\n> - [X] Ёжик\n\n```md\n- [ ] Код\n```\n",
            "~~~\n- [ ] Код\n\n- [x] Ещё код\n~~~\n\n* [ ] ![图](attachments/b.png)\n",
        ] {
            for notes in [false, true] {
                doc.data.notes = if notes { text } else { "" }.into();
                doc.data.enhanced = if notes { "" } else { text }.into();
                doc.changed();
                let mut preview = DocumentPreview::default();
                preview.prepare(doc, &path);
                let rendered: Vec<_> = preview
                    .blocks
                    .iter()
                    .filter_map(|block| block.source.as_ref())
                    .collect();
                let expected =
                    notetaker_core::parse_task_lines(text, doc.data.meta.started_at.date_naive());
                assert_eq!(rendered.len(), expected.len());
                for ((source_notes, task), source) in rendered.into_iter().zip(expected) {
                    assert_eq!(*source_notes, notes);
                    assert_eq!(task.path, path);
                    assert_eq!(task.line, source.task.line);
                    assert_eq!(task.source, source.task.source);
                    assert_eq!(
                        task.revision,
                        if notes {
                            doc.data.revisions.notes
                        } else {
                            doc.data.revisions.enhanced
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn fenced_tasks_are_plain_text_and_done_notes_have_the_summary_style() {
        let (mut app, _commands, _events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let doc = app.state.documents.get_mut(&path).unwrap();
        let text = "```md\n- [ ] Код\n\n- [x] Ещё код\n```\n\n- [x] Готово";
        doc.data.notes = text.into();
        doc.data.enhanced = text.into();
        doc.changed();
        let mut preview = DocumentPreview::default();
        preview.prepare(doc, &path);
        let sources: Vec<_> = preview
            .blocks
            .iter()
            .filter_map(|block| block.source.as_ref())
            .map(|(_, task)| task.line)
            .collect();
        assert_eq!(sources, [7]);
        assert!(preview.blocks.iter().any(|block| matches!(&block.kind, DocumentBlockKind::Markdown(markdown) if markdown.markdown.contains("```md\n- [ ] Код\n\n- [x] Ещё код\n```"))));
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut cache = NotesCache::default();
        crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            cache.layout(ui, text, 500.0);
        })
        .drop_without_applying_deltas();
        assert_eq!(
            cache
                .markers
                .iter()
                .filter_map(|(_, task)| task.as_ref().map(|task| task.line))
                .collect::<Vec<_>>(),
            [7]
        );
        for dark in [false, true] {
            let p = Palette::for_dark(dark);
            let tasks = notetaker_core::parse_task_lines(text, chrono::Utc::now().date_naive());
            let layout = note_layout(text, p, 500.0, &tasks);
            assert_eq!(layout.text, text);
            let done = layout
                .sections
                .iter()
                .find(|section| {
                    layout.text[section.byte_range.start.0..section.byte_range.end.0]
                        .contains("Готово")
                })
                .unwrap();
            assert_eq!(done.format.color, p.muted);
            assert_eq!(done.format.strikethrough, egui::Stroke::new(1.0, p.muted));
            let code = layout
                .sections
                .iter()
                .find(|section| {
                    layout.text[section.byte_range.start.0..section.byte_range.end.0]
                        .contains("Код")
                })
                .unwrap();
            assert_eq!(code.format.strikethrough, egui::Stroke::NONE);
        }
    }

    #[test]
    fn stale_render_snapshot_is_rejected_and_the_view_is_reloaded() {
        let (mut app, commands, events, _root) = fixture();
        let path = app.state.selected.clone().unwrap();
        let meeting = app.state.documents[&path].data.meeting.clone();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        marker_click(&ctx, &mut app, "Проверить кластер");
        let Command::ToggleTask {
            task,
            completed,
            notes,
            ..
        } = commands
            .try_iter()
            .find(|cmd| matches!(cmd, Command::ToggleTask { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        let changed = "- [ ] Проверить кластер\nИзменено извне";
        meeting.write_enhanced(changed).unwrap();
        let error = meeting
            .toggle_task_source(&task, completed, notes)
            .unwrap_err()
            .to_string();
        events
            .send(Event::TaskToggled {
                path: path.clone(),
                result: Err(error),
            })
            .unwrap();
        app.state.poll();
        assert!(
            app.state
                .error
                .as_ref()
                .unwrap()
                .contains("Документ изменился")
        );
        let Command::Load(meeting) = commands
            .try_iter()
            .find(|cmd| matches!(cmd, Command::Load(_)))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(meeting.load_enhanced().unwrap().value, changed);
        events
            .send(Event::Loaded {
                path: path.clone(),
                result: Ok(DocumentData::read(meeting).unwrap()),
            })
            .unwrap();
        app.state.poll();
        assert_eq!(app.state.documents[&path].data.enhanced, changed);
    }

    #[test]
    fn source_lines_do_not_depend_on_identical_text_or_checkbox_state() {
        let (app, _commands, _events, _root) = fixture();
        let path = app.state.selected.as_ref().unwrap();
        let doc = &app.state.documents[path];
        let runs = document_runs("- [ ] A\n- [ ] B", "- [x] A\n- [ ] C");
        let sources = runs
            .iter()
            .flat_map(|(_, _, sources)| sources.iter().copied())
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(sources, vec![(true, 2), (false, 1), (false, 2)]);
        assert_eq!(document_tasks(doc).get(&(false, 1)).unwrap().task.line, 1);
    }
}

#[cfg(test)]
mod calendar_midnight_tests {
    use super::*;
    use chrono::TimeZone;
    #[test]
    fn configured_calendar_schedules_a_local_midnight_wake() {
        let (mut state, _, _) = crate::test_ui::state();
        state.ready = true;
        state
            .settings
            .calendars
            .push(notetaker_llm::calendar::Subscription {
                id: "calendar-test".into(),
                name: "Команда".into(),
            });
        let monotonic = Instant::now();
        state.calendar_checked = Some(monotonic);
        let before = chrono::Local
            .with_ymd_and_hms(2026, 11, 15, 23, 59, 0)
            .earliest()
            .unwrap()
            .with_timezone(&chrono::Utc);
        let wake = next_state_wake_at(&state, monotonic, before).unwrap();
        assert_eq!(wake, Duration::from_secs(60));
        let after = before + chrono::Duration::minutes(2);
        assert!(
            next_state_wake_at(&state, monotonic + Duration::from_secs(120), after).unwrap()
                > Duration::from_secs(60)
        );
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    fn text(shape: &egui::epaint::Shape) -> String {
        match shape {
            egui::epaint::Shape::Text(t) => t.galley.text().into(),
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().map(text).collect::<Vec<_>>().join("\n")
            }
            _ => String::new(),
        }
    }
    #[test]
    fn final_pass_wait_dialog_shows_progress_and_delays_force_action() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.0);
        let (state, _commands, _events) = crate::test_ui::state();
        let mut app = NotetakerApp::with_state(state);
        app.state.recording.phase = Phase::Finalizing;
        app.state.recording.progress = Some((2, 5));
        let start = Instant::now();
        app.state.exit_at(start);
        assert!(!app.state.can_exit());
        let render = |app: &mut NotetakerApp, now| {
            let out = crate::test_ui::frame(&ctx, vec![], &mut |_| app.exit_dialog(&ctx, now));
            let rendered = out
                .shapes
                .iter()
                .map(|s| text(&s.shape))
                .collect::<Vec<_>>()
                .join("\n");
            out.drop_without_applying_deltas();
            rendered
        };
        render(&mut app, start);
        let early = render(&mut app, start + Duration::from_secs(9));
        assert!(early.contains("Завершаю запись"), "{early}");
        assert!(early.contains("Распознаю запись: 2 из 5"), "{early}");
        assert!(!early.contains("Выйти сейчас"));
        let later = render(&mut app, start + Duration::from_secs(10));
        assert!(later.contains("Выйти сейчас"));
        assert!(later.contains("Несохранённые правки"));
        crate::test_ui::click(
            &ctx,
            &mut |_| app.exit_dialog(&ctx, start + Duration::from_secs(10)),
            "Выйти сейчас",
        );
        assert!(app.state.can_exit());
    }
}

#[cfg(test)]
mod recording_next_tests {
    use super::*;
    fn app(phase: Phase) -> NotetakerApp {
        let (mut state, _commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.recording.phase = phase;
        let now = chrono::Utc::now();
        state.calendar_events = vec![notetaker_core::calendar::CalendarEvent {
            link: notetaker_core::calendar::EventLink {
                uid: "next-ui".into(),
                title: "Синтетическая следующая встреча".into(),
                attendees: Vec::new(),
                call_url: Some("https://space.ktalk.ru/neutral".into()),
            },
            start: now + chrono::Duration::seconds(30),
            end: now + chrono::Duration::minutes(30),
            all_day: false,
            organizer: None,
        }];
        state.update_calendar_reminder(now);
        NotetakerApp::with_state(state)
    }
    #[test]
    fn busy_recording_suppresses_both_reminders_and_shows_next_in_status() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        for phase in [Phase::Starting, Phase::Recording, Phase::Finalizing] {
            let mut app = app(phase);
            for _ in 0..2 {
                let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
                    let bounds = ui.max_rect();
                    app.calendar_reminder_overlay(ui.ctx(), bounds);
                    app.call_notification(ui.ctx());
                });
                assert!(
                    !output
                        .viewport_output
                        .contains_key(&egui::ViewportId::from_hash_of("call-notification"))
                );
                assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::epaint::Shape::Text(t) if t.galley.text().contains("Синтетическая"))));
                output.drop_without_applying_deltas();
            }
            let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| app.bottom_bar(ui));
            crate::test_ui::text_rect(&output.shapes, "Далее: Синтетическая следующая встреча");
            crate::test_ui::text_rect(&output.shapes, "· через 1 мин");
            assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::epaint::Shape::Text(t) if t.galley.text().contains("Завершить и записать"))));
            output.drop_without_applying_deltas();
            assert!(indicator_visible(phase, true, false, false, true));
        }
    }
    #[test]
    fn floating_indicator_adds_one_row_and_dismiss_does_not_open_or_stop() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut app = app(Phase::Recording);
        let next = app.state.next_meeting(chrono::Utc::now()).unwrap();
        let mut result = (false, false, false, egui::Vec2::ZERO);
        let mut render = |ui: &mut egui::Ui| {
            ui.set_width(320.0);
            result = recording_pill(
                ui,
                Duration::from_secs(120),
                0.3,
                Phase::Recording,
                Some(&next),
            );
        };
        for _ in 0..3 {
            crate::test_ui::frame(&ctx, vec![], &mut render).drop_without_applying_deltas();
        }
        drop(render);
        // A 54 point pill plus one standard 32 point row and its 8 point gap.
        assert!(result.3.x <= 320.1 && result.3.y <= 94.1, "{:?}", result.3);
        let pos = egui::pos2(result.3.x - 25.0, result.3.y - 25.0);
        for pressed in [true, false] {
            crate::test_ui::frame(
                &ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                &mut |ui| {
                    ui.set_width(320.0);
                    result = recording_pill(
                        ui,
                        Duration::from_secs(120),
                        0.3,
                        Phase::Recording,
                        Some(&next),
                    );
                },
            )
            .drop_without_applying_deltas();
        }
        assert_eq!((result.0, result.1, result.2), (false, false, true));
        app.state.dismiss_calendar_reminder();
        app.state.update_calendar_reminder(chrono::Utc::now());
        assert!(app.state.next_meeting(chrono::Utc::now()).is_none());
    }
}
