//! Virtual folders and shared-library controls, with file work off the UI thread.
use crate::{state::AppState, worker::Command};
use eframe::egui;
use notetaker_core::{
    Library, Meeting, MeetingMeta, Mode, Versioned,
    folders::{CalendarRule, FolderSettings, Folders, TrackerConfig, TrackerKind, matches_folder},
    sharing::{MergeChoice, SyncConflict, likely_synced},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

struct Snapshot {
    folders: Versioned<Folders>,
    metas: BTreeMap<PathBuf, MeetingMeta>,
    conflicts: BTreeMap<PathBuf, Vec<SyncConflict>>,
    shared: bool,
}
impl Snapshot {
    fn read(library: &Library) -> anyhow::Result<Self> {
        let mut metas = BTreeMap::new();
        let mut conflicts = BTreeMap::new();
        for m in library.list_meetings()? {
            metas.insert(m.path().to_owned(), m.read_meta()?);
            let copies = m.sync_conflicts()?;
            if !copies.is_empty() {
                conflicts.insert(m.path().to_owned(), copies);
            }
        }
        Ok(Self {
            folders: library.load_folders()?,
            metas,
            conflicts,
            shared: library.is_shared(),
        })
    }
}
#[derive(Default)]
pub struct Panel {
    root: PathBuf,
    snapshot: Option<Snapshot>,
    receiver: Option<Receiver<Result<Snapshot, String>>>,
    checked: Option<Instant>,
    pub tag: Option<String>,
    selected: BTreeSet<PathBuf>,
    settings_open: bool,
    folder_name: String,
    edit_name: String,
    edit: FolderSettings,
    settings_revision: notetaker_core::Revision,
    secret: String,
    owners_json: String,
    destinations: notetaker_llm::trackers::KaitenDestinations,
    destinations_rx: Option<Receiver<Result<notetaker_llm::trackers::KaitenDestinations, String>>>,
    tags_path: Option<PathBuf>,
    tags_text: String,
    compare: Option<(Meeting, SyncConflict)>,
    message: String,
}
impl Panel {
    fn launch(
        &mut self,
        ctx: &egui::Context,
        action: impl FnOnce(&Library) -> anyhow::Result<()> + Send + 'static,
    ) {
        if self.receiver.is_some() {
            return;
        }
        let root = self.root.clone();
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let l = Library::open(root)?;
                action(&l)?;
                Snapshot::read(&l)
            })()
            .map_err(|e: anyhow::Error| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    pub fn poll(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if self.root != state.settings.library {
            *self = Self {
                root: state.settings.library.clone(),
                ..Default::default()
            };
            state.folder_filter = None;
            state.record_mode_edited = false;
        }
        if let Some(result) = self
            .destinations_rx
            .as_ref()
            .and_then(|r| r.try_recv().ok())
        {
            self.destinations_rx = None;
            match result {
                Ok(destinations) => self.destinations = destinations,
                Err(error) => self.message = error,
            }
        }
        if let Some(result) = self.receiver.as_ref().and_then(|r| r.try_recv().ok()) {
            self.receiver = None;
            self.checked = Some(Instant::now());
            match result {
                Ok(snapshot) => {
                    self.settings_revision = snapshot.folders.revision;
                    self.snapshot = Some(snapshot);
                    if !state.unsaved() && !state.recording.busy() && state.enhancing.is_none() {
                        let current = state
                            .selected
                            .as_ref()
                            .and_then(|p| state.documents.get(p))
                            .map(|d| d.data.meeting.clone());
                        state.documents.retain(|_, d| {
                            d.dirty.any()
                                || d.saving
                                || d.speaker_ui.busy
                                || d.attachments_pending > 0
                        });
                        if let Some(m) = current {
                            state.send(Command::Load(m));
                        }
                        state.refresh();
                        if state.tasks_open {
                            state.refresh_tasks();
                        }
                    }
                }
                Err(e) => self.message = e,
            }
        }
        if state.ready
            && self.receiver.is_none()
            && !self.settings_open
            && self.tags_path.is_none()
            && self.compare.is_none()
            && !state.unsaved()
            && !state.recording.busy()
            && state.enhancing.is_none()
            && ctx.input(|i| i.viewport().focused.unwrap_or(false))
            && self
                .checked
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
        {
            self.launch(ctx, |_| Ok(()));
        }
    }
    pub fn visible(&self, meta: &MeetingMeta, state: &AppState) -> bool {
        matches_folder(meta, state.folder_filter.as_deref())
            && self.tag.as_ref().is_none_or(|tag| meta.tags.contains(tag))
    }
    pub fn task_visible(&self, path: &PathBuf, state: &AppState) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|s| s.metas.get(path))
            .is_none_or(|m| matches_folder(m, state.folder_filter.as_deref()))
    }
    pub fn sidebar(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let previous_filter = state.folder_filter.clone();
        let mut destination = None;
        let mut edit_folder = None;
        egui::CollapsingHeader::new("Папки и теги")
            .default_open(true)
            .show(ui, |ui| {
                if ui
                    .selectable_label(state.folder_filter.is_none(), "Все папки")
                    .clicked()
                {
                    state.folder_filter = None;
                }
                if let Some(s) = &self.snapshot {
                    let mut names = s.folders.value.keys().cloned().collect::<BTreeSet<_>>();
                    for meta in s.metas.values() {
                        if let Some(folder) = &meta.folder {
                            names.insert(folder.clone());
                        }
                    }
                    for name in names.clone() {
                        let parts = name.split('/').collect::<Vec<_>>();
                        for n in 1..parts.len() {
                            names.insert(parts[..n].join("/"));
                        }
                    }
                    for name in std::iter::once(String::new()).chain(names) {
                        let count = s
                            .metas
                            .values()
                            .filter(|m| matches_folder(m, Some(&name)))
                            .count();
                        let label = if name.is_empty() {
                            format!("Без папки ({count})")
                        } else {
                            format!(
                                "{}{} ({count})",
                                "  ".repeat(name.matches('/').count()),
                                name.rsplit('/').next().unwrap_or(&name)
                            )
                        };
                        let response = ui
                            .selectable_label(state.folder_filter.as_deref() == Some(&name), label);
                        if response.clicked() {
                            state.folder_filter = Some(name.clone());
                        }
                        if let Some(paths) = response.dnd_release_payload::<Vec<PathBuf>>() {
                            destination = Some(((*paths).clone(), name.clone()));
                        }
                        response.context_menu(|ui| {
                            if !name.is_empty() && ui.button("Настройки папки").clicked()
                            {
                                edit_folder = Some(name.clone());
                                ui.close();
                            }
                            if !self.selected.is_empty()
                                && ui.button("Переместить выбранные сюда").clicked()
                            {
                                destination =
                                    Some((self.selected.iter().cloned().collect(), name.clone()));
                                ui.close();
                            }
                        });
                    }
                    let tags = s
                        .metas
                        .values()
                        .flat_map(|m| m.tags.iter().cloned())
                        .collect::<BTreeSet<_>>();
                    ui.horizontal_wrapped(|ui| {
                        for tag in tags {
                            if ui
                                .selectable_label(
                                    self.tag.as_ref() == Some(&tag),
                                    format!("#{tag}"),
                                )
                                .clicked()
                            {
                                self.tag = if self.tag.as_ref() == Some(&tag) {
                                    None
                                } else {
                                    Some(tag)
                                };
                            }
                        }
                    });
                }
                if !self.selected.is_empty() {
                    ui.small(format!(
                        "Выбрано: {}. Меню папки - переместить",
                        self.selected.len()
                    ));
                    if ui.small_button("Снять выбор").clicked() {
                        self.selected.clear();
                    }
                }
                if self.snapshot.as_ref().is_some_and(|s| !s.shared) && likely_synced(&self.root) {
                    ui.colored_label(egui::Color32::YELLOW, "Библиотека похожа на общую. Включите режим синхронизации в настройках папок.");
                }
                if ui.small_button("Папки / общая библиотека").clicked() {
                    self.settings_open = true;
                    if let Some(s) = &self.snapshot {
                        self.settings_revision = s.folders.revision;
                    }
                }
                if !self.message.is_empty() {
                    ui.colored_label(egui::Color32::YELLOW, &self.message);
                }
            });
        if state.folder_filter != previous_filter {
            state.record_mode = state
                .folder_filter
                .as_ref()
                .and_then(|name| {
                    self.snapshot
                        .as_ref()
                        .and_then(|s| s.folders.value.get(name))
                })
                .and_then(|s| s.mode)
                .unwrap_or(state.settings.mode);
            state.record_mode_edited = false;
        }
        if let Some(name) = edit_folder {
            self.edit_folder(&name);
        }
        if let Some((paths, folder)) = destination {
            self.move_paths(ui.ctx(), paths, folder);
        }
    }
    pub fn selected(&self, path: &std::path::Path) -> bool {
        self.selected.contains(path)
    }
    pub fn row(&mut self, ui: &egui::Ui, response: &egui::Response, meeting: &Meeting) -> bool {
        if response.drag_started() {
            let paths = if self.selected.contains(meeting.path()) {
                self.selected.iter().cloned().collect()
            } else {
                vec![meeting.path().to_owned()]
            };
            response.dnd_set_drag_payload(paths);
        }
        if response.clicked() && ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
            let path = meeting.path().to_owned();
            if !self.selected.remove(&path) {
                self.selected.insert(path);
            }
            return false;
        }
        response.clicked()
    }
    fn move_paths(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, folder: String) {
        self.launch(ctx, move |l| {
            let meetings = l
                .list_meetings()?
                .into_iter()
                .filter(|m| paths.contains(&m.path().to_owned()))
                .collect::<Vec<_>>();
            l.assign_folder(&meetings, Some(&folder))?;
            Ok(())
        });
        self.selected.clear();
    }
    fn edit_folder(&mut self, name: &str) {
        self.folder_name = name.into();
        self.edit_name = name.into();
        if let Some(s) = &self.snapshot {
            self.edit = s.folders.value.get(name).cloned().unwrap_or_default();
            self.settings_revision = s.folders.revision;
        }
        self.secret.clear();
        self.owners_json = serde_json::to_string(
            &self
                .edit
                .tracker
                .as_ref()
                .map(|t| t.owners.clone())
                .unwrap_or_default(),
        )
        .unwrap_or_default();
        self.destinations = Default::default();
        self.settings_open = true;
    }
    pub fn meeting(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let Some(path) = state.selected.clone() else {
            return;
        };
        let Some(doc) = state.documents.get(&path) else {
            return;
        };
        let m = doc.data.meeting.clone();
        let meta = doc.data.meta.clone();
        let enabled = !state.unsaved()
            && !state.recording.busy()
            && state.enhancing.is_none()
            && self.receiver.is_none();
        if self.snapshot.as_ref().is_some_and(|s| s.shared) && meta.mode == Mode::Local {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Local: файлы этой встречи синхронизируются дисковым сервисом.",
            );
        }
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("meeting_folder")
                    .selected_text(meta.folder.as_deref().unwrap_or("Без папки"))
                    .show_ui(ui, |ui| {
                        let names = std::iter::once(String::new())
                            .chain(
                                self.snapshot
                                    .as_ref()
                                    .into_iter()
                                    .flat_map(|s| s.folders.value.keys().cloned()),
                            )
                            .collect::<Vec<_>>();
                        for name in names {
                            if ui
                                .selectable_label(
                                    meta.folder.as_deref() == Some(&name),
                                    if name.is_empty() {
                                        "Без папки"
                                    } else {
                                        &name
                                    },
                                )
                                .clicked()
                            {
                                self.move_paths(ui.ctx(), vec![path.clone()], name);
                            }
                        }
                    });
                if ui.button("Теги").clicked() {
                    self.tags_path = Some(path.clone());
                    self.tags_text = meta.tags.join(", ");
                }
            });
            if let Some(copies) = self.snapshot.as_ref().and_then(|s| s.conflicts.get(&path)) {
                for copy in copies {
                    ui.horizontal(|ui| {
                        ui.colored_label(egui::Color32::YELLOW, "Найдена копия от синхронизации");
                        if ui.button(format!("Сравнить: {}", copy.copy)).clicked() {
                            self.compare = Some((m.clone(), copy.clone()));
                        }
                    });
                }
            }
        });
    }
    fn load_destinations(&mut self, ctx: &egui::Context) {
        let Some(config) = self.edit.tracker.clone() else {
            return;
        };
        let secret = std::mem::take(&mut self.secret);
        let (tx, rx) = mpsc::channel();
        self.destinations_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                use notetaker_llm::CredentialStore;
                if !secret.is_empty() {
                    anyhow::ensure!(!config.credential_id.is_empty(), "Укажите имя ключа");
                    notetaker_llm::OsCredentialStore
                        .write(&format!("tracker/{}", config.credential_id), &secret)?;
                }
                notetaker_llm::trackers::kaiten_destinations(
                    &config,
                    &notetaker_llm::OsCredentialStore,
                )
            })()
            .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    fn folder_settings(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let mut shared = self.snapshot.as_ref().is_some_and(|s| s.shared);
        if likely_synced(&self.root) && !shared {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Путь похож на синхронизируемый. Включите общую библиотеку.",
            );
        }
        if ui
            .add_enabled(
                self.receiver.is_none(),
                egui::Checkbox::new(&mut shared, "Общая (синхронизируется)"),
            )
            .changed()
        {
            self.launch(ctx, move |l| {
                l.set_shared(shared)?;
                Ok(())
            });
        }
        ui.small(
            "Индекс пересоздаётся локально. Голоса всегда хранятся только на этом компьютере.",
        );
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(540.0)
            .show(ui, |ui| {
                let names = self
                    .snapshot
                    .as_ref()
                    .map(|s| s.folders.value.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                egui::ComboBox::from_id_salt("edit_folder")
                    .selected_text(if self.folder_name.is_empty() {
                        "Новая папка"
                    } else {
                        &self.folder_name
                    })
                    .show_ui(ui, |ui| {
                        for name in names {
                            if ui
                                .selectable_label(self.folder_name == name, &name)
                                .clicked()
                            {
                                self.edit_folder(&name);
                            }
                        }
                    });
                if ui.button("Новая папка").clicked() {
                    self.folder_name.clear();
                    self.edit_name.clear();
                    self.edit = Default::default();
                    self.owners_json = "{}".into();
                    self.destinations = Default::default();
                    if let Some(s) = &self.snapshot {
                        self.settings_revision = s.folders.revision;
                    }
                }
                ui.label("Имя (client/project для дерева)");
                ui.text_edit_singleline(&mut self.edit_name);
                let mut template = self.edit.template.clone().unwrap_or_default();
                ui.label("Шаблон по умолчанию");
                if ui.text_edit_singleline(&mut template).changed() {
                    self.edit.template = (!template.is_empty()).then_some(template);
                }
                egui::ComboBox::from_id_salt("folder_mode")
                    .selected_text(match self.edit.mode {
                        None => "Общий режим",
                        Some(Mode::Local) => "Local",
                        Some(Mode::Hybrid) => "Hybrid",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.edit.mode, None, "Общий режим");
                        ui.selectable_value(&mut self.edit.mode, Some(Mode::Local), "Local");
                        ui.selectable_value(&mut self.edit.mode, Some(Mode::Hybrid), "Hybrid");
                    });
                let mut hint = self.edit.speaker_hint.unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.label("Участников (0 - общий)");
                    if ui
                        .add(egui::DragValue::new(&mut hint).range(0..=32))
                        .changed()
                    {
                        self.edit.speaker_hint = (hint > 0).then_some(hint);
                    }
                });
                ui.label("Дополнения словаря");
                ui.text_edit_multiline(&mut self.edit.vocabulary);
                ui.label("Правила календаря (в строке условия И, между строками ИЛИ)");
                let mut remove = None;
                for (index, rule) in self.edit.rules.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Название содержит");
                            ui.text_edit_singleline(&mut rule.title_contains);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Домен участника");
                            ui.text_edit_singleline(&mut rule.attendee_domain);
                        });
                        if ui.small_button("Удалить правило").clicked() {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    self.edit.rules.remove(index);
                }
                if ui.button("Добавить правило").clicked() {
                    self.edit.rules.push(CalendarRule::default());
                }
                tracker_settings(
                    ui,
                    &mut self.edit.tracker,
                    &mut self.secret,
                    &mut self.owners_json,
                );
                if self
                    .edit
                    .tracker
                    .as_ref()
                    .is_some_and(|c| c.kind == TrackerKind::Kaiten)
                {
                    if ui
                        .add_enabled(
                            self.destinations_rx.is_none(),
                            egui::Button::new("Загрузить / обновить структуру Kaiten"),
                        )
                        .clicked()
                    {
                        self.load_destinations(ctx);
                    }
                    let c = self.edit.tracker.as_mut().expect("enabled tracker");
                    if choose(
                        ui,
                        "Пространство",
                        &mut c.space_id,
                        &self.destinations.spaces,
                    ) {
                        c.board_id.clear();
                        c.column_id.clear();
                        c.lane_id.clear();
                        self.destinations.boards.clear();
                        self.destinations.columns.clear();
                        self.destinations.lanes.clear();
                    }
                    if choose(ui, "Доска", &mut c.board_id, &self.destinations.boards) {
                        c.column_id.clear();
                        c.lane_id.clear();
                        self.destinations.columns.clear();
                        self.destinations.lanes.clear();
                    }
                    choose(ui, "Колонка", &mut c.column_id, &self.destinations.columns);
                    choose(ui, "Дорожка", &mut c.lane_id, &self.destinations.lanes);
                    ui.small("Обновите структуру после выбора пространства или доски.");
                }
                let valid_owners = self
                    .edit
                    .tracker
                    .as_ref()
                    .is_none_or(|c| c.kind != TrackerKind::Kaiten)
                    || serde_json::from_str::<BTreeMap<String, u64>>(&self.owners_json).is_ok();
                ui.add_enabled_ui(self.receiver.is_none() && valid_owners, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Сохранить папку").clicked() {
                            let old = self.folder_name.clone();
                            let name = self.edit_name.trim_matches('/').trim().to_owned();
                            let settings = self.edit.clone();
                            let revision = self.settings_revision;
                            let secret = std::mem::take(&mut self.secret);
                            if name.is_empty() {
                                self.message = "Укажите имя папки".into();
                            } else {
                                self.launch(ctx, move |l| {
                                    let mut folders = l.load_folders()?;
                                    anyhow::ensure!(
                                        folders.revision == revision,
                                        "Папки изменились; откройте настройки заново"
                                    );
                                    anyhow::ensure!(
                                        !old.is_empty() || !folders.value.contains_key(&name),
                                        "Папка уже существует; выберите её для изменения"
                                    );
                                    if !secret.is_empty() {
                                        use notetaker_llm::CredentialStore;
                                        let c = settings
                                            .tracker
                                            .as_ref()
                                            .ok_or_else(|| anyhow::anyhow!("Выберите трекер"))?;
                                        anyhow::ensure!(
                                            !c.credential_id.is_empty(),
                                            "Укажите имя ключа"
                                        );
                                        notetaker_llm::OsCredentialStore.write(
                                            &format!("tracker/{}", c.credential_id),
                                            &secret,
                                        )?;
                                    }
                                    if !old.is_empty() && old != name {
                                        l.rename_folder(&old, Some(&name))?;
                                        folders = l.load_folders()?;
                                    }
                                    folders.value.insert(name, settings);
                                    l.save_folders(&folders.value, folders.revision)?;
                                    Ok(())
                                });
                                self.folder_name.clear();
                                self.edit_name.clear();
                                self.edit = Default::default();
                            }
                        }
                        if !self.folder_name.is_empty()
                            && ui.button("Удалить папку (встречи остаются)").clicked()
                        {
                            let name = self.folder_name.clone();
                            self.launch(ctx, move |l| {
                                l.rename_folder(&name, None)?;
                                Ok(())
                            });
                            self.folder_name.clear();
                            self.edit_name.clear();
                            self.edit = Default::default();
                        }
                    });
                });
                ui.small(&self.message);
            });
    }
    pub fn windows(&mut self, ctx: &egui::Context) {
        let mut settings_open = self.settings_open;
        egui::Window::new("Папки и общая библиотека")
            .open(&mut settings_open)
            .default_width(620.0)
            .show(ctx, |ui| self.folder_settings(ctx, ui));
        self.settings_open = settings_open;
        if let Some(path) = self.tags_path.clone() {
            let mut open = true;
            egui::Window::new("Теги встречи")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Через запятую");
                    ui.text_edit_singleline(&mut self.tags_text);
                    if ui.button("Сохранить").clicked() {
                        let tags = self
                            .tags_text
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>();
                        self.launch(ctx, move |l| {
                            if let Some(m) =
                                l.list_meetings()?.into_iter().find(|m| m.path() == path)
                            {
                                m.update_meta(|meta| meta.tags = tags)?;
                            }
                            Ok(())
                        });
                        self.tags_path = None;
                    }
                });
            if !open {
                self.tags_path = None;
            }
        }
        if let Some((m, copy)) = self.compare.clone() {
            let mut open = true;
            egui::Window::new("Сравнение копии синхронизации")
                .open(&mut open)
                .default_width(850.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(450.0)
                        .show(ui, |ui| {
                            ui.columns(2, |cols| {
                                cols[0].strong("Моя версия");
                                cols[1].strong(&copy.copy);
                                let mine = copy.mine.value.lines().collect::<Vec<_>>();
                                let theirs = copy.theirs.value.lines().collect::<Vec<_>>();
                                for index in 0..mine.len().max(theirs.len()) {
                                    let left = mine.get(index).copied().unwrap_or("");
                                    let right = theirs.get(index).copied().unwrap_or("");
                                    for (column, text) in [(0, left), (1, right)] {
                                        let label =
                                            egui::RichText::new(format!("{}: {text}", index + 1))
                                                .monospace();
                                        cols[column].label(if left == right {
                                            label
                                        } else {
                                            label.color(egui::Color32::YELLOW)
                                        });
                                    }
                                }
                            });
                        });
                    ui.horizontal(|ui| {
                        for (label, choice) in [
                            ("Оставить мою", MergeChoice::Mine),
                            ("Оставить копию", MergeChoice::Theirs),
                            ("Оставить обе (дописать)", MergeChoice::Both),
                        ] {
                            if ui.button(label).clicked() {
                                let m = m.clone();
                                let copy = copy.clone();
                                self.launch(ctx, move |_| {
                                    m.merge_sync_conflict(&copy, choice)?;
                                    Ok(())
                                });
                                self.compare = None;
                            }
                        }
                    });
                });
            if !open {
                self.compare = None;
            }
        }
    }
}
fn tracker_settings(
    ui: &mut egui::Ui,
    config: &mut Option<TrackerConfig>,
    secret: &mut String,
    owners_json: &mut String,
) {
    let mut enabled = config.is_some();
    if ui.checkbox(&mut enabled, "Трекер по умолчанию").changed() {
        *config = enabled.then(TrackerConfig::default);
        if owners_json.is_empty() {
            *owners_json = "{}".into();
        }
    }
    let Some(c) = config else { return };
    egui::ComboBox::from_id_salt("tracker_kind")
        .selected_text(format!("{:?}", c.kind))
        .show_ui(ui, |ui| {
            for kind in [
                TrackerKind::Webhook,
                TrackerKind::Kaiten,
                TrackerKind::Jira,
                TrackerKind::YouTrack,
            ] {
                ui.selectable_value(&mut c.kind, kind, format!("{kind:?}"));
            }
        });
    for (label, value) in [
        ("URL (без /api)", &mut c.base_url),
        ("Имя ключа в Credential Manager", &mut c.credential_id),
    ] {
        ui.label(label);
        ui.text_edit_singleline(value);
    }
    ui.label("Новый ключ / секрет HMAC (пусто - сохранить прежний)");
    ui.add(egui::TextEdit::singleline(secret).password(true));
    match c.kind {
        TrackerKind::Webhook => {
            ui.label("Заголовок HMAC (пусто - X-Notetaker-Signature)");
            ui.text_edit_singleline(&mut c.signature_header);
        }
        TrackerKind::Kaiten => {
            for (label, value) in [
                ("ID пространства", &mut c.space_id),
                ("ID доски", &mut c.board_id),
                ("ID колонки", &mut c.column_id),
                ("ID дорожки", &mut c.lane_id),
            ] {
                ui.label(label);
                ui.text_edit_singleline(value);
            }
            ui.label("Ответственные: email -> числовой ID (JSON)");
            if ui.text_edit_singleline(owners_json).changed()
                && let Ok(value) = serde_json::from_str(owners_json)
            {
                c.owners = value;
            }
            if serde_json::from_str::<BTreeMap<String, u64>>(owners_json).is_err() {
                ui.colored_label(egui::Color32::YELLOW, "Некорректный JSON ответственных");
            }
        }
        TrackerKind::Jira => {
            ui.label("Email для API token (пусто - PAT)");
            ui.text_edit_singleline(&mut c.email);
            ui.label("Ключ проекта");
            ui.text_edit_singleline(&mut c.project);
            ui.label("Тип задачи");
            ui.text_edit_singleline(&mut c.issue_type);
            ui.checkbox(&mut c.jira_v3, "Jira Cloud REST v3 (иначе v2)");
        }
        TrackerKind::YouTrack => {
            ui.label("Внутренний id проекта (например 0-1)");
            ui.text_edit_singleline(&mut c.project);
        }
    }
}

fn choose(
    ui: &mut egui::Ui,
    label: &str,
    selected: &mut String,
    choices: &[notetaker_llm::trackers::KaitenChoice],
) -> bool {
    let before = selected.clone();
    egui::ComboBox::from_id_salt(label)
        .selected_text(format!(
            "{label}: {}",
            choices
                .iter()
                .find(|c| c.id.to_string() == *selected)
                .map_or(selected.as_str(), |c| c.title.as_str())
        ))
        .show_ui(ui, |ui| {
            for choice in choices {
                ui.selectable_value(
                    selected,
                    choice.id.to_string(),
                    format!("{} ({})", choice.title, choice.id),
                );
            }
        });
    before != *selected
}
