use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use eframe::egui::{self, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use notetaker_core::Mode;

use crate::{
    config::{MODELS, ProviderSettings},
    detection::CallPrompt,
    state::{AppState, Document, Phase, Tab, edit_segment, speaker, timestamp},
    theme::{self, ButtonStyle, Palette},
    worker::Command,
};

pub struct NotetakerApp {
    state: AppState,
    cache: CommonMarkCache,
    notes_preview: bool,
    enhanced_preview: bool,
    show_record: bool,
    show_settings: bool,
    toasts: VecDeque<(Instant, String)>,
    main_rect: Option<egui::Rect>,
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
            enhanced_preview: true,
            show_record: false,
            show_settings: false,
            toasts: VecDeque::new(),
            main_rect: None,
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
        ui.add_space(12.0);
        ui.add_enabled_ui(
            self.state.ready
                && !self.state.recording.busy()
                && !self.state.settings_pending
                && !self.state.downloading,
            |ui| {
                if theme::button(ui, "+ Новая заметка", ButtonStyle::Primary).clicked()
                {
                    self.show_record = true;
                    self.state.inventory_pending = true;
                    self.state.send(Command::Inventory);
                }
            },
        );
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
            self.state.select(meeting);
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
                let can_stop = matches!(r.phase, Phase::Starting | Phase::Recording);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if can_stop && theme::button(ui, "Стоп", ButtonStyle::Danger).clicked() {
                        self.state.toggle_recording();
                    }
                });
            });
        });
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
        let locked =
            self.state.recording.busy() && self.state.recording.path.as_ref() == Some(&path);
        let live = locked && self.state.recording.phase == Phase::Recording;
        let Some(document) = self.state.documents.get_mut(&path) else {
            ui.spinner();
            return;
        };
        ui.heading(RichText::new(&document.data.meta.title).strong());
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
        ui.add_space(12.0);
        if document.generation.is_some() {
            theme::bar(Palette::of(ui)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Пишу конспект…");
                });
            });
        }
        if live {
            let height = ui.available_height();
            ui.columns(2, |columns| {
                columns[0].set_height(height);
                columns[1].set_height(height);
                columns[0].weak("МОИ ЗАМЕТКИ");
                notes_editor(&mut columns[0], document, &path);
                columns[1].weak("ТРАНСКРИПТ");
                transcript_view(&mut columns[1], document, &path, true);
            });
            return;
        }
        theme::segmented(
            ui,
            &mut self.state.tab,
            &[
                (Tab::Enhanced, "Конспект"),
                (Tab::Notes, "Мои заметки"),
                (Tab::Transcript, "Транскрипт"),
            ],
        );
        let mut enhance = false;
        let mut export = None;
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
                                enhance = theme::button(ui, "Пересоздать", ButtonStyle::Secondary)
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
                                if theme::button(ui, "Сохранить .md", ButtonStyle::Primary)
                                    .on_hover_text("Сохранить копию конспекта в папку встречи")
                                    .clicked()
                                {
                                    export = Some(Command::Export {
                                        folder: path.clone(),
                                        text: document.data.enhanced.clone(),
                                    });
                                }
                            });
                        });
                    });
                });
        }
        match self.state.tab {
            Tab::Notes => {
                ui.checkbox(&mut self.notes_preview, "Просмотр Markdown");
                if self.notes_preview {
                    egui::ScrollArea::vertical()
                        .id_salt(("notes_preview", &path))
                        .show(ui, |ui| {
                            CommonMarkViewer::new().show(ui, &mut self.cache, &document.data.notes);
                        });
                } else {
                    notes_editor(ui, document, &path);
                }
            }
            Tab::Enhanced => {
                ui.checkbox(&mut self.enhanced_preview, "Просмотр Markdown");
                egui::ScrollArea::vertical()
                    .id_salt(("enhanced", &path))
                    .show(ui, |ui| {
                        if let Some(text) = &document.generation {
                            CommonMarkViewer::new().show(ui, &mut self.cache, text);
                        } else if document.data.enhanced.is_empty() && self.enhanced_preview {
                            ui.weak(
                                "Выберите шаблон и нажмите «Пересоздать», чтобы собрать конспект.",
                            );
                        } else if self.enhanced_preview {
                            CommonMarkViewer::new().show(
                                ui,
                                &mut self.cache,
                                &document.data.enhanced,
                            );
                        } else if ui
                            .add(
                                egui::TextEdit::multiline(&mut document.data.enhanced)
                                    .id_salt(("enhanced_edit", &path))
                                    .frame(egui::Frame::NONE)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(24),
                            )
                            .changed()
                        {
                            document.dirty.enhanced = true;
                            document.changed();
                        }
                    });
            }
            Tab::Transcript => transcript_view(ui, document, &path, locked),
        }
        if enhance {
            self.state.enhance();
        }
        if let Some(command) = export {
            self.state.send(command);
        }
    }
    fn notifications(&mut self, ctx: &egui::Context) {
        for text in self
            .state
            .warnings
            .drain(..)
            .chain(self.state.recording.messages.drain(..))
        {
            if self.toasts.len() == 5 {
                self.toasts.pop_front();
            }
            self.toasts.push_back((Instant::now(), text));
        }
        self.toasts
            .retain(|(at, _)| at.elapsed() < Duration::from_secs(6));
        if self.toasts.is_empty() && self.state.error.is_none() {
            return;
        }
        egui::Area::new(egui::Id::unique("notifications"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -18.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                for (_, text) in &self.toasts {
                    theme::bar(Palette::of(ui)).show(ui, |ui| {
                        ui.label(text);
                    });
                }
                if let Some(error) = self.state.error.clone() {
                    theme::bar(Palette::of(ui)).show(ui, |ui| {
                        ui.colored_label(Palette::of(ui).red, error);
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
    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("Настройки").open(&mut open).default_width(640.0).default_height(600.0).resizable(true).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let settings = &mut self.state.settings_draft;
                ui.horizontal(|ui| {
                    ui.label("Тема:");
                    egui::ComboBox::from_id_salt("theme").selected_text(settings.theme.label()).show_ui(ui, |ui| {
                        for theme in [crate::theme::Theme::System, crate::theme::Theme::Light, crate::theme::Theme::Dark] {
                            ui.selectable_value(&mut settings.theme, theme, theme.label());
                        }
                    });
                });
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
                ui.small("0 - удалить аудио после успешного чистового прохода. Срок применяется к новым записям; просроченное аудио очищается после записи.");
                ui.label("Глобальная клавиша старт/стоп (Windows)"); ui.text_edit_singleline(&mut settings.hotkey);
                ui.small("Например Ctrl+Alt+R. Модификаторы: Ctrl, Alt, Shift, Super; клавиша: A–Z, 0–9, F1–F24.");
                ui.separator();
                ui.add_enabled_ui(cfg!(windows), |ui| {
                    ui.checkbox(&mut settings.detect_calls, "Предлагать запись при обнаружении звонка");
                    ui.checkbox(&mut settings.detect_browsers, "Учитывать браузеры")
                        .on_hover_text("Включите для Meet, Толка и Телемоста в браузере. Диктовка и голосовые сообщения тоже могут выглядеть как звонок.");
                });
                ui.horizontal(|ui| {
                    ui.label("Автостоп по тишине:");
                    ui.add(egui::DragValue::new(&mut settings.auto_stop_minutes).range(0..=u32::MAX).suffix(" мин"));
                });
                ui.small("0 - выключено. Завершение звонка само по себе не останавливает запись.");
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
    fn first_run(&mut self, ctx: &egui::Context) {
        if !self.state.offer_model_download {
            return;
        }
        let mut open = true;
        egui::Window::new("Локальное распознавание речи")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Модели ещё не установлены. Скачать GigaAM v3 для русского языка?");
                ui.small("Около 171 МБ вместе с Silero VAD. Аудио обрабатывается локально.");
                if ui
                    .add_enabled(
                        !self.state.downloading && !self.state.recording.busy(),
                        egui::Button::new("Скачать GigaAM v3"),
                    )
                    .clicked()
                {
                    self.state.download("gigaam-v3".into());
                }
                if self.state.downloading {
                    ui.spinner();
                }
                for line in &self.state.download_messages {
                    ui.small(line);
                }
                if ui
                    .button("Позже / выбрать другую модель в настройках")
                    .clicked()
                {
                    self.state.offer_model_download = false;
                    self.show_settings = true;
                }
            });
        self.state.offer_model_download &= open;
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
                            CallPrompt::Start { app, .. } => {
                                (format!("Звонок в {app}"), "Записать встречу?", "Записать")
                            }
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
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("recording-indicator"),
            egui::ViewportBuilder::default()
                .with_title("notetaker · запись")
                .with_inner_size([430.0, 52.0])
                .with_min_inner_size([430.0, 52.0])
                .with_max_inner_size([430.0, 52.0])
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
                        ui.set_width(410.0);
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
                            open = theme::button(ui, "Открыть", ButtonStyle::Secondary).clicked();
                            stop = theme::button(ui, "Стоп", ButtonStyle::Danger).clicked();
                        });
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
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe calls this even when the root window is hidden in the tray.
        self.state.poll();
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
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(24))
            .show(ui, |ui| {
                ui.add_enabled_ui(!self.state.settings_pending, |ui| self.meeting(ui));
            });
        self.notifications(&ctx);
        if self.show_record {
            self.recording_dialog(&ctx);
        }
        if self.show_settings {
            self.settings_dialog(&ctx);
        }
        self.indicator(&ctx);
        self.call_notification(&ctx);
        self.first_run(&ctx);
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
fn notes_editor(ui: &mut egui::Ui, document: &mut Document, path: &std::path::Path) {
    egui::ScrollArea::vertical()
        .id_salt(("notes", path))
        .show(ui, |ui| {
            if ui
                .add(
                    egui::TextEdit::multiline(&mut document.data.notes)
                        .id_salt(("notes_edit", path))
                        .hint_text("Пишите важное здесь…")
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY)
                        .desired_rows(24),
                )
                .changed()
            {
                document.dirty.notes = true;
                document.changed();
            }
        });
}
fn transcript_view(ui: &mut egui::Ui, document: &mut Document, path: &std::path::Path, live: bool) {
    let p = Palette::of(ui);
    let mut edits = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt(("transcript", path))
        .stick_to_bottom(live)
        .show(ui, |ui| {
            for segment in &document.data.transcript.segments {
                let me = segment.channel == notetaker_core::Channel::Mic;
                let width = (ui.available_width() * 0.90).max(1.0);
                ui.push_id(segment.id, |ui| {
                    ui.with_layout(
                        if me {
                            egui::Layout::top_down(egui::Align::RIGHT)
                        } else {
                            egui::Layout::top_down(egui::Align::LEFT)
                        },
                        |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}{}",
                                    speaker(segment.channel),
                                    timestamp(segment.start_ms),
                                    if segment.edited {
                                        " · исправлено"
                                    } else {
                                        ""
                                    }
                                ))
                                .size(12.0)
                                .color(p.muted),
                            );
                            egui::Frame::new()
                                .fill(if me { p.me } else { p.soft })
                                .corner_radius(10)
                                .inner_margin(10)
                                .show(ui, |ui| {
                                    ui.set_max_width(width);
                                    if live {
                                        ui.add(egui::Label::new(&segment.text).wrap());
                                    } else {
                                        let mut text = segment.text.clone();
                                        if ui
                                            .add(
                                                egui::TextEdit::multiline(&mut text)
                                                    .frame(egui::Frame::NONE)
                                                    .desired_width(width)
                                                    .desired_rows(1),
                                            )
                                            .changed()
                                        {
                                            edits.push((segment.id, text));
                                        }
                                    }
                                });
                        },
                    );
                });
                ui.add_space(8.0);
            }
            if document.data.transcript.segments.is_empty() {
                ui.weak("Текст появится здесь");
            }
        });
    for (id, text) in edits {
        if edit_segment(&mut document.data.transcript, id, text) {
            document.dirty.transcript = true;
            document.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
