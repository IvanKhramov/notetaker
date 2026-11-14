//! People, series and digest screens. Disk and LLM work stays off the UI thread.
use crate::{config::Settings, state::AppState, theme, worker::Command};
use chrono::{Datelike, Timelike};
use eframe::egui;
use notetaker_core::{
    CarryOver, Digest, DigestFilter, DigestPeriod, Library, Meeting, MeetingMeta, Mode,
    PeopleEdits, Person, PersonPage, Series, Task,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
};

#[derive(Clone, Default)]
pub struct Request {
    pub name: String,
    pub email: String,
    pub voice: Option<String>,
}
pub fn request(ui: &egui::Ui, request: Request) {
    ui.ctx()
        .data_mut(|data| data.insert_temp(egui::Id::unique("open-person"), request));
}
#[derive(Clone, PartialEq, Eq)]
enum View {
    People,
    Person(String),
    Series(String),
    Digest,
}
struct Snapshot {
    people: Vec<Person>,
    edits: PeopleEdits,
    series: Vec<Series>,
    meetings: BTreeMap<PathBuf, Meeting>,
    tasks: Vec<Task>,
}
impl Snapshot {
    fn read(settings: &Settings) -> anyhow::Result<Self> {
        let library = Library::open(&settings.library)?;
        Ok(Self {
            people: library.people_in_folder(
                false,
                &settings.user_name,
                &settings.user_aliases,
                settings.folder.as_deref(),
            )?,
            edits: library.read_people_edits()?,
            series: library.series_in_folder(false, settings.folder.as_deref())?,
            meetings: library
                .list_meetings()?
                .into_iter()
                .map(|m| (m.path().to_owned(), m))
                .collect(),
            tasks: library.list_tasks_in_folder(None, None, false, settings.folder.as_deref())?,
        })
    }
}
enum Response {
    Loaded(Box<Snapshot>),
    Person(Box<PersonPage>),
    Brief(notetaker_llm::PeopleBrief),
    Digest(Box<Digest>, PathBuf),
}
pub enum HeaderAction {
    Open(Meeting),
    Insert(uuid::Uuid),
}
pub struct Panel {
    view: Option<View>,
    root: PathBuf,
    folder: Option<String>,
    snapshot: Option<Snapshot>,
    receiver: Option<Receiver<Result<Response, String>>>,
    needs_refresh: bool,
    pending: Option<Request>,
    person: Option<PersonPage>,
    brief: String,
    message: Option<String>,
    query: String,
    rename: String,
    merge_into: String,
    pending_merge: Option<(Person, Person)>,
    show_hidden: bool,
    recent: usize,
    period: usize,
    start: String,
    end: String,
    filter_person: String,
    filter_series: String,
    digest: Option<(Digest, PathBuf)>,
    reminded: Option<chrono::NaiveDate>,
    selected: Option<PathBuf>,
}
impl Default for Panel {
    fn default() -> Self {
        let week = DigestPeriod::week(chrono::Local::now(), false);
        Self {
            view: None,
            root: PathBuf::new(),
            folder: None,
            snapshot: None,
            receiver: None,
            needs_refresh: false,
            pending: None,
            person: None,
            brief: String::new(),
            message: None,
            query: String::new(),
            rename: String::new(),
            merge_into: String::new(),
            pending_merge: None,
            show_hidden: false,
            recent: 10,
            period: 0,
            start: week.start.to_string(),
            end: week.end.to_string(),
            filter_person: String::new(),
            filter_series: String::new(),
            digest: None,
            reminded: None,
            selected: None,
        }
    }
}
impl Panel {
    pub fn active(&self) -> bool {
        self.view.is_some()
    }
    pub fn close(&mut self) {
        self.view = None;
    }
    fn launch(
        &mut self,
        ctx: &egui::Context,
        job: impl FnOnce() -> anyhow::Result<Response> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.message = None;
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = job().map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    fn refresh(&mut self, ctx: &egui::Context, settings: &Settings) {
        if self.receiver.is_some() {
            self.needs_refresh = true;
            return;
        }
        self.needs_refresh = false;
        let mut settings = settings.clone();
        settings.folder = self.folder.clone();
        self.launch(ctx, move || {
            Ok(Response::Loaded(Box::new(Snapshot::read(&settings)?)))
        });
    }
    pub fn poll(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if self.root != state.settings.library || self.folder != state.folder_filter {
            *self = Self {
                root: state.settings.library.clone(),
                folder: state.folder_filter.clone(),
                ..Self::default()
            };
        }
        if let Some(response) = self.receiver.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.receiver = None;
            match response {
                Ok(Response::Loaded(snapshot)) => {
                    self.snapshot = Some(*snapshot);
                    self.person = None;
                }
                Ok(Response::Person(page)) => {
                    self.rename.clone_from(&page.person.name);
                    self.person = Some(*page);
                }
                Ok(Response::Brief(brief)) => {
                    self.brief = brief.text;
                    self.message = brief.warning;
                }
                Ok(Response::Digest(digest, path)) => self.digest = Some((*digest, path)),
                Err(error) => {
                    self.message = Some(error);
                    if matches!(self.view, Some(View::Person(_))) && self.person.is_none() {
                        self.view = Some(View::People);
                    }
                }
            }
        }
        if let Some(request) =
            ctx.data_mut(|data| data.remove_temp::<Request>(egui::Id::unique("open-person")))
        {
            self.pending = Some(request);
            self.view = Some(View::People);
            state.tasks_open = false;
            self.refresh(ctx, &state.settings);
        }
        if self.needs_refresh && self.receiver.is_none() {
            self.refresh(ctx, &state.settings);
        }
        if self.receiver.is_none() {
            if let Some(request) = self.pending.take() {
                let matches: Vec<_> = self
                    .snapshot
                    .as_ref()
                    .map(|s| {
                        s.people
                            .iter()
                            .filter(|p| {
                                if !request.email.is_empty() {
                                    p.emails.contains(&request.email.trim().to_lowercase())
                                } else if let Some(voice) = &request.voice {
                                    p.voice_ids.contains(voice)
                                } else {
                                    p.matches(&request.name)
                                }
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if matches.len() == 1 {
                    self.open_person(ctx, matches[0].clone());
                } else {
                    self.query = request.name;
                    self.message =
                        Some("Выберите человека; имя не найдено или неоднозначно".into());
                }
            } else if let Some(View::Person(id)) = &self.view
                && self.person.as_ref().is_none_or(|p| &p.person.id != id)
                && let Some(person) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.people.iter().find(|p| &p.id == id))
                    .cloned()
            {
                self.open_person(ctx, person);
            }
        }
        let now = chrono::Local::now();
        if state.settings.digest_reminder
            && now.weekday() == chrono::Weekday::Fri
            && now.hour() >= 17
            && self.reminded != Some(now.date_naive())
        {
            self.reminded = Some(now.date_naive());
            state.notices.push((
                crate::theme::ToastLevel::Info,
                "Пора подготовить недельный дайджест. Откройте «Дайджест» в боковой панели.".into(),
            ));
        }
        if self.selected != state.selected {
            if self.selected.is_some() {
                self.close();
            }
            self.selected.clone_from(&state.selected);
            self.refresh(ctx, &state.settings);
        }
        if self.receiver.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    pub fn sidebar(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        for (label, view) in [
            ("Люди", View::People),
            ("Серии", View::Series(String::new())),
            ("Дайджест", View::Digest),
        ] {
            let selected = matches!(
                (&view, &self.view),
                (View::People, Some(View::People | View::Person(_)))
                    | (View::Series(_), Some(View::Series(_)))
                    | (View::Digest, Some(View::Digest))
            );
            if theme::nav_row(ui, label, selected).clicked() {
                self.view = Some(view);
                state.tasks_open = false;
                self.refresh(ui.ctx(), &state.settings);
                ui.close();
            }
        }
    }
    fn open_person(&mut self, ctx: &egui::Context, person: Person) {
        self.view = Some(View::Person(person.id.clone()));
        self.person = None;
        self.brief.clear();
        self.merge_into.clear();
        let root = self.root.clone();
        let folder = self.folder.clone();
        self.launch(ctx, move || {
            Ok(Response::Person(Box::new(
                Library::open(root)?.person_page_in_folder(person, false, folder.as_deref())?,
            )))
        });
    }
    fn navigate(&mut self, state: &mut AppState, path: &std::path::Path) {
        if let Some(meeting) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.meetings.get(path))
            .cloned()
        {
            self.close();
            state.select(meeting);
        }
    }
    pub fn header(
        &mut self,
        ui: &mut egui::Ui,
        meta: &MeetingMeta,
        editable: bool,
    ) -> Option<HeaderAction> {
        let snapshot = self.snapshot.as_ref()?;
        let series = snapshot
            .series
            .iter()
            .find(|s| s.meetings.iter().any(|m| m.meta.id == meta.id))?;
        if series.meetings.len() < 2 {
            return None;
        }
        let mut action = None;
        let index = series.meetings.iter().position(|m| m.meta.id == meta.id)?;
        ui.horizontal_wrapped(|ui| {
            if ui
                .link(format!(
                    "Серия: {} · {} встреч",
                    series.title,
                    series.meetings.len()
                ))
                .clicked()
            {
                self.view = Some(View::Series(series.id.clone()));
            }
            for (label, next) in [
                ("Предыдущая", series.meetings.get(index + 1)),
                (
                    "Следующая",
                    index.checked_sub(1).and_then(|i| series.meetings.get(i)),
                ),
            ] {
                if ui
                    .add_enabled(
                        next.is_some(),
                        crate::theme::styled_button(
                            crate::theme::Palette::of(ui),
                            label,
                            crate::theme::ButtonStyle::Secondary,
                        ),
                    )
                    .clicked()
                    && let Some(meeting) = next.and_then(|m| snapshot.meetings.get(&m.path))
                {
                    action = Some(HeaderAction::Open(meeting.clone()));
                }
            }
        });
        if let Some(previous) = series
            .meetings
            .iter()
            .find(|m| m.meta.started_at < meta.started_at)
        {
            let carry = CarryOver {
                previous: previous.clone(),
                decisions: notetaker_core::decisions(&previous.summary),
                tasks: snapshot
                    .tasks
                    .iter()
                    .filter(|t| !t.completed && t.meeting_id == previous.meta.id)
                    .cloned()
                    .collect(),
            };
            crate::theme::disclosure("С прошлой встречи").show(ui, |ui| {
                ui.label(carry.markdown());
                if meta.mode == Mode::Hybrid && previous.meta.mode == Mode::Local {
                    ui.weak("Источник Локально: при вставке встреча станет Локально");
                }
                if ui
                    .add_enabled(
                        editable,
                        crate::theme::styled_button(
                            crate::theme::Palette::of(ui),
                            "Вставить в заметки",
                            crate::theme::ButtonStyle::Secondary,
                        ),
                    )
                    .clicked()
                {
                    action = Some(HeaderAction::Insert(previous.meta.id));
                }
            });
        }
        action
    }
    pub fn screen(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let Some(view) = self.view.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            if theme::icon_button(ui, theme::icons::CHEVRON_LEFT, "Назад к встречам").clicked()
                || theme::button(ui, "Встречи", theme::ButtonStyle::Ghost).clicked()
            {
                self.close();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled_ui(self.receiver.is_none(), |ui| {
                        theme::icon_button(ui, theme::icons::REFRESH_CW, "Обновить")
                    })
                    .inner
                    .clicked()
                {
                    self.refresh(ui.ctx(), &state.settings);
                }
                if self.receiver.is_some() {
                    ui.spinner();
                }
            });
        });
        ui.add_space(16.0);
        if let Some(message) = &self.message {
            ui.colored_label(ui.visuals().warn_fg_color, message);
        }
        let width = ui.available_width();
        egui::ScrollArea::vertical()
            .id_salt("people-screen")
            .max_width(width)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(width);
                match view {
                    View::People => self.people_screen(ui),
                    View::Person(_) => self.person_screen(ui, state),
                    View::Series(id) => self.series_screen(ui, state, &id),
                    View::Digest => self.digest_screen(ui, state),
                }
            });
        self.merge_confirmation(ui.ctx(), state);
    }
    fn merge_confirmation(&mut self, ctx: &egui::Context, state: &AppState) {
        let Some((from, to)) = self.pending_merge.clone() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        let closed = theme::modal(ctx, "Объединить людей?")
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Объединить «{}» с «{}»? Имена и связи будут объединены.",
                    from.name, to.name
                ));
                theme::footer(ui, |ui| {
                    confirm =
                        theme::button(ui, "Объединить", theme::ButtonStyle::DangerFilled).clicked();
                    cancel = theme::button(ui, "Отмена", theme::ButtonStyle::Ghost).clicked();
                });
            });
        if confirm {
            let mut edits = self.snapshot.as_ref().expect("loaded people").edits.clone();
            edits.merge(&from, &to);
            self.view = Some(View::Person(to.id));
            let mut settings = state.settings.clone();
            settings.folder = self.folder.clone();
            self.launch(ctx, move || {
                let library = Library::open(&settings.library)?;
                library.write_people_edits(&edits)?;
                Ok(Response::Loaded(Box::new(Snapshot::read(&settings)?)))
            });
        }
        if confirm || cancel || closed {
            self.pending_merge = None;
        }
    }
    fn people_screen(&mut self, ui: &mut egui::Ui) {
        theme::page_heading(ui, "Люди");
        ui.scope(|ui| {
            ui.set_max_width(ui.available_width().min(480.0));
            theme::search_input(
                ui,
                &mut self.query,
                "people-search",
                "Поиск по имени или почте…",
                None,
            );
        });
        theme::toggle_chip(ui, &mut self.show_hidden, "Показать скрытых");
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut selected = None;
        if let Some(snapshot) = &self.snapshot {
            let mut shown = 0;
            for person in &snapshot.people {
                if (!person.hidden || self.show_hidden)
                    && (self.query.is_empty()
                        || person
                            .name
                            .to_lowercase()
                            .contains(&self.query.to_lowercase())
                        || person.matches(&self.query))
                {
                    shown += 1;
                    ui.push_id(&person.id, |ui| {
                        let p = theme::Palette::of(ui);
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 48.0),
                            egui::Sense::click(),
                        );
                        theme::row_background(ui, &response, false);
                        let center = rect.left_center() + egui::vec2(22.0, 0.0);
                        ui.painter().circle_filled(center, 14.0, p.selected);
                        ui.painter().text(
                            center,
                            egui::Align2::CENTER_CENTER,
                            person.name.chars().next().unwrap_or('?').to_uppercase(),
                            theme::medium(13.0),
                            p.muted,
                        );
                        let meta = person.emails.iter().cloned().collect::<Vec<_>>().join(", ");
                        let title = format!(
                            "{}{}",
                            person.name,
                            if person.hidden { " (скрыт)" } else { "" }
                        );
                        ui.painter().text(
                            rect.left_center()
                                + egui::vec2(48.0, if meta.is_empty() { 0.0 } else { -8.0 }),
                            egui::Align2::LEFT_CENTER,
                            title,
                            theme::medium(14.0),
                            p.text,
                        );
                        if !meta.is_empty() {
                            ui.painter().text(
                                rect.left_center() + egui::vec2(48.0, 10.0),
                                egui::Align2::LEFT_CENTER,
                                meta,
                                egui::FontId::proportional(12.0),
                                p.muted,
                            );
                        }
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Button,
                                ui.is_enabled(),
                                &person.name,
                            )
                        });
                        if response.clicked() {
                            selected = Some(person.clone());
                        }
                    });
                }
            }
            if snapshot.people.is_empty() {
                ui.weak("Люди появятся из участников календаря, имён спикеров и задач");
            } else if shown == 0 {
                ui.weak("Никого не найдено. Попробуйте другое имя или почту.");
            }
        }
        if let Some(person) = selected {
            self.open_person(ui.ctx(), person);
        }
    }
    fn person_screen(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let Some(page) = &self.person else {
            return;
        };
        theme::page_heading(ui, &page.person.name);
        if !page.person.emails.is_empty() {
            ui.label(
                egui::RichText::new(
                    page.person
                        .emails
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                )
                .size(12.0)
                .color(theme::Palette::of(ui).muted),
            );
        }
        if !page.person.aliases.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "Алиасы: {}",
                    page.person
                        .aliases
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .size(12.0)
                .color(theme::Palette::of(ui).muted),
            );
        }
        let mut changed = None;
        ui.add_enabled_ui(self.receiver.is_none(), |ui| {
            crate::theme::disclosure("Изменить человека").show(ui, |ui| {
                ui.add(crate::theme::text_field(&mut self.rename));
                let mut edits = self
                    .snapshot
                    .as_ref()
                    .map(|s| s.edits.clone())
                    .unwrap_or_default();
                if crate::theme::button(ui, "Переименовать", crate::theme::ButtonStyle::Ghost)
                    .clicked()
                {
                    edits.rename(&page.person, &self.rename);
                    changed = Some(edits.clone());
                }
                if crate::theme::button(
                    ui,
                    if page.person.hidden {
                        "Показать"
                    } else {
                        "Скрыть"
                    },
                    crate::theme::ButtonStyle::Ghost,
                )
                .clicked()
                {
                    edits.set_hidden(&page.person, !page.person.hidden);
                    changed = Some(edits.clone());
                }
                crate::theme::select("merge-person")
                    .selected_text("Объединить с...")
                    .show_ui(ui, |ui| {
                        if let Some(snapshot) = &self.snapshot {
                            for other in &snapshot.people {
                                if other.id != page.person.id {
                                    crate::theme::select_value(
                                        ui,
                                        &mut self.merge_into,
                                        other.id.clone(),
                                        format!(
                                            "{} <{}>",
                                            other.name,
                                            other
                                                .emails
                                                .iter()
                                                .cloned()
                                                .collect::<Vec<_>>()
                                                .join(", ")
                                        ),
                                    );
                                }
                            }
                        }
                    });
                if ui
                    .add_enabled(
                        !self.merge_into.is_empty(),
                        crate::theme::styled_button(
                            crate::theme::Palette::of(ui),
                            "Объединить выбранных людей",
                            crate::theme::ButtonStyle::Secondary,
                        ),
                    )
                    .clicked()
                    && let Some(other) = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.people.iter().find(|p| p.id == self.merge_into))
                {
                    self.pending_merge = Some((page.person.clone(), other.clone()));
                }
            });
        });
        ui.label(format!(
            "Время речи: {} мин · реплик: {} · ходов: {} · длинный монолог: {} с",
            page.talk_ms / 60_000,
            page.segment_count,
            page.turns,
            page.longest_monologue_ms / 1000
        ));
        theme::section_caption(ui, "О чём говорили в последний раз");
        if let Some(last) = page.meetings.first() {
            ui.label(last.excerpt());
        }
        let mut generate = false;
        ui.horizontal(|ui| {
            ui.label("Последних встреч:");
            ui.add(egui::DragValue::new(&mut self.recent).range(1..=100));
            generate = ui
                .add_enabled(
                    self.receiver.is_none(),
                    crate::theme::styled_button(
                        crate::theme::Palette::of(ui),
                        "Сводка по человеку",
                        crate::theme::ButtonStyle::Secondary,
                    ),
                )
                .clicked();
        });
        if !self.brief.is_empty() {
            ui.label(&self.brief);
            if crate::theme::button(ui, "Копировать сводку", crate::theme::ButtonStyle::Ghost)
                .clicked()
            {
                ui.ctx().copy_text(self.brief.clone());
                state
                    .notices
                    .push((theme::ToastLevel::Success, "Сводка скопирована".into()));
            }
        }
        let mut open = None;
        for (heading, tasks) in [
            ("Открытые задачи", &page.owned_tasks),
            ("Поручил или попросил", &page.requested_tasks),
        ] {
            theme::section_caption(ui, heading);
            for task in tasks {
                if ui
                    .link(format!(
                        "{} - {} - {}",
                        task.text, task.owner, task.due_text
                    ))
                    .clicked()
                {
                    open = Some(task.path.clone());
                }
            }
        }
        theme::section_caption(ui, "Встречи");
        for meeting in &page.meetings {
            if ui
                .link(format!(
                    "{} · {}",
                    meeting
                        .meta
                        .started_at
                        .with_timezone(&chrono::Local)
                        .format("%d.%m.%Y"),
                    meeting.meta.title
                ))
                .clicked()
            {
                open = Some(meeting.path.clone());
            }
            ui.small(meeting.excerpt());
        }
        let page = page.clone();
        if let Some(path) = open {
            self.navigate(state, &path);
        }
        if generate {
            let recent = self.recent;
            let root = self.root.clone();
            let folder = self.folder.clone();
            self.launch(ui.ctx(), move || {
                Ok(Response::Brief(notetaker_llm::person_brief_in_folder(
                    &Library::open(root)?,
                    page.person,
                    recent,
                    notetaker_llm::Config::load().ok().as_ref(),
                    notetaker_llm::Cancellation::default(),
                    folder.as_deref(),
                )?))
            });
        }
        if let Some(edits) = changed {
            let mut settings = state.settings.clone();
            settings.folder = self.folder.clone();
            self.launch(ui.ctx(), move || {
                let library = Library::open(&settings.library)?;
                library.write_people_edits(&edits)?;
                Ok(Response::Loaded(Box::new(Snapshot::read(&settings)?)))
            });
        }
    }
    fn series_screen(&mut self, ui: &mut egui::Ui, state: &mut AppState, id: &str) {
        theme::page_heading(ui, "Серии");
        let mut open = None;
        if let Some(snapshot) = &self.snapshot {
            if snapshot.series.is_empty() {
                ui.weak("Повторяющиеся встречи появятся здесь после синхронизации календаря.");
            }
            for series in &snapshot.series {
                if !id.is_empty() && series.id != id {
                    continue;
                }
                theme::card(theme::Palette::of(ui), false).show(ui, |ui| {
                    ui.set_width((ui.available_width() - 2.0).max(1.0));
                    if ui
                        .link(
                            egui::RichText::new(format!(
                                "{} · {} встреч",
                                series.title,
                                series.meetings.len()
                            ))
                            .font(theme::semibold(theme::SUBHEADING)),
                        )
                        .clicked()
                    {
                        self.view = Some(View::Series(series.id.clone()));
                    }
                    for meeting in &series.meetings {
                        ui.scope(|ui| {
                            if ui
                                .link(format!(
                                    "{} · {} · открытых задач: {}",
                                    meeting
                                        .meta
                                        .started_at
                                        .with_timezone(&chrono::Local)
                                        .format("%d.%m.%Y %H:%M"),
                                    meeting.meta.title,
                                    meeting.open_tasks
                                ))
                                .clicked()
                            {
                                open = Some(meeting.path.clone());
                            }
                        });
                    }
                });
            }
        }
        if let Some(path) = open {
            self.navigate(state, &path);
        }
    }
    fn digest_screen(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        theme::page_heading(ui, "Дайджест");
        ui.horizontal(|ui| {
            theme::segmented(
                ui,
                &mut self.period,
                &[(0, "Эта неделя"), (1, "Прошлая"), (2, "Произвольно")],
            );
        });
        if self.period == 2 {
            ui.horizontal(|ui| {
                ui.label("С (YYYY-MM-DD):");
                ui.add(crate::theme::text_field(&mut self.start));
                ui.label("До (не включая):");
                ui.add(crate::theme::text_field(&mut self.end));
            });
        }
        crate::theme::select("digest-person")
            .selected_text(
                self.snapshot
                    .as_ref()
                    .and_then(|s| s.people.iter().find(|p| p.id == self.filter_person))
                    .map_or("Все люди", |p| p.name.as_str()),
            )
            .show_ui(ui, |ui| {
                crate::theme::select_value(ui, &mut self.filter_person, String::new(), "Все люди");
                if let Some(snapshot) = &self.snapshot {
                    for person in snapshot.people.iter().filter(|p| !p.hidden) {
                        crate::theme::select_value(
                            ui,
                            &mut self.filter_person,
                            person.id.clone(),
                            &person.name,
                        );
                    }
                }
            });
        crate::theme::select("digest-series")
            .selected_text(
                self.snapshot
                    .as_ref()
                    .and_then(|s| s.series.iter().find(|s| s.id == self.filter_series))
                    .map_or("Все серии", |s| s.title.as_str()),
            )
            .show_ui(ui, |ui| {
                crate::theme::select_value(ui, &mut self.filter_series, String::new(), "Все серии");
                if let Some(snapshot) = &self.snapshot {
                    for series in &snapshot.series {
                        crate::theme::select_value(
                            ui,
                            &mut self.filter_series,
                            series.id.clone(),
                            &series.title,
                        );
                    }
                }
            });
        if self.digest.is_none() && self.receiver.is_none() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Выберите период и участников, чтобы собрать итоги встреч.",
                    )
                    .color(theme::Palette::of(ui).muted),
                )
                .wrap(),
            );
        }
        if ui
            .add_enabled(
                self.receiver.is_none(),
                crate::theme::styled_button(
                    crate::theme::Palette::of(ui),
                    "Создать дайджест",
                    crate::theme::ButtonStyle::Secondary,
                ),
            )
            .clicked()
        {
            let period = if self.period == 2 {
                match (self.start.parse(), self.end.parse()) {
                    (Ok(start), Ok(end)) => Some(DigestPeriod { start, end }),
                    _ => {
                        self.message = Some("Дата должна быть YYYY-MM-DD".into());
                        None
                    }
                }
            } else {
                Some(DigestPeriod::week(chrono::Local::now(), self.period == 1))
            };
            if let Some(period) = period {
                let root = self.root.clone();
                let filter = DigestFilter {
                    person: self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.people.iter().find(|p| p.id == self.filter_person))
                        .cloned(),
                    series_id: (!self.filter_series.is_empty()).then(|| self.filter_series.clone()),
                    folder: self.folder.clone(),
                };
                self.launch(ui.ctx(), move || {
                    let library = Library::open(root)?;
                    let digest = library.digest(period, &filter, &chrono::Local, false)?;
                    let digest = notetaker_llm::summarize_digest(
                        digest,
                        notetaker_llm::Config::load().ok().as_ref(),
                        notetaker_llm::Cancellation::default(),
                    );
                    let path = library.save_digest(&digest)?;
                    Ok(Response::Digest(Box::new(digest), path))
                });
            }
        }
        if let Some((digest, path)) = &self.digest {
            ui.horizontal(|ui| {
                if crate::theme::button(ui, "Копировать", crate::theme::ButtonStyle::Ghost)
                    .clicked()
                {
                    ui.ctx().copy_text(digest.markdown.clone());
                    state
                        .notices
                        .push((theme::ToastLevel::Success, "Дайджест скопирован".into()));
                }
                if crate::theme::button(ui, "Показать в папке", crate::theme::ButtonStyle::Ghost)
                    .clicked()
                {
                    state.send(Command::Reveal(path.clone()));
                }
            });
            ui.label(&digest.markdown);
        }
    }
}
