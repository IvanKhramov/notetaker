//! Preview every outgoing payload and require a separate Local consent checkbox.
use crate::{state::AppState, worker::Command};
use eframe::egui;
use notetaker_core::{Library, Task};
use notetaker_llm::trackers::{self, Preview};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
};

enum Response {
    Preview(Vec<Preview>),
    Sent(Vec<(PathBuf, String)>),
}
#[derive(Default)]
pub struct Panel {
    receiver: Option<Receiver<Result<Response, String>>>,
    previews: Vec<Preview>,
    open: bool,
    local_consent: bool,
    message: String,
}
impl Panel {
    fn launch(
        &mut self,
        ctx: &egui::Context,
        job: impl FnOnce() -> anyhow::Result<Response> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let ctx = ctx.clone();
        self.message.clear();
        std::thread::spawn(move || {
            let _ = tx.send(job().map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
    }
    pub fn prepare(&mut self, ctx: &egui::Context, state: &AppState, tasks: Vec<Task>) {
        if self.receiver.is_some()
            || state.unsaved()
            || state.recording.busy()
            || state.enhancing.is_some()
        {
            return;
        }
        self.open = true;
        self.previews.clear();
        self.local_consent = false;
        let root = state.settings.library.clone();
        self.launch(ctx, move || {
            let library = Library::open(root)?;
            let folders = library.load_folders()?.value;
            let mut previews = Vec::new();
            for task in tasks
                .into_iter()
                .filter(|t| !t.completed && t.issue_url.is_none())
            {
                let meta = task.meeting(&library)?.read_meta()?;
                let config = meta
                    .folder
                    .as_ref()
                    .and_then(|f| folders.get(f))
                    .and_then(|f| f.tracker.clone())
                    .ok_or_else(|| anyhow::anyhow!("Настройте трекер в папке встречи"))?;
                previews.push(trackers::preview(&library, task, config)?);
            }
            anyhow::ensure!(
                !previews.is_empty(),
                "Нет новых открытых задач для отправки"
            );
            Ok(Response::Preview(previews))
        });
    }
    pub fn meeting(&mut self, ui: &mut egui::Ui, state: &AppState) {
        let Some(path) = state.selected.as_ref() else {
            return;
        };
        let tasks = state
            .tasks
            .iter()
            .filter(|t| &t.path == path)
            .cloned()
            .collect::<Vec<_>>();
        // Refresh runs when a meeting opens, so summary actions use source revisions.
        if tasks.is_empty() {
            return;
        }
        egui::CollapsingHeader::new("Задачи в трекер").show(ui, |ui| {
            ui.add_enabled_ui(
                self.receiver.is_none()
                    && !state.unsaved()
                    && !state.recording.busy()
                    && state.enhancing.is_none(),
                |ui| {
                    if ui.button("Отправить все новые задачи встречи").clicked()
                    {
                        self.prepare(ui.ctx(), state, tasks.clone());
                    }
                    for task in &tasks {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(&task.text);
                            if let Some(url) = &task.issue_url {
                                ui.hyperlink_to("В трекере", url);
                            } else if !task.completed && ui.button("Отправить в трекер").clicked()
                            {
                                self.prepare(ui.ctx(), state, vec![task.clone()]);
                            }
                        });
                    }
                },
            );
        });
    }
    pub fn poll(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if let Some(result) = self.receiver.as_ref().and_then(|r| r.try_recv().ok()) {
            self.receiver = None;
            match result {
                Ok(Response::Preview(p)) => self.previews = p,
                Ok(Response::Sent(sent)) => {
                    self.previews.clear();
                    self.message = format!("Отправлено: {}", sent.len());
                    for (path, _) in sent {
                        if let Some(doc) = state.documents.get(&path)
                            && !doc.dirty.any()
                            && !doc.saving
                        {
                            let meeting = doc.data.meeting.clone();
                            state.documents.remove(&path);
                            state.send(Command::Load(meeting));
                        }
                    }
                    state.refresh_tasks();
                    state.refresh();
                }
                Err(e) => {
                    self.message = e;
                    state.refresh_tasks();
                }
            }
        }
        let mut open = self.open;
        egui::Window::new("Отправить в трекер: предварительный просмотр")
            .open(&mut open)
            .default_width(680.0)
            .show(ctx, |ui| {
                if self.receiver.is_some() {
                    ui.spinner();
                }
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for p in &self.previews {
                            ui.strong(&p.task.text);
                            ui.label(&p.endpoint);
                            ui.monospace(serde_json::to_string_pretty(&p.body).unwrap_or_default());
                            ui.separator();
                        }
                    });
                let local = self.previews.iter().any(|p| p.local);
                if local {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Это Local-встреча. Задача и фрагмент конспекта покинут компьютер.",
                    );
                    ui.checkbox(
                        &mut self.local_consent,
                        "Подтверждаю отправку Local-данных в трекер",
                    );
                }
                if ui
                    .add_enabled(
                        self.receiver.is_none()
                            && !self.previews.is_empty()
                            && (!local || self.local_consent)
                            && !state.unsaved()
                            && !state.recording.busy()
                            && state.enhancing.is_none(),
                        egui::Button::new("Отправить"),
                    )
                    .clicked()
                {
                    let previews = self.previews.clone();
                    let consent = self.local_consent;
                    let root = state.settings.library.clone();
                    self.launch(ctx, move || {
                        let l = Library::open(root)?;
                        let mut sent = Vec::new();
                        for p in previews {
                            // Earlier items in this batch only append issue links. Reload
                            // revisions, but reject changes to any originally previewed task.
                            l.reindex_incremental()?;
                            let current = l
                                .list_tasks(None, None, false)?
                                .into_iter()
                                .find(|t| {
                                    t.meeting_id == p.task.meeting_id && t.line == p.task.line
                                })
                                .ok_or_else(|| anyhow::anyhow!("Задача больше не найдена"))?;
                            anyhow::ensure!(
                                current.completed == p.task.completed
                                    && current.text == p.task.text
                                    && current.owner == p.task.owner
                                    && current.due_text == p.task.due_text,
                                "Задача изменилась; обновите просмотр"
                            );
                            if current.issue_url.is_some() {
                                continue;
                            }
                            let mut outgoing = p.clone();
                            outgoing.task = current;
                            let url = trackers::send(
                                &l,
                                &outgoing,
                                consent,
                                &notetaker_llm::OsCredentialStore,
                            )?;
                            sent.push((outgoing.task.path, url));
                        }
                        Ok(Response::Sent(sent))
                    });
                }
                ui.label(&self.message);
            });
        self.open = open;
    }
}
