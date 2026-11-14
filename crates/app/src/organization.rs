//! Virtual folders and shared-library controls, with file work off the UI thread.
use crate::{state::AppState, theme, worker::Command};
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
    names: Vec<String>,
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
        let folders = library.load_folders()?;
        let names = folders
            .value
            .keys()
            .chain(metas.values().filter_map(|meta| meta.folder.as_ref()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(Self {
            names,
            folders,
            metas,
            conflicts,
            shared: library.is_shared(),
        })
    }
}
#[derive(Default)]
pub struct Panel {
    revision: u64,
    root: PathBuf,
    snapshot: Option<Snapshot>,
    receiver: Option<Receiver<Result<Snapshot, String>>>,
    checked: Option<Instant>,
    selected: BTreeSet<PathBuf>,
    settings_open: bool,
    folder_name: String,
    edit_name: String,
    create_name: String,
    rename_dialog: Option<(String, String)>,
    delete_dialog: Option<String>,
    edit: FolderSettings,
    settings_revision: notetaker_core::Revision,
    pending_folder_name: Option<String>,
    secret: String,
    owners_json: String,
    destinations: notetaker_llm::trackers::KaitenDestinations,
    destinations_rx: Option<Receiver<Result<notetaker_llm::trackers::KaitenDestinations, String>>>,
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
            state.tag_filter = None;
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
                    if self
                        .snapshot
                        .as_ref()
                        .is_some_and(|s| s.shared != snapshot.shared)
                    {
                        state.semantic_ready = false;
                        state.semantic_progress = None;
                        state.semantic_note = None;
                    }
                    self.apply_snapshot(snapshot);
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
                Err(e) => {
                    self.pending_folder_name = None;
                    self.message = e;
                }
            }
        }
        if state.ready
            && self.receiver.is_none()
            && !self.settings_open
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
        if state.ready
            && self.receiver.is_none()
            && !self.settings_open
            && self.compare.is_none()
            && !state.unsaved()
            && !state.recording.busy()
            && state.enhancing.is_none()
            && ctx.input(|i| i.viewport().focused.unwrap_or(false))
            && let Some(checked) = self.checked
        {
            ctx.request_repaint_after(
                (checked + Duration::from_secs(30)).saturating_duration_since(Instant::now()),
            );
        }
    }
    pub fn visible(&self, meta: &MeetingMeta, state: &AppState) -> bool {
        matches_folder(meta, state.folder_filter.as_deref())
            && state
                .tag_filter
                .as_ref()
                .is_none_or(|tag| meta.tags.contains(tag))
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
        ui.horizontal(|ui| {
            theme::section_caption(ui, "Папки");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::icon_button_with_color(
                    ui,
                    theme::icons::PLUS,
                    "Создать или настроить папку",
                    theme::Palette::of(ui).tertiary,
                    true,
                )
                .clicked()
                {
                    self.settings_open = true;
                    if let Some(s) = &self.snapshot {
                        self.settings_revision = s.folders.revision;
                    }
                }
            });
        });
        if let Some(s) = &self.snapshot {
            for name in std::iter::once("").chain(s.names.iter().map(String::as_str)) {
                let label = if name.is_empty() {
                    "Без папки"
                } else {
                    name
                };
                let response = theme::nav_row(
                    ui,
                    &format!("▫  {label}"),
                    state.folder_filter.as_deref() == Some(name),
                )
                .on_hover_text(label);
                if response.clicked() {
                    state.folder_filter = Some(name.to_owned());
                }
                if let Some(paths) = response.dnd_release_payload::<Vec<PathBuf>>() {
                    destination = Some(((*paths).clone(), name.to_owned()));
                }
                response.context_menu(|ui| {
                    if !name.is_empty()
                        && crate::theme::button(
                            ui,
                            "Настройки папки",
                            crate::theme::ButtonStyle::Ghost,
                        )
                        .clicked()
                    {
                        edit_folder = Some(name.to_owned());
                        ui.close();
                    }
                    if !self.selected.is_empty()
                        && crate::theme::button(
                            ui,
                            "Переместить выбранные сюда",
                            crate::theme::ButtonStyle::Ghost,
                        )
                        .clicked()
                    {
                        destination =
                            Some((self.selected.iter().cloned().collect(), name.to_owned()));
                        ui.close();
                    }
                });
            }
        }
        if !self.selected.is_empty() {
            ui.small(format!(
                "Выбрано: {}. Меню папки - переместить",
                self.selected.len()
            ));
            if crate::theme::button(ui, "Снять выбор", crate::theme::ButtonStyle::Ghost).clicked()
            {
                self.selected.clear();
            }
        }
        if self.snapshot.as_ref().is_some_and(|s| !s.shared) && likely_synced(&self.root) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Библиотека похожа на общую. Включите режим синхронизации в настройках папок.",
            );
        }
        if !self.message.is_empty() {
            ui.colored_label(ui.visuals().warn_fg_color, &self.message);
        }
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
        if state.folder_filter != previous_filter {
            state.refresh_results();
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
        self.load_folder_editor(name);
        self.settings_open = true;
    }
    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.revision = self.revision.wrapping_add(1);
        self.settings_revision = snapshot.folders.revision;
        self.snapshot = Some(snapshot);
        if let Some(name) = self.pending_folder_name.take() {
            self.load_folder_editor(&name);
        }
    }
    fn load_folder_editor(&mut self, name: &str) {
        self.folder_name = name.into();
        self.edit_name = name.into();
        self.edit = self
            .snapshot
            .as_ref()
            .and_then(|s| s.folders.value.get(name))
            .cloned()
            .unwrap_or_default();
        self.settings_revision = self
            .snapshot
            .as_ref()
            .map(|s| s.folders.revision)
            .unwrap_or_default();
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
        self.destinations_rx = None;
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn meeting(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let Some(path) = state.selected.clone() else {
            return;
        };
        let Some(doc) = state.documents.get(&path) else {
            return;
        };
        let m = doc.data.meeting.clone();
        let meta = &doc.data.meta;
        let enabled = !state.unsaved()
            && !state.recording.busy()
            && state.enhancing.is_none()
            && self.receiver.is_none();
        if self.snapshot.as_ref().is_some_and(|s| s.shared) && meta.mode == Mode::Local {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Локально: файлы этой встречи синхронизируются дисковым сервисом.",
            );
        }
        ui.add_enabled_ui(enabled, |ui| {
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = 26.0;
                ui.spacing_mut().button_padding = egui::vec2(9.0, 4.0);
                ui.spacing_mut().icon_width = 10.0;
                ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
                ui.visuals_mut().widgets.inactive.corner_radius = 12.into();
                crate::theme::select("meeting_folder")
                    .selected_text(
                        egui::RichText::new(meta.folder.as_deref().unwrap_or("Без папки"))
                            .size(theme::SMALL)
                            .color(crate::theme::Palette::of(ui).muted),
                    )
                    .width(0.0)
                    .height(220.0)
                    .icon(|ui, rect, _visuals, _open| {
                        let c = rect.center();
                        ui.painter().line_segment(
                            [c + egui::vec2(-3.0, -1.5), c + egui::vec2(0.0, 1.5)],
                            egui::Stroke::new(1.0, crate::theme::Palette::of(ui).muted),
                        );
                        ui.painter().line_segment(
                            [c + egui::vec2(0.0, 1.5), c + egui::vec2(3.0, -1.5)],
                            egui::Stroke::new(1.0, crate::theme::Palette::of(ui).muted),
                        );
                    })
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
            });
            if let Some(copies) = self.snapshot.as_ref().and_then(|s| s.conflicts.get(&path)) {
                for copy in copies {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Найдена копия от синхронизации",
                        );
                        if crate::theme::button(
                            ui,
                            &format!("Сравнить: {}", copy.copy),
                            crate::theme::ButtonStyle::Ghost,
                        )
                        .clicked()
                        {
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
                if !secret.is_empty() {
                    notetaker_llm::trackers::save_secret(
                        &config,
                        &secret,
                        &notetaker_llm::OsCredentialStore,
                    )?;
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
    pub fn shared_step(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let mut shared = self.snapshot.as_ref().is_some_and(|s| s.shared);
        if likely_synced(&self.root) && !shared {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Путь похож на синхронизируемый. Включите общую библиотеку.",
            );
        }
        if theme::settings_row_desc(
            ui,
            "Общая (синхронизируется)",
            Some(
                "Индекс пересоздаётся локально. Голоса всегда хранятся только на этом компьютере.",
            ),
            |ui| {
                ui.add_enabled(
                    enabled && self.receiver.is_none() && self.snapshot.is_some(),
                    theme::Checkbox::without_text(&mut shared),
                )
            },
        )
        .changed()
        {
            self.launch(ui.ctx(), move |l| {
                l.set_shared(shared)?;
                Ok(())
            });
        }
    }
    fn folder_settings(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, can_share: bool) {
        let names = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.names.clone())
            .unwrap_or_default();
        let mut select = None;
        for name in names {
            ui.push_id(&name, |ui| {
                let p = theme::Palette::of(ui);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 40.0),
                    egui::Sense::hover(),
                );
                theme::row_background(ui, &response, self.folder_name == name);
                theme::paint_icon(
                    ui,
                    rect.left_center() + egui::vec2(12.0, 0.0),
                    theme::icons::FOLDER,
                    16.0,
                    p.muted,
                );
                let title_rect = egui::Rect::from_min_size(
                    rect.min + egui::vec2(28.0, 4.0),
                    egui::vec2((rect.width() - 100.0).max(1.0), 32.0),
                );
                ui.painter().with_clip_rect(title_rect).text(
                    title_rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    &name,
                    theme::medium(14.0),
                    p.text,
                );
                if ui
                    .interact(
                        title_rect,
                        ui.scope_id().with("folder-name"),
                        egui::Sense::click(),
                    )
                    .clicked()
                {
                    select = Some(name.clone());
                }
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_min_size(
                            rect.right_top() - egui::vec2(64.0, -6.0),
                            egui::vec2(64.0, 28.0),
                        ))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.add_enabled_ui(self.receiver.is_none(), |ui| {
                            if theme::icon_button(ui, theme::icons::PENCIL, "Переименовать папку")
                                .clicked()
                            {
                                self.rename_dialog = Some((name.clone(), name.clone()));
                            }
                            if theme::icon_button(ui, theme::icons::TRASH_2, "Удалить папку")
                                .clicked()
                            {
                                self.delete_dialog = Some(name.clone());
                            }
                        });
                    },
                );
            });
        }
        if let Some(name) = select {
            self.edit_folder(&name);
        }
        ui.add_space(16.0);
        theme::form_field(ui, "Новая папка", None, |ui| {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 112.0).max(32.0);
                ui.add(theme::text_field(&mut self.create_name).desired_width(width));
                if ui
                    .add_enabled(
                        self.receiver.is_none()
                            && self.snapshot.is_some()
                            && !self.create_name.trim().is_empty(),
                        theme::styled_button(
                            theme::Palette::of(ui),
                            "Создать",
                            theme::ButtonStyle::Primary,
                        ),
                    )
                    .clicked()
                {
                    let name = self.create_name.trim_matches('/').trim().to_owned();
                    let revision = self.snapshot.as_ref().map(|s| s.folders.revision);
                    if name.is_empty() {
                        self.message = "Укажите имя папки".into();
                    } else if let Some(revision) = revision {
                        self.pending_folder_name = Some(name.clone());
                        self.create_name.clear();
                        self.launch(ctx, move |library| {
                            let mut folders = library.load_folders()?;
                            anyhow::ensure!(
                                folders.revision == revision,
                                "Папки изменились; откройте настройки заново"
                            );
                            anyhow::ensure!(
                                !folders.value.contains_key(&name),
                                "Папка уже существует; выберите её для изменения"
                            );
                            folders.value.insert(name, FolderSettings::default());
                            library.save_folders(&folders.value, folders.revision)?;
                            Ok(())
                        });
                    }
                }
            });
        });
        theme::settings_disclosure(ui, "Дополнительно", |ui| {
            self.shared_step(ui, can_share);
            if !self.folder_name.is_empty() {
                ui.add_space(16.0);
                theme::section_caption(ui, &self.folder_name);
                self.folder_advanced(ctx, ui);
            } else {
                ui.small("Выберите папку для изменения параметров.");
            }
        });
        if !self.message.is_empty() {
            ui.small(&self.message);
        }
    }
    fn folder_confirmations(&mut self, ctx: &egui::Context) {
        if let Some((old, mut name)) = self.rename_dialog.clone() {
            let mut save = false;
            let mut cancel = false;
            let closed = theme::modal(ctx, "Переименовать папку")
                .default_width(420.0)
                .show(ctx, |ui| {
                    theme::form_field(ui, "Название", None, |ui| {
                        ui.add(theme::text_field(&mut name).desired_width(f32::INFINITY))
                    });
                    theme::footer(ui, |ui| {
                        save = ui
                            .add_enabled(
                                !name.trim().is_empty(),
                                theme::styled_button(
                                    theme::Palette::of(ui),
                                    "Сохранить",
                                    theme::ButtonStyle::Primary,
                                ),
                            )
                            .clicked();
                        cancel = theme::button(ui, "Отмена", theme::ButtonStyle::Ghost).clicked();
                    });
                });
            if save {
                let new = name.trim_matches('/').trim().to_owned();
                self.pending_folder_name = Some(new.clone());
                let source = old.clone();
                self.launch(ctx, move |l| {
                    l.rename_folder(&source, Some(&new))?;
                    Ok(())
                });
            }
            self.rename_dialog = if save || cancel || closed {
                None
            } else {
                Some((old, name))
            };
        }
        if let Some(name) = self.delete_dialog.clone() {
            let mut delete = false;
            let mut cancel = false;
            let closed = theme::modal(ctx, "Удалить папку?")
                .default_width(420.0)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Удалить папку «{name}»? Встречи останутся в библиотеке."
                    ));
                    theme::footer(ui, |ui| {
                        delete = theme::button(ui, "Удалить", theme::ButtonStyle::DangerFilled)
                            .clicked();
                        cancel = theme::button(ui, "Отмена", theme::ButtonStyle::Ghost).clicked();
                    });
                });
            if delete {
                self.pending_folder_name = Some(String::new());
                self.launch(ctx, move |l| {
                    l.rename_folder(&name, None)?;
                    Ok(())
                });
            }
            if delete || cancel || closed {
                self.delete_dialog = None;
            }
        }
    }
    fn folder_advanced(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        if self.receiver.is_some() || self.destinations_rx.is_some() {
            ui.spinner();
            return;
        }
        let width = ui.available_width();
        ui.scope(|ui| {
            ui.set_max_width(width);
            let mut template = self.edit.template.clone().unwrap_or_default();
            if theme::form_field(ui, "Шаблон по умолчанию", None, |ui| {
                ui.add(theme::text_field(&mut template).desired_width(f32::INFINITY))
            })
            .changed()
            {
                self.edit.template = (!template.is_empty()).then_some(template);
            }
            theme::form_field(ui, "Режим", None, |ui| {
                crate::theme::select("folder_mode")
                    .width(ui.available_width())
                    .selected_text(match self.edit.mode {
                        None => "Общий режим",
                        Some(Mode::Local) => "Локально",
                        Some(Mode::Hybrid) => "Гибрид",
                    })
                    .show_ui(ui, |ui| {
                        crate::theme::select_value(ui, &mut self.edit.mode, None, "Общий режим");
                        crate::theme::select_value(
                            ui,
                            &mut self.edit.mode,
                            Some(Mode::Local),
                            "Локально",
                        );
                        crate::theme::select_value(
                            ui,
                            &mut self.edit.mode,
                            Some(Mode::Hybrid),
                            "Гибрид",
                        );
                    });
            });
            let mut hint = self.edit.speaker_hint.unwrap_or(0);
            theme::form_field(ui, "Участников", Some("0 - общий"), |ui| {
                if ui
                    .add(egui::DragValue::new(&mut hint).range(0..=32))
                    .changed()
                {
                    self.edit.speaker_hint = (hint > 0).then_some(hint);
                }
            });
            theme::form_field(ui, "Дополнения словаря", None, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.edit.vocabulary)
                        .desired_width(f32::INFINITY)
                        .desired_rows(2),
                )
            });
            ui.label("Правила календаря (в строке условия И, между строками ИЛИ)");
            let mut remove = None;
            for (index, rule) in self.edit.rules.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    ui.horizontal(|ui| {
                        theme::form_field(ui, "Название содержит", None, |ui| {
                            ui.add(
                                theme::text_field(&mut rule.title_contains)
                                    .desired_width(f32::INFINITY),
                            )
                        });
                    });
                    ui.horizontal(|ui| {
                        theme::form_field(ui, "Домен участника", None, |ui| {
                            ui.add(
                                theme::text_field(&mut rule.attendee_domain)
                                    .desired_width(f32::INFINITY),
                            )
                        });
                    });
                    if crate::theme::button(ui, "Удалить правило", crate::theme::ButtonStyle::Ghost)
                        .clicked()
                    {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                self.edit.rules.remove(index);
            }
            if crate::theme::button(ui, "Добавить правило", crate::theme::ButtonStyle::Ghost)
                .clicked()
            {
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
                        crate::theme::styled_button(
                            crate::theme::Palette::of(ui),
                            "Загрузить / обновить структуру Kaiten",
                            crate::theme::ButtonStyle::Secondary,
                        ),
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
                theme::footer(ui, |ui| {
                    if crate::theme::button(
                        ui,
                        "Сохранить папку",
                        crate::theme::ButtonStyle::Primary,
                    )
                    .clicked()
                    {
                        let old = self.folder_name.clone();
                        let name = self.edit_name.trim_matches('/').trim().to_owned();
                        let settings = self.edit.clone();
                        let revision = self.settings_revision;
                        let secret = self.secret.clone();
                        if name.is_empty() {
                            self.message = "Укажите имя папки".into();
                        } else {
                            self.pending_folder_name = Some(name.clone());
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
                                if let Some(c) = &settings.tracker {
                                    notetaker_llm::trackers::validate_config(c)?;
                                }
                                if !secret.is_empty() {
                                    let c = settings
                                        .tracker
                                        .as_ref()
                                        .ok_or_else(|| anyhow::anyhow!("Выберите трекер"))?;
                                    notetaker_llm::trackers::save_secret(
                                        c,
                                        &secret,
                                        &notetaker_llm::OsCredentialStore,
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
                        }
                    }
                });
            });
            ui.small(&self.message);
        });
    }
    pub fn windows(&mut self, ctx: &egui::Context, can_share: bool) {
        let mut settings_open = self.settings_open;
        crate::theme::modal(ctx, "Папки и общая библиотека")
            .open(&mut settings_open)
            .default_width(620.0)
            .show(ctx, |ui| self.folder_settings(ctx, ui, can_share));
        self.settings_open = settings_open;
        self.folder_confirmations(ctx);
        if let Some((m, copy)) = self.compare.clone() {
            let mut open = true;
            crate::theme::modal(ctx, "Сравнение копии синхронизации")
                .open(&mut open)
                .default_width(850.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(450.0)
                        .show(ui, |ui| {
                            let warning_color = ui.visuals().warn_fg_color;
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
                                            label.color(warning_color)
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
                            if crate::theme::button(ui, label, crate::theme::ButtonStyle::Ghost)
                                .clicked()
                            {
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
    if ui
        .add(crate::theme::Checkbox::new(
            &mut enabled,
            "Трекер по умолчанию",
        ))
        .changed()
    {
        *config = enabled.then(TrackerConfig::default);
        if owners_json.is_empty() {
            *owners_json = "{}".into();
        }
    }
    let Some(c) = config else { return };
    theme::form_field(ui, "Трекер", None, |ui| {
        crate::theme::select("tracker_kind")
            .width(ui.available_width())
            .selected_text(format!("{:?}", c.kind))
            .show_ui(ui, |ui| {
                for kind in [
                    TrackerKind::Webhook,
                    TrackerKind::Kaiten,
                    TrackerKind::Jira,
                    TrackerKind::YouTrack,
                ] {
                    crate::theme::select_value(ui, &mut c.kind, kind, format!("{kind:?}"));
                }
            });
    });
    for (label, value) in [
        ("URL (без /api)", &mut c.base_url),
        ("Имя ключа в Credential Manager", &mut c.credential_id),
    ] {
        theme::form_field(ui, label, None, |ui| {
            ui.add(theme::text_field(value).desired_width(f32::INFINITY))
        });
    }
    theme::form_field(
        ui,
        "Новый ключ / секрет HMAC (пусто - сохранить прежний)",
        None,
        |ui| {
            ui.add(
                theme::text_field(secret)
                    .password(true)
                    .desired_width(f32::INFINITY),
            )
        },
    );
    match c.kind {
        TrackerKind::Webhook => {
            theme::form_field(
                ui,
                "Заголовок HMAC (пусто - X-Notetaker-Signature)",
                None,
                |ui| {
                    ui.add(theme::text_field(&mut c.signature_header).desired_width(f32::INFINITY))
                },
            );
        }
        TrackerKind::Kaiten => {
            for (label, value) in [
                ("ID пространства", &mut c.space_id),
                ("ID доски", &mut c.board_id),
                ("ID колонки", &mut c.column_id),
                ("ID дорожки", &mut c.lane_id),
            ] {
                theme::form_field(ui, label, None, |ui| {
                    ui.add(theme::text_field(value).desired_width(f32::INFINITY))
                });
            }
            if theme::form_field(
                ui,
                "Ответственные: email -> числовой ID (JSON)",
                None,
                |ui| ui.add(theme::text_field(owners_json).desired_width(f32::INFINITY)),
            )
            .changed()
                && let Ok(value) = serde_json::from_str(owners_json)
            {
                c.owners = value;
            }
            if serde_json::from_str::<BTreeMap<String, u64>>(owners_json).is_err() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Некорректный JSON ответственных",
                );
            }
        }
        TrackerKind::Jira => {
            theme::form_field(ui, "Email для API token (пусто - PAT)", None, |ui| {
                ui.add(theme::text_field(&mut c.email).desired_width(f32::INFINITY))
            });
            theme::form_field(ui, "Ключ проекта", None, |ui| {
                ui.add(theme::text_field(&mut c.project).desired_width(f32::INFINITY))
            });
            theme::form_field(ui, "Тип задачи", None, |ui| {
                ui.add(theme::text_field(&mut c.issue_type).desired_width(f32::INFINITY))
            });
            ui.add(crate::theme::Checkbox::new(
                &mut c.jira_v3,
                "Jira Cloud REST v3 (иначе v2)",
            ));
        }
        TrackerKind::YouTrack => {
            theme::form_field(
                ui,
                "Внутренний id проекта (например 0-1)",
                None,
                |ui| ui.add(theme::text_field(&mut c.project).desired_width(f32::INFINITY)),
            );
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
    crate::theme::select(label)
        .selected_text(format!(
            "{label}: {}",
            choices
                .iter()
                .find(|c| c.id.to_string() == *selected)
                .map_or(selected.as_str(), |c| c.title.as_str())
        ))
        .show_ui(ui, |ui| {
            for choice in choices {
                crate::theme::select_value(
                    ui,
                    selected,
                    choice.id.to_string(),
                    format!("{} ({})", choice.title, choice.id),
                );
            }
        });
    before != *selected
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_dialog_creates_renames_and_confirms_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let (mut state, _commands, _events) = crate::test_ui::state();
        state.settings.library = dir.path().to_owned();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut panel = Panel {
            root: dir.path().to_owned(),
            settings_open: true,
            create_name: "Created".into(),
            ..Default::default()
        };
        panel.apply_snapshot(Snapshot::read(&library).unwrap());
        crate::test_ui::click(&ctx, &mut |ui| panel.windows(ui.ctx(), false), "Создать");
        wait(&mut panel, &ctx, &mut state);
        assert!(
            library
                .load_folders()
                .unwrap()
                .value
                .contains_key("Created")
        );
        assert_eq!(panel.folder_name, "Created");
        crate::test_ui::click(
            &ctx,
            &mut |ui| panel.windows(ui.ctx(), false),
            theme::icons::PENCIL,
        );
        assert!(panel.rename_dialog.is_some());
        crate::test_ui::replace_text(
            &ctx,
            &mut |ui| panel.windows(ui.ctx(), false),
            "Created",
            "Renamed",
        );
        crate::test_ui::click(&ctx, &mut |ui| panel.windows(ui.ctx(), false), "Сохранить");
        wait(&mut panel, &ctx, &mut state);
        assert_eq!(panel.folder_name, "Renamed");
        let folders = library.load_folders().unwrap().value;
        assert!(!folders.contains_key("Created"));
        assert!(folders.contains_key("Renamed"));
        crate::test_ui::click(
            &ctx,
            &mut |ui| panel.windows(ui.ctx(), false),
            theme::icons::TRASH_2,
        );
        assert!(panel.delete_dialog.is_some());
        assert!(
            library
                .load_folders()
                .unwrap()
                .value
                .contains_key("Renamed")
        );
        crate::test_ui::click(&ctx, &mut |ui| panel.windows(ui.ctx(), false), "Отмена");
        assert!(
            library
                .load_folders()
                .unwrap()
                .value
                .contains_key("Renamed")
        );
        crate::test_ui::click(
            &ctx,
            &mut |ui| panel.windows(ui.ctx(), false),
            theme::icons::TRASH_2,
        );
        crate::test_ui::click(&ctx, &mut |ui| panel.windows(ui.ctx(), false), "Удалить");
        wait(&mut panel, &ctx, &mut state);
        assert!(library.load_folders().unwrap().value.is_empty());
        assert!(panel.folder_name.is_empty());
    }
    fn wait(panel: &mut Panel, ctx: &egui::Context, state: &mut AppState) {
        for _ in 0..300 {
            panel.poll(ctx, state);
            if panel.receiver.is_none() {
                assert!(panel.message.is_empty(), "{}", panel.message);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("folder operation timed out");
    }

    fn snapshot(folders: Folders) -> Snapshot {
        Snapshot {
            names: folders.keys().cloned().collect(),
            folders: Versioned {
                value: folders,
                revision: Default::default(),
            },
            metas: Default::default(),
            conflicts: Default::default(),
            shared: false,
        }
    }

    #[test]
    fn renaming_another_folder_reloads_its_entire_editor() {
        let settings = FolderSettings {
            mode: Some(Mode::Local),
            template: Some("project".into()),
            vocabulary: "folder B".into(),
            speaker_hint: Some(3),
            rules: vec![CalendarRule {
                title_contains: "B".into(),
                ..Default::default()
            }],
            tracker: Some(TrackerConfig {
                base_url: "https://tracker.example".into(),
                owners: BTreeMap::from([("b@example.com".into(), 42)]),
                ..Default::default()
            }),
        };
        let mut panel = Panel::default();
        panel.apply_snapshot(snapshot(BTreeMap::from([
            (
                "A".into(),
                FolderSettings {
                    mode: Some(Mode::Hybrid),
                    ..Default::default()
                },
            ),
            ("B".into(), settings.clone()),
        ])));
        panel.edit_folder("A");
        panel.secret = "unsaved draft".into();
        let (_tx, rx) = mpsc::channel();
        panel.destinations_rx = Some(rx);
        panel.pending_folder_name = Some("C".into());
        panel.apply_snapshot(snapshot(BTreeMap::from([("C".into(), settings.clone())])));
        assert_eq!(panel.folder_name, "C");
        assert_eq!(panel.edit_name, "C");
        assert_eq!(
            serde_json::to_value(&panel.edit).unwrap(),
            serde_json::to_value(&settings).unwrap()
        );
        assert_eq!(
            serde_json::from_str::<BTreeMap<String, u64>>(&panel.owners_json).unwrap(),
            settings.tracker.unwrap().owners
        );
        assert!(panel.secret.is_empty());
        assert!(panel.destinations_rx.is_none());
    }

    #[test]
    fn selecting_creating_and_deleting_folders_reset_editor_fields() {
        let mut panel = Panel::default();
        panel.apply_snapshot(snapshot(BTreeMap::from([
            (
                "A".into(),
                FolderSettings {
                    vocabulary: "A".into(),
                    ..Default::default()
                },
            ),
            (
                "B".into(),
                FolderSettings {
                    vocabulary: "B".into(),
                    ..Default::default()
                },
            ),
        ])));
        panel.edit_folder("A");
        panel.edit_folder("B");
        assert_eq!(panel.edit.vocabulary, "B");
        panel.pending_folder_name = Some("new".into());
        panel.apply_snapshot(snapshot(BTreeMap::from([(
            "new".into(),
            Default::default(),
        )])));
        assert_eq!(panel.folder_name, "new");
        assert!(panel.edit.vocabulary.is_empty());
        panel.edit.vocabulary = "unsaved".into();
        panel.pending_folder_name = Some(String::new());
        panel.apply_snapshot(snapshot(Default::default()));
        assert!(panel.folder_name.is_empty());
        assert!(panel.edit_name.is_empty());
        assert!(panel.edit.vocabulary.is_empty());
        panel.edit.vocabulary = "stale".into();
        panel.snapshot = None;
        panel.edit_folder("missing");
        assert!(panel.edit.vocabulary.is_empty());
    }
}
