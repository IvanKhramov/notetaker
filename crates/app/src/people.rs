//! People, series and digest screens. Disk and LLM work stays off the UI thread.
use crate::{config::Settings, state::AppState, worker::Command};
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
            people: library.people(false, &settings.user_name, &settings.user_aliases)?,
            edits: library.read_people_edits()?,
            series: library.series(false)?,
            meetings: library
                .list_meetings()?
                .into_iter()
                .map(|m| (m.path().to_owned(), m))
                .collect(),
            tasks: library.list_tasks(None, None, false)?,
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
        let settings = settings.clone();
        self.launch(ctx, move || {
            Ok(Response::Loaded(Box::new(Snapshot::read(&settings)?)))
        });
    }
    pub fn poll(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if self.root != state.settings.library {
            *self = Self {
                root: state.settings.library.clone(),
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
            if ui
                .selectable_label(self.view.as_ref() == Some(&view), label)
                .clicked()
            {
                self.view = Some(view);
                state.tasks_open = false;
                self.refresh(ui.ctx(), &state.settings);
            }
        }
    }
    fn open_person(&mut self, ctx: &egui::Context, person: Person) {
        self.view = Some(View::Person(person.id.clone()));
        self.person = None;
        self.brief.clear();
        self.merge_into.clear();
        let root = self.root.clone();
        self.launch(ctx, move || {
            Ok(Response::Person(Box::new(
                Library::open(root)?.person_page(person, false)?,
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
                    .add_enabled(next.is_some(), egui::Button::new(label))
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
            ui.collapsing("С прошлой встречи", |ui| {
                ui.label(carry.markdown());
                if meta.mode == Mode::Hybrid && previous.meta.mode == Mode::Local {
                    ui.weak("Источник Local: при вставке встреча станет Local");
                }
                if ui
                    .add_enabled(editable, egui::Button::new("Вставить в заметки"))
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
            if ui.button("К встрече").clicked() {
                self.close();
            }
            if ui
                .add_enabled(self.receiver.is_none(), egui::Button::new("Обновить"))
                .clicked()
            {
                self.refresh(ui.ctx(), &state.settings);
            }
            if self.receiver.is_some() {
                ui.spinner();
            }
        });
        if let Some(message) = &self.message {
            ui.colored_label(ui.visuals().warn_fg_color, message);
        }
        egui::ScrollArea::vertical()
            .id_salt("people-screen")
            .show(ui, |ui| match view {
                View::People => self.people_screen(ui),
                View::Person(_) => self.person_screen(ui, state),
                View::Series(id) => self.series_screen(ui, state, &id),
                View::Digest => self.digest_screen(ui, state),
            });
    }
    fn people_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Люди");
        ui.text_edit_singleline(&mut self.query);
        ui.checkbox(&mut self.show_hidden, "Показать скрытых");
        let mut selected = None;
        if let Some(snapshot) = &self.snapshot {
            for person in &snapshot.people {
                if (!person.hidden || self.show_hidden)
                    && (self.query.is_empty()
                        || person
                            .name
                            .to_lowercase()
                            .contains(&self.query.to_lowercase())
                        || person.matches(&self.query))
                {
                    if ui
                        .link(format!(
                            "{}{}",
                            person.name,
                            if person.hidden { " (скрыт)" } else { "" }
                        ))
                        .clicked()
                    {
                        selected = Some(person.clone());
                    }
                    ui.small(person.emails.iter().cloned().collect::<Vec<_>>().join(", "));
                }
            }
            if snapshot.people.is_empty() {
                ui.weak("Люди появятся из участников календаря, имён спикеров и задач");
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
        ui.heading(&page.person.name);
        ui.label(
            page.person
                .emails
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
        ui.label(format!(
            "Алиасы: {}",
            page.person
                .aliases
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let mut changed = None;
        ui.add_enabled_ui(self.receiver.is_none(), |ui| {
            ui.collapsing("Изменить человека", |ui| {
                ui.text_edit_singleline(&mut self.rename);
                let mut edits = self
                    .snapshot
                    .as_ref()
                    .map(|s| s.edits.clone())
                    .unwrap_or_default();
                if ui.button("Переименовать").clicked() {
                    edits.rename(&page.person, &self.rename);
                    changed = Some(edits.clone());
                }
                if ui
                    .button(if page.person.hidden {
                        "Показать"
                    } else {
                        "Скрыть"
                    })
                    .clicked()
                {
                    edits.set_hidden(&page.person, !page.person.hidden);
                    changed = Some(edits.clone());
                }
                egui::ComboBox::from_id_salt("merge-person")
                    .selected_text("Объединить с...")
                    .show_ui(ui, |ui| {
                        if let Some(snapshot) = &self.snapshot {
                            for other in &snapshot.people {
                                if other.id != page.person.id {
                                    ui.selectable_value(
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
                        egui::Button::new("Объединить выбранных людей"),
                    )
                    .clicked()
                    && let Some(other) = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.people.iter().find(|p| p.id == self.merge_into))
                {
                    edits.merge(&page.person, other);
                    self.view = Some(View::Person(other.id.clone()));
                    changed = Some(edits);
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
        ui.heading("О чём говорили в последний раз");
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
                    egui::Button::new("Сводка по человеку"),
                )
                .clicked();
        });
        if !self.brief.is_empty() {
            ui.label(&self.brief);
            if ui.button("Копировать сводку").clicked() {
                ui.ctx().copy_text(self.brief.clone());
            }
        }
        let mut open = None;
        for (heading, tasks) in [
            ("Открытые задачи", &page.owned_tasks),
            ("Поручил или попросил", &page.requested_tasks),
        ] {
            ui.heading(heading);
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
        ui.heading("Встречи");
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
            self.launch(ui.ctx(), move || {
                Ok(Response::Brief(notetaker_llm::person_brief(
                    &Library::open(root)?,
                    page.person,
                    recent,
                    notetaker_llm::Config::load().ok().as_ref(),
                    notetaker_llm::Cancellation::default(),
                )?))
            });
        }
        if let Some(edits) = changed {
            let settings = state.settings.clone();
            self.launch(ui.ctx(), move || {
                let library = Library::open(&settings.library)?;
                library.write_people_edits(&edits)?;
                Ok(Response::Loaded(Box::new(Snapshot::read(&settings)?)))
            });
        }
    }
    fn series_screen(&mut self, ui: &mut egui::Ui, state: &mut AppState, id: &str) {
        ui.heading("Серии");
        let mut open = None;
        if let Some(snapshot) = &self.snapshot {
            for series in &snapshot.series {
                if !id.is_empty() && series.id != id {
                    continue;
                }
                if ui
                    .link(format!(
                        "{} · {} встреч",
                        series.title,
                        series.meetings.len()
                    ))
                    .clicked()
                {
                    self.view = Some(View::Series(series.id.clone()));
                }
                for meeting in &series.meetings {
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
                }
                ui.separator();
            }
        }
        if let Some(path) = open {
            self.navigate(state, &path);
        }
    }
    fn digest_screen(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        ui.heading("Дайджест");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.period, 0, "Эта неделя");
            ui.selectable_value(&mut self.period, 1, "Прошлая");
            ui.selectable_value(&mut self.period, 2, "Произвольно");
        });
        if self.period == 2 {
            ui.horizontal(|ui| {
                ui.label("С (YYYY-MM-DD):");
                ui.text_edit_singleline(&mut self.start);
                ui.label("До (не включая):");
                ui.text_edit_singleline(&mut self.end);
            });
        }
        egui::ComboBox::from_id_salt("digest-person")
            .selected_text(
                self.snapshot
                    .as_ref()
                    .and_then(|s| s.people.iter().find(|p| p.id == self.filter_person))
                    .map_or("Все люди", |p| p.name.as_str()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter_person, String::new(), "Все люди");
                if let Some(snapshot) = &self.snapshot {
                    for person in snapshot.people.iter().filter(|p| !p.hidden) {
                        ui.selectable_value(
                            &mut self.filter_person,
                            person.id.clone(),
                            &person.name,
                        );
                    }
                }
            });
        egui::ComboBox::from_id_salt("digest-series")
            .selected_text(
                self.snapshot
                    .as_ref()
                    .and_then(|s| s.series.iter().find(|s| s.id == self.filter_series))
                    .map_or("Все серии", |s| s.title.as_str()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter_series, String::new(), "Все серии");
                if let Some(snapshot) = &self.snapshot {
                    for series in &snapshot.series {
                        ui.selectable_value(
                            &mut self.filter_series,
                            series.id.clone(),
                            &series.title,
                        );
                    }
                }
            });
        if ui
            .add_enabled(
                self.receiver.is_none(),
                egui::Button::new("Создать дайджест"),
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
                if ui.button("Копировать").clicked() {
                    ui.ctx().copy_text(digest.markdown.clone());
                }
                if ui.button("Показать в папке").clicked() {
                    state.send(Command::Reveal(path.clone()));
                }
            });
            ui.label(&digest.markdown);
        }
    }
}
