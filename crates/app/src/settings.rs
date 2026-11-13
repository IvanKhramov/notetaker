//! Sectioned settings UI. Secret drafts are accepted only on focus loss.
use std::collections::HashMap;

use eframe::egui::{self, RichText};

use crate::{
    config::{LlmSettings, ProviderSettings},
    state::AppState,
    theme::{self, ButtonStyle, Palette},
    worker::Command,
};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    #[default]
    General,
    Recording,
    Calendars,
    Ai,
    Search,
    Storage,
}
impl Section {
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Recording,
        Self::Calendars,
        Self::Ai,
        Self::Search,
        Self::Storage,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "Общие",
            Self::Recording => "Запись",
            Self::Calendars => "Календари",
            Self::Ai => "AI",
            Self::Search => "Поиск",
            Self::Storage => "Хранилище и диагностика",
        }
    }
}
#[derive(Default)]
pub struct Panel {
    pub section: Section,
    calendar_inputs: HashMap<String, String>,
    calendar_errors: HashMap<String, String>,
    folder_picker: Option<std::sync::mpsc::Receiver<(bool, Option<std::path::PathBuf>)>>,
    disk_usage: Option<std::sync::mpsc::Receiver<(u64, u64)>>,
    usage: Option<(u64, u64)>,
    usage_checked: Option<std::time::Instant>,
    usage_paths: Option<(std::path::PathBuf, std::path::PathBuf)>,
}
#[derive(Default)]
struct Edits {
    typing: bool,
    blur: bool,
}
impl Edits {
    fn text(&mut self, ui: &mut egui::Ui, label: &str, value: &mut String) {
        let response = theme::setting_row(ui, label, |ui| {
            ui.add(egui::TextEdit::singleline(value).desired_width(240.0))
        });
        self.typing |= response.changed();
        self.blur |= response.lost_focus();
    }
    fn number(&mut self, response: egui::Response) {
        self.typing |= response.changed() && response.has_focus() && !response.dragged();
        self.blur |= response.lost_focus();
    }
    fn multiline(&mut self, ui: &mut egui::Ui, label: &str, value: &mut String) {
        ui.label(label);
        let response = ui.add(
            egui::TextEdit::multiline(value)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        self.typing |= response.changed();
        self.blur |= response.lost_focus();
    }
}
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(Palette::of(ui).muted));
}
fn error(ui: &mut egui::Ui, result: anyhow::Result<()>) {
    if let Err(error) = result {
        ui.colored_label(Palette::of(ui).red, format!("{error:#}"));
    }
}

pub fn validate_calendar_secret(id: &str, secret: &str) -> anyhow::Result<()> {
    // Reuse the authoritative URL validator without touching the OS store.
    struct ValidateOnly;
    impl notetaker_llm::CredentialStore for ValidateOnly {
        fn read(&self, _: &str) -> anyhow::Result<String> {
            anyhow::bail!("Validation only")
        }
        fn write(&self, _: &str, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }
    notetaker_llm::calendar::Subscription {
        id: id.into(),
        name: "Календарь".into(),
    }
    .save_url(secret.trim(), &ValidateOnly)
}

impl Panel {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        state: &mut AppState,
        maintenance: &mut crate::maintenance::Maintenance,
        organization: &mut crate::organization::Panel,
    ) -> bool {
        if let Some(rx) = &self.folder_picker
            && let Ok((models, path)) = rx.try_recv()
        {
            if let Some(path) = path {
                if models {
                    state.settings_draft.models_folder = Some(path);
                } else {
                    state.settings_draft.library = path;
                }
                state.save_preferences();
            }
            self.folder_picker = None;
        }
        let mut quickstart = false;
        let previous_section = self.section;
        let mut close = false;
        // This dialog owns its scroll area so the header and navigation stay visible.
        egui::Window::new("Настройки")
            .title_bar(false)
            .open(open)
            .default_size([900.0, 650.0])
            .max_size(
                (ctx.content_rect().size() - egui::vec2(32.0, 32.0)).max(egui::vec2(280.0, 200.0)),
            )
            .constrain_to(ctx.content_rect().shrink(8.0))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Настройки").font(theme::semibold(theme::SUBHEADING)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = theme::button(ui, "×", ButtonStyle::Ghost)
                            .on_hover_text("Закрыть")
                            .clicked();
                    });
                });
                hint(
                    ui,
                    if !self.calendar_errors.is_empty() {
                        "Не сохранено: проверьте URL календаря"
                    } else if self
                        .calendar_inputs
                        .values()
                        .any(|input| !input.trim().is_empty())
                    {
                        "Сохраню после ввода URL"
                    } else {
                        state.settings_status()
                    },
                );
                if let Some(message) = state.settings_autosave.error.clone() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(Palette::of(ui).red, message);
                        if ui.small_button("Повторить").clicked() {
                            state.settings_edited(false, state.settings_autosave.llm);
                        }
                    });
                }
                ui.separator();
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(
                            if ctx.content_rect().width() < 800.0 {
                                125.0
                            } else {
                                170.0
                            },
                            260.0,
                        ),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            for section in Section::ALL {
                                if theme::nav_row(
                                    ui,
                                    if section == Section::Storage {
                                        "Хранилище"
                                    } else {
                                        section.label()
                                    },
                                    self.section == section,
                                )
                                .clicked()
                                {
                                    self.section = section;
                                }
                            }
                        },
                    );
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width((ui.available_width() - 8.0).min(470.0));
                        // A floating bar in its own lane: quiet like the rest, never over controls.
                        ui.style_mut().spacing.scroll.floating_allocated_width = theme::SCROLL_LANE;
                        ui.heading(self.section.label());
                        ui.add_space(12.0);
                        egui::ScrollArea::vertical()
                            .id_salt(("settings_section", self.section as u8))
                            .max_height(
                                ui.available_height()
                                    .min(ctx.content_rect().height() - 150.0)
                                    .max(80.0),
                            )
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                quickstart = self.body(ui, state, maintenance, organization);
                            });
                    });
                });
            });
        if close {
            *open = false;
        }
        if !*open || previous_section != self.section {
            self.blur(state);
        }
        quickstart
    }
    pub fn blur(&mut self, state: &mut AppState) {
        state.connections.blur();
        for (id, input) in &mut self.calendar_inputs {
            if input.trim().is_empty() {
                continue;
            }
            match validate_calendar_secret(id, input) {
                Ok(()) => {
                    self.calendar_errors.remove(id);
                    state
                        .calendar_secrets
                        .insert(id.clone(), input.trim().to_owned());
                    input.clear();
                    state
                        .settings_autosave
                        .edit(std::time::Instant::now(), false, false);
                }
                Err(error) => {
                    self.calendar_errors.insert(id.clone(), error.to_string());
                }
            }
        }
        if state.settings_autosave.pending {
            state.settings_edited(false, state.settings_autosave.llm);
        }
    }
    pub fn body(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        maintenance: &mut crate::maintenance::Maintenance,
        organization: &mut crate::organization::Panel,
    ) -> bool {
        let before = serde_json::to_value(&state.settings_draft).expect("settings serialize");
        let llm_before = serde_json::to_value(&state.llm_draft).expect("LLM serialize");
        let mut edits = Edits::default();
        let mut quickstart = false;
        match self.section {
            Section::General => {
                let settings = &mut state.settings_draft;
                theme::setting_row(ui, "Тема", |ui| {
                    egui::ComboBox::from_id_salt("theme")
                        .selected_text(settings.theme.label())
                        .show_ui(ui, |ui| {
                            for theme in [
                                theme::Theme::Light,
                                theme::Theme::Dark,
                                theme::Theme::System,
                            ] {
                                ui.selectable_value(&mut settings.theme, theme, theme.label());
                            }
                        });
                });
                theme::setting_row(ui, "Масштаб интерфейса", |ui| {
                    egui::ComboBox::from_id_salt("ui_zoom")
                        .selected_text(format!("{:.0}%", settings.ui_zoom * 100.0))
                        .show_ui(ui, |ui| {
                            for value in [0.9, 1.0, 1.1, 1.25, 1.5] {
                                ui.selectable_value(
                                    &mut settings.ui_zoom,
                                    value,
                                    format!("{:.0}%", value * 100.0),
                                );
                            }
                        });
                });
                hint(
                    ui,
                    "Ctrl+= / Ctrl+- / Ctrl+0: изменить или сбросить масштаб.",
                );
                edits.text(ui, "Ваше имя", &mut settings.user_name);
                edits.text(ui, "Другие варианты имени", &mut settings.user_aliases);
                ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                    edits.text(ui, "Горячая клавиша записи", &mut settings.hotkey);
                    error(ui, crate::config::validate_hotkey(&settings.hotkey));
                });
                theme::setting_row(ui, "Быстрый старт", |ui| {
                    quickstart = theme::button(ui, "Открыть ›", ButtonStyle::Ghost).clicked();
                });
                maintenance.windows_settings(ui);
                theme::toggle(
                    ui,
                    &mut settings.digest_reminder,
                    "Напоминать о дайджесте по пятницам",
                );
                ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                    theme::toggle(ui, &mut settings.check_updates, "Проверять обновления");
                });
            }
            Section::Recording => {
                let settings = &mut state.settings_draft;
                theme::setting_row(ui, "Режим новых встреч", |ui| {
                    egui::ComboBox::from_id_salt("settings_mode")
                        .selected_text(if settings.mode == notetaker_core::Mode::Local {
                            "Local"
                        } else {
                            "Hybrid"
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut settings.mode,
                                notetaker_core::Mode::Local,
                                "Local",
                            );
                            ui.selectable_value(
                                &mut settings.mode,
                                notetaker_core::Mode::Hybrid,
                                "Hybrid",
                            );
                        });
                });
                hint(
                    ui,
                    "Local: локальный AI. Hybrid: текст может отправляться провайдеру. Аудио остаётся на компьютере.",
                );
                ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                    theme::toggle(ui, &mut settings.detect_calls, "Предлагать запись звонков");
                });
                theme::toggle(
                    ui,
                    &mut settings.auto_summary,
                    "Создавать конспект после встречи",
                );
                theme::toggle(ui, &mut settings.voice_library, "Запоминать голоса");
                hint(ui, "Голосовые отпечатки хранятся только локально.");
                theme::setting_row(ui, "Автостоп по тишине", |ui| {
                    edits.number(
                        ui.add(
                            egui::DragValue::new(&mut settings.auto_stop_minutes)
                                .range(0..=u32::MAX)
                                .suffix(" мин"),
                        ),
                    );
                });
                hint(
                    ui,
                    "0 минут: выключен. Завершение звонка не останавливает запись.",
                );
                ui.collapsing(
                    "Распознавание и обнаружение звонков",
                    |ui| {
                        theme::toggle(ui, &mut settings.diarize, "Разделять собеседников");
                        theme::setting_row(ui, "Число собеседников", |ui| {
                            egui::ComboBox::from_id_salt("speakers")
                                .selected_text(
                                    settings.speakers.map_or("Авто".into(), |n| n.to_string()),
                                )
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut settings.speakers, None, "Авто");
                                    for n in 1..=32 {
                                        ui.selectable_value(
                                            &mut settings.speakers,
                                            Some(n),
                                            n.to_string(),
                                        );
                                    }
                                });
                        });
                        theme::setting_row(ui, "Потоков ASR (0: авто)", |ui| {
                            edits.number(
                                ui.add(
                                    egui::DragValue::new(&mut settings.asr_threads).range(0..=8),
                                ),
                            );
                        });
                        theme::setting_row(ui, "Ускорение ASR", |ui| {
                            egui::ComboBox::from_id_salt("acceleration")
                                .selected_text(format!("{:?}", settings.asr_acceleration))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut settings.asr_acceleration,
                                        notetaker_asr_proto::Provider::Auto,
                                        "Авто",
                                    );
                                    ui.selectable_value(
                                        &mut settings.asr_acceleration,
                                        notetaker_asr_proto::Provider::Cpu,
                                        "CPU",
                                    );
                                    #[cfg(windows)]
                                    ui.selectable_value(
                                        &mut settings.asr_acceleration,
                                        notetaker_asr_proto::Provider::Directml,
                                        "GPU (DirectML)",
                                    );
                                });
                        });
                        edits.multiline(
                            ui,
                            "Словарь (один термин на строку)",
                            &mut settings.vocabulary,
                        );
                        hint(ui, "Алиасы: werf = верф, вёрф.");
                        ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                            theme::toggle(ui, &mut settings.detect_browsers, "Учитывать браузеры");
                            theme::toggle(
                                ui,
                                &mut settings.detect_any_microphone,
                                "Любое приложение с микрофоном",
                            );
                            edits.multiline(
                                ui,
                                "Свои приложения для звонков",
                                &mut settings.call_apps,
                            );
                            let candidate = crate::config::Settings {
                                call_apps: settings.call_apps.clone(),
                                ..Default::default()
                            };
                            error(ui, candidate.validate());
                        });
                    },
                );
                ui.collapsing("Для опытных", |ui| {
                    theme::setting_row(ui, "Модель распознавания", |ui| {
                        crate::ui::model_selector(ui, "settings_model", &mut settings.model);
                    });
                    hint(ui, "Выбранная модель загрузится автоматически.");
                });
                ui.collapsing("Сохранённые голоса", |ui| {
                    if state.voices.voices.is_empty() {
                        hint(ui, "Голосов пока нет. Назовите участника в транскрипте.");
                    }
                    let mut delete = None;
                    for voice in &state.voices.voices {
                        theme::setting_row(ui, &voice.name, |ui| {
                            if ui.button("Удалить").clicked() {
                                delete = Some(voice.name.clone());
                            }
                        });
                    }
                    if let Some(name) = delete {
                        state.send(Command::Voices {
                            root: state.settings.library.clone(),
                            delete: Some(name),
                        });
                    }
                });
                #[cfg(target_os = "macos")]
                hint(
                    ui,
                    "macOS: системный звук целиком, кроме notetaker. Нужны разрешения Microphone и System Audio Recording.",
                );
                #[cfg(not(any(windows, target_os = "macos")))]
                hint(
                    ui,
                    "Захват звука, трей и глобальная клавиша недоступны на Linux.",
                );
            }
            Section::Calendars => {
                hint(
                    ui,
                    if cfg!(target_os = "macos") {
                        "Приватные ICS-подписки. URL хранится в macOS Keychain."
                    } else if cfg!(windows) {
                        "Приватные ICS-подписки. URL хранится в Windows Credential Manager."
                    } else {
                        "Хранение приватных URL недоступно на Linux."
                    },
                );
                edits.text(
                    ui,
                    "Часовой пояс без TZID",
                    &mut state.settings_draft.calendar_timezone,
                );
                error(
                    ui,
                    notetaker_core::calendar::timezone(&state.settings_draft.calendar_timezone)
                        .map(|_| ())
                        .map_err(Into::into),
                );
                let mut remove = None;
                let mut secret_changed = false;
                for calendar in &mut state.settings_draft.calendars {
                    ui.push_id(calendar.id.clone(), |ui| {
                        ui.add_space(12.0);
                        edits.text(ui, "Название календаря", &mut calendar.name);
                        error(ui, calendar.validate());
                        let input = self.calendar_inputs.entry(calendar.id.clone()).or_default();
                        ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                            let response = ui.add(
                                egui::TextEdit::singleline(input)
                                    .password(true)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("HTTPS URL ICS (пусто: сохранённый)"),
                            );
                            if response.lost_focus() && !input.trim().is_empty() {
                                match validate_calendar_secret(&calendar.id, input) {
                                    Ok(()) => {
                                        self.calendar_errors.remove(&calendar.id);
                                        state
                                            .calendar_secrets
                                            .insert(calendar.id.clone(), input.trim().to_owned());
                                        input.clear();
                                        secret_changed = true;
                                    }
                                    Err(error) => {
                                        self.calendar_errors
                                            .insert(calendar.id.clone(), error.to_string());
                                    }
                                }
                            }
                        });
                        if let Some(message) = self.calendar_errors.get(&calendar.id) {
                            ui.colored_label(Palette::of(ui).red, message);
                        }
                        if !state
                            .settings
                            .calendars
                            .iter()
                            .any(|saved| saved.id == calendar.id)
                            && !state.calendar_secrets.contains_key(&calendar.id)
                        {
                            hint(ui, "Укажите URL, затем перейдите к другому полю.");
                        }
                        if ui.small_button("Удалить календарь").clicked() {
                            remove = Some(calendar.id.clone());
                        }
                    });
                }
                if let Some(id) = remove {
                    state
                        .settings_draft
                        .calendars
                        .retain(|calendar| calendar.id != id);
                    state.calendar_secrets.remove(&id);
                    self.calendar_inputs.remove(&id);
                    self.calendar_errors.remove(&id);
                }
                ui.add_space(12.0);
                if ui
                    .add_enabled(
                        cfg!(any(windows, target_os = "macos"))
                            && state.settings_draft.calendars.len() < 16,
                        egui::Button::new("Добавить календарь"),
                    )
                    .clicked()
                {
                    state
                        .settings_draft
                        .calendars
                        .push(notetaker_llm::calendar::Subscription {
                            id: format!("calendar-{}", uuid::Uuid::now_v7()),
                            name: "Календарь".into(),
                        });
                }
                if ui
                    .add_enabled(
                        !state.calendar_busy,
                        egui::Button::new("Обновить календари"),
                    )
                    .clicked()
                {
                    state.refresh_calendar();
                }
                hint(ui, &state.calendar_status);
                hint(ui, "Обновление при запуске и каждые 10 минут.");
                if secret_changed {
                    state
                        .settings_autosave
                        .edit(std::time::Instant::now(), false, false);
                }
            }
            Section::Ai => {
                let actions =
                    state
                        .connections
                        .ui(ui, &state.llm_draft.defaults.hybrid, state.ready);
                for action in actions {
                    state.send(Command::Ai(action));
                }
                ui.collapsing("Для опытных", |ui| {
                    scenarios(ui, &mut state.llm_draft, &mut edits);
                    ui.separator();
                    providers(ui, &mut state.llm_draft, &mut edits);
                });
            }
            Section::Search => {
                ui.label("Поиск по словам и по смыслу");
                hint(
                    ui,
                    "Локальная модель загружается автоматически. До её загрузки работает поиск по словам. Тексты остаются на компьютере.",
                );
                crate::quickstart::model_status(ui, state);
                organization.shared_step(ui, !state.semantic_running && state.can_save_settings());
                theme::toggle(
                    ui,
                    &mut state.settings_draft.mcp_exclude_local,
                    "MCP: не отдавать Local-встречи",
                );
                ui.collapsing("Удалённый поиск по смыслу", |ui| {
                    hint(ui, "Только Hybrid и только без локальной модели. Local-встречи остаются в поиске по словам.");
                    let llm = &mut state.llm_draft;
                    theme::setting_row(ui, "Провайдер", |ui| {
                        egui::ComboBox::from_id_salt("embedding_provider").selected_text(if llm.embeddings.provider.is_empty() { "Выключено" } else { &llm.embeddings.provider }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut llm.embeddings.provider, String::new(), "Выключено");
                            for provider in &llm.providers { if provider.kind == "openai" { ui.selectable_value(&mut llm.embeddings.provider, provider.id.clone(), &provider.id); } }
                        });
                    });
                    edits.text(ui, "Модель embeddings", &mut llm.embeddings.model);
                    error(ui, llm.validate().map(|_| ()));
                });
            }
            Section::Storage => {
                let paths = (state.settings.library.clone(), state.settings.models_path());
                if self.usage_paths.as_ref() != Some(&paths)
                    || self
                        .usage_checked
                        .is_none_or(|at| at.elapsed().as_secs() >= 10)
                {
                    self.usage_checked = Some(std::time::Instant::now());
                    self.usage_paths = Some(paths.clone());
                    self.usage = None;
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.disk_usage = Some(rx);
                    std::thread::spawn(move || {
                        let _ = tx.send((directory_bytes(&paths.0), directory_bytes(&paths.1)));
                    });
                }
                if let Some(rx) = &self.disk_usage
                    && let Ok(usage) = rx.try_recv()
                {
                    self.usage = Some(usage);
                    self.disk_usage = None;
                }
                for (models, title, path) in [
                    (
                        false,
                        "Папка для встреч",
                        state.settings_draft.library.clone(),
                    ),
                    (
                        true,
                        "Папка для моделей",
                        state.settings_draft.models_path(),
                    ),
                ] {
                    theme::setting_row(ui, title, |ui| {
                        if ui
                            .add_enabled(
                                self.folder_picker.is_none(),
                                egui::Button::new("Выбрать…"),
                            )
                            .clicked()
                        {
                            let (tx, rx) = std::sync::mpsc::channel();
                            self.folder_picker = Some(rx);
                            let directory = path.clone();
                            std::thread::spawn(move || {
                                let result = rfd::FileDialog::new()
                                    .set_directory(directory)
                                    .pick_folder();
                                let _ = tx.send((models, result));
                            });
                        }
                    });
                    hint(ui, &notetaker_core::display_path(&path));
                }
                if let Some((library, models)) = self.usage {
                    hint(
                        ui,
                        &format!(
                            "На диске: встречи {:.0} МБ · модели {:.0} МБ",
                            library as f64 / 1_048_576.0,
                            models as f64 / 1_048_576.0
                        ),
                    );
                }
                hint(
                    ui,
                    "В новой папке недостающие модели загрузятся автоматически. Прежние файлы сохранятся.",
                );
                crate::quickstart::model_status(ui, state);
                ui.add_space(12.0);
                let settings = &mut state.settings_draft;
                ui.collapsing("Для опытных", |ui| {
                    let mut forever = settings.retention_days.is_none();
                    if theme::toggle(ui, &mut forever, "Хранить аудио всегда").changed()
                    {
                        settings.retention_days = if forever { None } else { Some(7) };
                    }
                    if let Some(days) = &mut settings.retention_days {
                        theme::setting_row(ui, "Хранить аудио", |ui| {
                            edits.number(
                                ui.add(
                                    egui::DragValue::new(days).range(0..=36_500).suffix(" дней"),
                                ),
                            );
                        });
                    }
                    hint(
                        ui,
                        "0 дней: удалить после чистового прохода. Применяется к новым записям.",
                    );
                    maintenance.settings(ui, settings);
                });
            }
        }
        let settings_changed =
            before != serde_json::to_value(&state.settings_draft).expect("settings serialize");
        let llm_changed =
            llm_before != serde_json::to_value(&state.llm_draft).expect("LLM serialize");
        if settings_changed || llm_changed || (edits.blur && state.settings_autosave.pending) {
            state.settings_edited(edits.typing && !edits.blur, llm_changed);
        }
        quickstart
    }
}

fn scenarios(ui: &mut egui::Ui, llm: &mut LlmSettings, edits: &mut Edits) {
    theme::section_caption(ui, "Модели по сценариям");
    for (label, choice) in [
        ("Конспект", &mut llm.scenarios.summary),
        ("Чат", &mut llm.scenarios.chat),
    ] {
        ui.push_id(label, |ui| {
            theme::setting_row(ui, label, |ui| {
                egui::ComboBox::from_id_salt("provider")
                    .selected_text(if choice.provider.is_empty() {
                        "По умолчанию"
                    } else {
                        &choice.provider
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut choice.provider, String::new(), "По умолчанию");
                        for provider in &llm.providers {
                            ui.selectable_value(
                                &mut choice.provider,
                                provider.id.clone(),
                                &provider.id,
                            );
                        }
                    });
            });
            edits.text(ui, "Модель (пусто: модель провайдера)", &mut choice.model);
        });
    }
    hint(
        ui,
        "Для Local облачный сценарий заменяется локальным провайдером по умолчанию.",
    );
}
fn providers(ui: &mut egui::Ui, llm: &mut LlmSettings, edits: &mut Edits) {
    theme::section_caption(ui, "Провайдеры");
    hint(
        ui,
        "Только имена переменных окружения. Ключи вводятся в подключении AI. Для Local разрешён только loopback.",
    );
    edits.text(ui, "По умолчанию Local", &mut llm.defaults.local);
    if !llm.providers.iter().any(|p| p.id == llm.defaults.local) {
        ui.colored_label(Palette::of(ui).red, "Провайдер Local не найден");
    }
    edits.text(ui, "По умолчанию Hybrid", &mut llm.defaults.hybrid);
    if !llm.providers.iter().any(|p| p.id == llm.defaults.hybrid) {
        ui.colored_label(Palette::of(ui).red, "Провайдер Hybrid не найден");
    }
    let mut remove = None;
    for (index, provider) in llm.providers.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.separator();
            edits.text(ui, "id", &mut provider.id);
            theme::setting_row(ui, "Тип", |ui| {
                egui::ComboBox::from_id_salt("kind")
                    .selected_text(&provider.kind)
                    .show_ui(ui, |ui| {
                        for kind in ["openai", "anthropic", "claude-cli", "codex-cli"] {
                            ui.selectable_value(&mut provider.kind, kind.into(), kind);
                        }
                    });
            });
            edits.text(ui, "base_url", &mut provider.base_url);
            edits.text(ui, "model", &mut provider.model);
            theme::setting_row(ui, "key_source", |ui| {
                egui::ComboBox::from_id_salt("key_source")
                    .selected_text(&provider.key_source)
                    .show_ui(ui, |ui| {
                        for source in ["env", "credential"] {
                            ui.selectable_value(&mut provider.key_source, source.into(), source);
                        }
                    });
            });
            let mut env = provider.api_key_env.clone().unwrap_or_default();
            edits.text(ui, "api_key_env", &mut env);
            if !env.is_empty()
                && !env.chars().enumerate().all(|(index, ch)| {
                    ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
                })
            {
                ui.colored_label(
                    Palette::of(ui).red,
                    "Укажите имя переменной, например OPENAI_API_KEY",
                );
            }
            provider.api_key_env = (!env.is_empty()).then_some(env);
            theme::setting_row(ui, "context_tokens", |ui| {
                edits.number(
                    ui.add(egui::DragValue::new(&mut provider.context_tokens).range(1..=2_000_000)),
                );
            });
            error(ui, provider.config().map(|_| ()));
            if ui.small_button("Удалить провайдер").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        llm.providers.remove(index);
    }
    if ui.button("Добавить провайдер").clicked() {
        let mut number = llm.providers.len() + 1;
        while llm
            .providers
            .iter()
            .any(|p| p.id == format!("provider-{number}"))
        {
            number += 1;
        }
        llm.providers.push(ProviderSettings {
            id: format!("provider-{number}"),
            ..Default::default()
        });
    }
    error(ui, llm.validate().map(|_| ()));
}

// Ignore symlinks to avoid cycles and counting files outside the selected folder.
fn directory_bytes(path: &std::path::Path) -> u64 {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}
