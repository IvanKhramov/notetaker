use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use notetaker_core::Mode;

use crate::{
    config::{MODELS, ProviderSettings},
    state::{AppState, Phase, Tab, edit_segment, speaker, timestamp},
    worker::Command,
};

pub struct NotetakerApp {
    state: AppState,
    cache: CommonMarkCache,
    notes_preview: bool,
    enhanced_preview: bool,
    show_record: bool,
    show_settings: bool,
    last_error: Option<String>,
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
            cache: CommonMarkCache::default(),
            notes_preview: false,
            enhanced_preview: false,
            show_record: false,
            show_settings: false,
            last_error: None,
            #[cfg(windows)]
            desktop: None,
            #[cfg(windows)]
            desktop_attempted: false,
            #[cfg(windows)]
            applied_hotkey: String::new(),
        }
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("notetaker");
            if self.state.recording.busy() {
                let label = if self.state.recording.phase == Phase::Finalizing {
                    "Чистовой проход…"
                } else {
                    "Стоп"
                };
                if ui
                    .add_enabled(
                        self.state.recording.phase != Phase::Finalizing,
                        egui::Button::new(label).fill(Color32::from_rgb(150, 45, 45)),
                    )
                    .clicked()
                {
                    self.state.recording.request_stop();
                }
                ui.label(timestamp(self.state.recording.elapsed().as_millis() as u64));
            } else if ui
                .add_enabled(
                    self.state.ready && !self.state.settings_pending && !self.state.downloading,
                    egui::Button::new("Начать запись"),
                )
                .clicked()
            {
                self.show_record = true;
                self.state.inventory_pending = true;
                self.state.send(Command::Inventory);
            }
            if ui.button("Настройки").clicked() {
                self.show_settings = true;
            }
            if ui
                .add_enabled(
                    self.state.ready && !self.state.listing,
                    egui::Button::new("Обновить"),
                )
                .clicked()
            {
                self.state.flush();
                self.state.refresh();
            }
            if self.state.unsaved() {
                ui.small("Сохранение…");
            }
            if self.state.exit_requested {
                ui.spinner();
                ui.label("Завершаем задачи перед выходом…");
            }
        });
        let recording = &self.state.recording;
        if recording.busy() {
            ui.horizontal_wrapped(|ui| {
                if recording.phase == Phase::Starting {
                    ui.spinner();
                    ui.label("Загружаем модель и открываем аудио…");
                }
                ui.label(format!("Отставание: {:.1} с", recording.lag));
                if recording.live_disabled {
                    ui.colored_label(
                        Color32::YELLOW,
                        "LiveDisabled: текст появится после чистового прохода",
                    );
                }
                if recording.fallback {
                    ui.label("Захват: весь системный звук (откат loopback)");
                }
            });
            if let Some((completed, total)) = recording.progress {
                ui.add(
                    egui::ProgressBar::new(completed as f32 / total.max(1) as f32)
                        .text(format!("Чистовой проход: {completed}/{total}")),
                );
            }
            for message in &recording.messages {
                ui.colored_label(Color32::YELLOW, message);
            }
        }
    }
    fn meetings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Встречи");
        if ui
            .add(
                egui::TextEdit::singleline(&mut self.state.query)
                    .hint_text("Поиск по всем текстам…")
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            self.state.query_changed = Some(Instant::now());
        }
        if self.state.listing {
            ui.spinner();
        }
        ui.small(&self.state.status);
        ui.separator();
        let mut select = None;
        egui::ScrollArea::vertical().id_salt("meetings").show(ui, |ui| {
            for row in &self.state.rows {
                let selected = self.state.selected.as_deref() == Some(row.meeting.path());
                let title = format!("{}\n{}", row.meta.started_at.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M"), row.meta.title);
                if ui.selectable_label(selected, title).on_hover_text(&row.snippet).clicked() { select = Some(row.meeting.clone()); }
                ui.separator();
            }
            if self.state.rows.is_empty() && !self.state.listing { ui.label("Пока нет встреч. Начните запись или выберите другую библиотеку в настройках."); }
        });
        if let Some(meeting) = select {
            self.state.select(meeting);
        }
    }
    fn meeting(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.state.selected.clone() else {
            ui.heading("Заметки во время звонка");
            ui.label("Начните запись и пишите важное здесь. После звонка создайте конспект по заметкам и транскрипту.");
            return;
        };
        let can_enhance = self.state.enhancing.is_none()
            && !self.state.recording.busy()
            && !self.state.unsaved()
            && !self.state.settings_pending;
        let locked_transcript =
            self.state.recording.busy() && self.state.recording.path.as_ref() == Some(&path);
        let Some(document) = self.state.documents.get_mut(&path) else {
            ui.spinner();
            ui.label("Читаем встречу…");
            return;
        };
        ui.heading(&document.data.meta.title);
        ui.horizontal(|ui| {
            ui.small(
                document
                    .data
                    .meta
                    .started_at
                    .with_timezone(&chrono::Local)
                    .format("%d.%m.%Y %H:%M")
                    .to_string(),
            );
            ui.small(mode_label(document.data.meta.mode));
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.state.tab, Tab::Notes, "Заметки");
            ui.selectable_value(&mut self.state.tab, Tab::Enhanced, "Конспект");
            ui.selectable_value(&mut self.state.tab, Tab::Transcript, "Транскрипт");
        });
        ui.separator();
        let mut enhance = false;
        match self.state.tab {
            Tab::Notes => {
                ui.checkbox(&mut self.notes_preview, "Просмотр Markdown");
                egui::ScrollArea::vertical()
                    .id_salt(("notes", &path))
                    .show(ui, |ui| {
                        if self.notes_preview {
                            CommonMarkViewer::new().show(ui, &mut self.cache, &document.data.notes);
                        } else if ui
                            .add(
                                egui::TextEdit::multiline(&mut document.data.notes)
                                    .id_salt(("notes_edit", &path))
                                    .hint_text("Пишите заметки во время звонка…")
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(28),
                            )
                            .changed()
                        {
                            document.dirty.notes = true;
                            document.changed();
                        }
                    });
            }
            Tab::Enhanced => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(document.generation.is_none(), |ui| {
                        egui::ComboBox::from_id_salt("template")
                            .selected_text(&document.template)
                            .show_ui(ui, |ui| {
                                for name in &self.state.templates {
                                    ui.selectable_value(&mut document.template, name.clone(), name);
                                }
                            });
                    });
                    enhance = ui
                        .add_enabled(can_enhance, egui::Button::new("Сделать конспект"))
                        .clicked();
                    ui.checkbox(&mut self.enhanced_preview, "Просмотр Markdown");
                });
                egui::ScrollArea::vertical().id_salt(("enhanced", &path)).show(ui, |ui| {
                    if let Some(text) = &document.generation {
                        ui.spinner(); ui.label("Создаём конспект…");
                        CommonMarkViewer::new().show(ui, &mut self.cache, text);
                    } else if self.enhanced_preview { CommonMarkViewer::new().show(ui, &mut self.cache, &document.data.enhanced); }
                    else if ui.add(egui::TextEdit::multiline(&mut document.data.enhanced).id_salt(("enhanced_edit", &path)).hint_text("Конспект появится здесь. Выберите шаблон и нажмите «Сделать конспект».").desired_width(f32::INFINITY).desired_rows(28)).changed() {
                        document.dirty.enhanced = true; document.changed();
                    }
                });
            }
            Tab::Transcript => {
                if locked_transcript {
                    ui.small("Текст обновляется вживую. Правка сегментов доступна после чистового прохода.");
                }
                let mut edits = Vec::new();
                egui::ScrollArea::vertical()
                    .id_salt(("transcript", &path))
                    .stick_to_bottom(locked_transcript)
                    .show(ui, |ui| {
                        for segment in &document.data.transcript.segments {
                            ui.push_id(segment.id, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "[{}] {}{}",
                                        timestamp(segment.start_ms),
                                        speaker(segment.channel),
                                        if segment.edited {
                                            " · исправлено"
                                        } else {
                                            ""
                                        }
                                    ))
                                    .strong(),
                                );
                                let mut text = segment.text.clone();
                                if ui
                                    .add_enabled(
                                        !locked_transcript,
                                        egui::TextEdit::multiline(&mut text)
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(1),
                                    )
                                    .changed()
                                {
                                    edits.push((segment.id, text));
                                }
                            });
                            ui.separator();
                        }
                        if document.data.transcript.segments.is_empty() {
                            ui.label("Сегментов пока нет.");
                        }
                    });
                for (id, text) in edits {
                    if edit_segment(&mut document.data.transcript, id, text) {
                        document.dirty.transcript = true;
                        document.changed();
                    }
                }
            }
        }
        if enhance {
            self.state.enhance();
        }
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
    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("Настройки").open(&mut open).default_width(640.0).default_height(600.0).resizable(true).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let settings = &mut self.state.settings_draft;
                ui.label("Библиотека (полный путь)");
                let mut library = settings.library.to_string_lossy().into_owned();
                if ui.text_edit_singleline(&mut library).changed() { settings.library = library.into(); }
                model_selector(ui, "default_model", &mut settings.model);
                mode_selector(ui, &mut settings.mode);
                ui.horizontal(|ui| {
                    let mut forever = settings.retention_days.is_none();
                    if ui.checkbox(&mut forever, "Хранить аудио навсегда").changed() { settings.retention_days = if forever { None } else { Some(7) }; }
                    if let Some(days) = &mut settings.retention_days { ui.add(egui::DragValue::new(days).range(0..=36_500).suffix(" дней")); }
                });
                ui.small("0 — удалить аудио после успешного чистового прохода. Срок применяется к новым записям; просроченное аудио очищается после записи.");
                ui.label("Глобальная клавиша старт/стоп (Windows)"); ui.text_edit_singleline(&mut settings.hotkey);
                ui.small("Например Ctrl+Alt+R. Модификаторы: Ctrl, Alt, Shift, Super; клавиша: A–Z, 0–9, F1–F24.");
                ui.separator(); ui.heading("Модели ASR");
                ui.label("Загрузка выполняется отдельно через notetaker-asr. Прогресс показывает этапы загрузки и установки модели и Silero VAD.");
                ui.horizontal(|ui| {
                    for model in MODELS {
                        if ui.add_enabled(!self.state.downloading && !self.state.recording.busy(), egui::Button::new(format!("Скачать {model}"))).clicked() { self.state.download(model.into()); }
                    }
                    if self.state.downloading { ui.spinner(); }
                });
                for line in &self.state.download_messages { ui.small(line); }
                ui.separator(); ui.heading("LLM-провайдеры");
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
                        });
                        ui.horizontal(|ui| { ui.label("base_url"); ui.text_edit_singleline(&mut provider.base_url); });
                        ui.horizontal(|ui| { ui.label("model"); ui.text_edit_singleline(&mut provider.model); });
                        let mut env = provider.api_key_env.clone().unwrap_or_default();
                        ui.horizontal(|ui| { ui.label("api_key_env"); if ui.text_edit_singleline(&mut env).changed() { provider.api_key_env = (!env.is_empty()).then_some(env.clone()); } });
                        ui.horizontal(|ui| { ui.label("context_tokens"); ui.add(egui::DragValue::new(&mut provider.context_tokens).range(1..=2_000_000)); });
                    }); });
                }
                if let Some(index) = remove { llm.providers.remove(index); }
                if ui.button("Добавить провайдер").clicked() { llm.providers.push(ProviderSettings { id: format!("provider-{}", llm.providers.len() + 1), ..ProviderSettings::default() }); }
                ui.separator();
                let can_save = self.state.ready && !self.state.recording.busy() && self.state.enhancing.is_none() && !self.state.unsaved() && !self.state.settings_pending;
                if ui.add_enabled(can_save, egui::Button::new("Сохранить настройки")).clicked() { self.state.save_settings(); }
                if self.state.recording.busy() || self.state.enhancing.is_some() { ui.small("Сохранение настроек доступно после завершения записи и конспекта."); }
                ui.small("Настройки приложения и LLM сохраняются отдельно. Значения API-ключей читаются из окружения при создании конспекта.");
                #[cfg(not(windows))]
                ui.small("Трей и глобальная клавиша включены только на Windows; GTK/AppIndicator не нужны для сборки Linux.");
            });
        });
        self.show_settings = open;
    }
    fn indicator(&mut self, ctx: &egui::Context) {
        if !self.state.recording.busy() {
            return;
        }
        let mut stop = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("recording-indicator"),
            egui::ViewportBuilder::default()
                .with_title("notetaker · запись")
                .with_inner_size([260.0, 88.0])
                .with_resizable(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_close_button(false)
                .with_minimize_button(false)
                .with_maximize_button(false)
                .with_active(false),
            |ui, _| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::LIGHT_RED, "●");
                    ui.label(timestamp(self.state.recording.elapsed().as_millis() as u64));
                    stop = ui
                        .add_enabled(
                            self.state.recording.phase != Phase::Finalizing,
                            egui::Button::new("Стоп"),
                        )
                        .clicked();
                });
                ui.label(match self.state.recording.phase {
                    Phase::Starting => "Загружаем модель…",
                    Phase::Finalizing => "Чистовой проход…",
                    _ => "Идёт запись",
                });
                if ui.input(|input| input.viewport().close_requested()) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                ui.ctx().request_repaint_after(Duration::from_millis(200));
            },
        );
        if stop {
            self.state.recording.request_stop();
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
            if self.applied_hotkey != self.state.settings.hotkey {
                self.applied_hotkey = self.state.settings.hotkey.clone();
                if let Err(error) = desktop.set_hotkey(&self.applied_hotkey) {
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
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe calls this even when the root window is hidden in the tray.
        self.state.poll();
        #[cfg(windows)]
        self.desktop(ctx);
        if self.state.error != self.last_error {
            if self.state.error.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            }
            self.last_error = self.state.error.clone();
        }
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
        if self.state.recording.busy() {
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
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        if let Some(error) = self.state.error.clone() {
            egui::Panel::bottom("error").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::LIGHT_RED, &error);
                    if ui.button("Настройки / скачать модель").clicked() {
                        self.show_settings = true;
                    }
                    if ui.button("Закрыть сообщение").clicked() {
                        self.state.error = None;
                    }
                });
            });
        }
        egui::Panel::left("library")
            .resizable(true)
            .default_size(270.0)
            .size_range(180.0..=500.0)
            .show(ui, |ui| self.meetings(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_enabled_ui(!self.state.settings_pending, |ui| self.meeting(ui));
        });
        if self.show_record {
            self.recording_dialog(&ctx);
        }
        if self.show_settings {
            self.settings_dialog(&ctx);
        }
        self.indicator(&ctx);
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
        .selected_text(model.as_str())
        .show_ui(ui, |ui| {
            for name in MODELS {
                ui.selectable_value(model, name.into(), name);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_fonts_have_russian_in_both_families() {
        let ctx = egui::Context::default();
        let output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            ctx.fonts_mut(|fonts| {
                for font in [
                    egui::FontId::proportional(16.0),
                    egui::FontId::monospace(16.0),
                ] {
                    assert!(fonts.has_glyphs(
                        &font,
                        "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя"
                    ));
                }
            });
        });
        output.drop_without_applying_deltas();
    }
}
