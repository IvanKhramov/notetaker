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
    save_error: Option<String>,
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
        let response = setting(ui, label, |ui| {
            ui.add(crate::theme::text_field(value).desired_width(ui.available_width().min(240.0)))
        });
        self.typing |= response.changed();
        self.blur |= response.lost_focus();
    }
    fn number(&mut self, response: egui::Response) {
        self.typing |= response.changed() && response.has_focus() && !response.dragged();
        self.blur |= response.lost_focus();
    }
    fn multiline(&mut self, ui: &mut egui::Ui, label: &str, value: &mut String) {
        let response = setting(ui, label, |ui| {
            ui.add(
                egui::TextEdit::multiline(value)
                    .desired_rows(2)
                    .desired_width(ui.available_width()),
            )
        });
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
        ui: &mut egui::Ui,
        open: &mut bool,
        state: &mut AppState,
        maintenance: &mut crate::maintenance::Maintenance,
        organization: &mut crate::organization::Panel,
    ) -> bool {
        let ctx = ui.ctx().clone();
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
        if self.save_error != state.settings_autosave.error {
            self.save_error = state.settings_autosave.error.clone();
            if let Some(error) = &self.save_error {
                state
                    .notices
                    .push((theme::ToastLevel::Error, error.clone()));
            }
        }
        let mut quickstart = false;
        let previous_section = self.section;
        let mut close = false;
        close |= theme::escape_to_close(&ctx);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Palette::for_dark(ctx.global_style().visuals.dark_mode).bg)
                    .inner_margin(0),
            )
            .show(ui, |ui| {
                ui.style_mut()
                    .text_styles
                    .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
                egui::ScrollArea::vertical()
                    .id_salt(("settings-page", self.section as u8))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new().inner_margin(32).show(ui, |ui| {
                            ui.set_max_width(ui.available_width().min(680.0));
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.heading(self.section.label())
                                .on_hover_text(state.settings_status());
                            ui.add_space(24.0);
                            quickstart = self.body(ui, state, maintenance, organization);
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
                theme::settings_group(ui, "Оформление", |ui| {
                    setting(ui, "Тема", |ui| {
                        crate::theme::select("theme")
                            .selected_text(settings.theme.label())
                            .show_ui(ui, |ui| {
                                for theme in [
                                    theme::Theme::Light,
                                    theme::Theme::Dark,
                                    theme::Theme::System,
                                ] {
                                    crate::theme::select_value(
                                        ui,
                                        &mut settings.theme,
                                        theme,
                                        theme.label(),
                                    );
                                }
                            });
                    });
                    setting(ui, "Масштаб интерфейса", |ui| {
                        crate::theme::select("ui_zoom")
                            .selected_text(format!("{:.0}%", settings.ui_zoom * 100.0))
                            .show_ui(ui, |ui| {
                                for value in [0.9, 1.0, 1.1, 1.25, 1.5] {
                                    crate::theme::select_value(
                                        ui,
                                        &mut settings.ui_zoom,
                                        value,
                                        format!("{:.0}%", value * 100.0),
                                    );
                                }
                            });
                    });
                });
                theme::settings_group(ui, "Профиль", |ui| {
                    edits.text(ui, "Ваше имя", &mut settings.user_name);
                    edits.text(ui, "Другие варианты имени", &mut settings.user_aliases);
                });
                theme::settings_group(ui, "Запуск и клавиши", |ui| {
                    ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                        edits.text(ui, "Горячая клавиша записи", &mut settings.hotkey);
                        error(ui, crate::config::validate_hotkey(&settings.hotkey));
                    });
                    maintenance.windows_settings(ui);
                    setting(ui, "Быстрый старт", |ui| {
                        quickstart = ui
                            .add(
                                theme::styled_button(
                                    Palette::of(ui),
                                    "Открыть",
                                    ButtonStyle::Ghost,
                                )
                                .right_text(theme::icons::CHEVRON_RIGHT),
                            )
                            .clicked();
                    });
                });
                theme::settings_group(ui, "Уведомления и обновления", |ui| {
                    toggle(
                        ui,
                        &mut settings.digest_reminder,
                        "Напоминать о дайджесте по пятницам",
                    );
                    ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                        toggle(ui, &mut settings.check_updates, "Проверять обновления");

                        setting(
                            ui,
                            &format!("Версия {}", env!("CARGO_PKG_VERSION")),
                            |ui| {
                                maintenance.check_updates_button(ui);
                            },
                        );
                    });
                });
            }
            Section::Recording => {
                #[cfg(not(any(windows, target_os = "macos")))]
                theme::callout(ui, theme::icons::INFO, |ui| {
                    ui.small("Захват звука, трей и глобальная клавиша недоступны на Linux.");
                });
                #[cfg(target_os = "macos")]
                theme::callout(ui, theme::icons::INFO, |ui| {
                    ui.small("macOS: системный звук целиком, кроме notetaker. Нужны разрешения Microphone и System Audio Recording.");
                });
                ui.add_space(24.0);
                let settings = &mut state.settings_draft;
                theme::settings_group(ui, "Запись", |ui| {
                    setting(ui, "Режим новых встреч", |ui| {
                        crate::theme::select("settings_mode")
                            .selected_text(if settings.mode == notetaker_core::Mode::Local {
                                "Локально"
                            } else {
                                "Гибрид"
                            })
                            .show_ui(ui, |ui| {
                                crate::theme::select_value(
                                    ui,
                                    &mut settings.mode,
                                    notetaker_core::Mode::Local,
                                    "Локально",
                                );
                                crate::theme::select_value(
                                    ui,
                                    &mut settings.mode,
                                    notetaker_core::Mode::Hybrid,
                                    "Гибрид",
                                );
                            });
                    });

                    ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                        toggle(ui, &mut settings.detect_calls, "Предлагать запись звонков");
                    });
                });
                theme::settings_group(ui, "Обработка", |ui| {
                    toggle(
                        ui,
                        &mut settings.auto_summary,
                        "Создавать конспект после встречи",
                    );
                    toggle(ui, &mut settings.voice_library, "Запоминать голоса");

                    setting(ui, "Автостоп по тишине", |ui| {
                        edits.number(
                            ui.add(
                                egui::DragValue::new(&mut settings.auto_stop_minutes)
                                    .range(0..=u32::MAX)
                                    .suffix(" мин"),
                            ),
                        );
                    });
                });
                theme::settings_disclosure(
                    ui,
                    "Распознавание и обнаружение звонков",
                    |ui| {
                        toggle(ui, &mut settings.diarize, "Разделять собеседников");
                        setting(ui, "Число собеседников", |ui| {
                            crate::theme::select("speakers")
                                .selected_text(
                                    settings.speakers.map_or("Авто".into(), |n| n.to_string()),
                                )
                                .show_ui(ui, |ui| {
                                    crate::theme::select_value(
                                        ui,
                                        &mut settings.speakers,
                                        None,
                                        "Авто",
                                    );
                                    for n in 1..=32 {
                                        crate::theme::select_value(
                                            ui,
                                            &mut settings.speakers,
                                            Some(n),
                                            n.to_string(),
                                        );
                                    }
                                });
                        });
                        setting(ui, "Потоков ASR (0: авто)", |ui| {
                            edits.number(
                                ui.add(
                                    egui::DragValue::new(&mut settings.asr_threads).range(0..=8),
                                ),
                            );
                        });
                        setting(ui, "Ускорение ASR", |ui| {
                            crate::theme::select("acceleration")
                                .selected_text(format!("{:?}", settings.asr_acceleration))
                                .show_ui(ui, |ui| {
                                    crate::theme::select_value(
                                        ui,
                                        &mut settings.asr_acceleration,
                                        notetaker_asr_proto::Provider::Auto,
                                        "Авто",
                                    );
                                    crate::theme::select_value(
                                        ui,
                                        &mut settings.asr_acceleration,
                                        notetaker_asr_proto::Provider::Cpu,
                                        "CPU",
                                    );
                                    #[cfg(windows)]
                                    crate::theme::select_value(
                                        ui,
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

                        ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                            toggle(ui, &mut settings.detect_browsers, "Учитывать браузеры");
                            toggle(
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
                theme::settings_disclosure(ui, "Для опытных", |ui| {
                    setting(ui, "Модель распознавания", |ui| {
                        crate::ui::model_selector(ui, "settings_model", &mut settings.model);
                    });
                });
                theme::settings_disclosure(ui, "Сохранённые голоса", |ui| {
                    if state.voices.voices.is_empty() {
                        hint(ui, "Голосов пока нет. Назовите участника в транскрипте.");
                    }
                    let mut delete = None;
                    for voice in &state.voices.voices {
                        setting(ui, &voice.name, |ui| {
                            if crate::theme::button(ui, "Удалить", crate::theme::ButtonStyle::Ghost)
                                .clicked()
                            {
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
            }
            Section::Calendars => {
                theme::callout(ui, theme::icons::INFO, |ui| {
                    ui.small(if cfg!(target_os = "macos") {
                        "Приватные ICS-подписки. URL хранится в macOS Keychain."
                    } else if cfg!(windows) {
                        "Приватные ICS-подписки. URL хранится в Windows Credential Manager."
                    } else {
                        "Хранение приватных URL недоступно на Linux."
                    });
                });
                ui.add_space(24.0);
                theme::settings_group(ui, "Подписки", |ui| {
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
                            let input =
                                self.calendar_inputs.entry(calendar.id.clone()).or_default();
                            ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                                let response = setting(ui, "URL календаря", |ui| {
                                    ui.add(
                                        crate::theme::text_field(input)
                                            .password(true)
                                            .desired_width(f32::INFINITY)
                                            .hint_text("HTTPS URL ICS (пусто: сохранённый)"),
                                    )
                                });
                                if response.lost_focus() && !input.trim().is_empty() {
                                    match validate_calendar_secret(&calendar.id, input) {
                                        Ok(()) => {
                                            self.calendar_errors.remove(&calendar.id);
                                            state.calendar_secrets.insert(
                                                calendar.id.clone(),
                                                input.trim().to_owned(),
                                            );
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
                            if crate::theme::button(
                                ui,
                                "Удалить календарь",
                                crate::theme::ButtonStyle::Ghost,
                            )
                            .clicked()
                            {
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
                    setting(ui, "Подписки", |ui| {
                        if ui
                            .add_enabled(
                                cfg!(any(windows, target_os = "macos"))
                                    && state.settings_draft.calendars.len() < 16,
                                crate::theme::styled_button(
                                    crate::theme::Palette::of(ui),
                                    "Добавить календарь",
                                    crate::theme::ButtonStyle::Secondary,
                                ),
                            )
                            .clicked()
                        {
                            state.settings_draft.calendars.push(
                                notetaker_llm::calendar::Subscription {
                                    id: format!("calendar-{}", uuid::Uuid::now_v7()),
                                    name: "Календарь".into(),
                                },
                            );
                        }
                    });
                    let description = if state.calendar_status.is_empty() {
                        "Обновление при запуске и каждые 10 минут.".to_owned()
                    } else {
                        format!(
                            "Обновление при запуске и каждые 10 минут.\n{}",
                            state.calendar_status
                        )
                    };
                    theme::settings_row_desc(ui, "Обновление", Some(&description), |ui| {
                        if ui
                            .add_enabled(
                                !state.calendar_busy,
                                crate::theme::styled_button(
                                    crate::theme::Palette::of(ui),
                                    "Обновить календари",
                                    crate::theme::ButtonStyle::Secondary,
                                ),
                            )
                            .clicked()
                        {
                            state.refresh_calendar();
                        }
                    });
                    if secret_changed {
                        state
                            .settings_autosave
                            .edit(std::time::Instant::now(), false, false);
                    }
                });
            }
            Section::Ai => {
                let actions =
                    state
                        .connections
                        .ui(ui, &state.llm_draft.defaults.hybrid, state.ready);
                for action in actions {
                    state.send(Command::Ai(action));
                }
                theme::settings_disclosure(ui, "Для опытных", |ui| {
                    scenarios(ui, &mut state.llm_draft, &mut edits);
                    ui.separator();
                    providers(ui, &mut state.llm_draft, &mut edits);
                });
            }
            Section::Search => {
                theme::settings_group(ui, "Локальный поиск", |ui| {
                    theme::settings_row_desc(
                        ui,
                        "Поиск по словам и по смыслу",
                        Some(
                            "Локальная модель загружается автоматически. До её загрузки работает поиск по словам. Тексты остаются на компьютере.",
                        ),
                        |ui| crate::quickstart::model_status(ui, state),
                    );
                });
                theme::settings_group(ui, "Библиотека", |ui| {
                    organization
                        .shared_step(ui, !state.semantic_running && state.can_save_settings());
                });
                theme::settings_group(ui, "MCP", |ui| {
                    toggle(
                        ui,
                        &mut state.settings_draft.mcp_exclude_local,
                        "MCP: не отдавать Локально-встречи",
                    );
                });
                theme::settings_disclosure(
                    ui,
                    "Удалённый поиск по смыслу",
                    |ui| {
                        hint(
                            ui,
                            "Только Гибрид и только без локальной модели. Локально-встречи остаются в поиске по словам.",
                        );
                        let llm = &mut state.llm_draft;
                        setting(ui, "Провайдер", |ui| {
                            crate::theme::select("embedding_provider")
                                .selected_text(if llm.embeddings.provider.is_empty() {
                                    "Выключено"
                                } else {
                                    &llm.embeddings.provider
                                })
                                .show_ui(ui, |ui| {
                                    crate::theme::select_value(
                                        ui,
                                        &mut llm.embeddings.provider,
                                        String::new(),
                                        "Выключено",
                                    );
                                    for provider in &llm.providers {
                                        if provider.kind == "openai" {
                                            crate::theme::select_value(
                                                ui,
                                                &mut llm.embeddings.provider,
                                                provider.id.clone(),
                                                &provider.id,
                                            );
                                        }
                                    }
                                });
                        });
                        edits.text(ui, "Модель embeddings", &mut llm.embeddings.model);
                        error(ui, llm.validate().map(|_| ()));
                    },
                );
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
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = tx.send((directory_bytes(&paths.0), directory_bytes(&paths.1)));
                        ctx.request_repaint();
                    });
                }
                if let Some(rx) = &self.disk_usage
                    && let Ok(usage) = rx.try_recv()
                {
                    self.usage = Some(usage);
                    self.disk_usage = None;
                }
                if self.disk_usage.is_none()
                    && let Some(checked) = self.usage_checked
                {
                    ui.ctx().request_repaint_after(
                        (checked + std::time::Duration::from_secs(10))
                            .saturating_duration_since(std::time::Instant::now()),
                    );
                }
                theme::settings_group(ui, "Папки", |ui| {
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
                        theme::settings_row_desc(
                            ui,
                            title,
                            Some(&notetaker_core::display_path(&path)),
                            |ui| {
                                if ui
                                    .add_enabled(
                                        self.folder_picker.is_none(),
                                        crate::theme::styled_button(
                                            crate::theme::Palette::of(ui),
                                            "Выбрать…",
                                            crate::theme::ButtonStyle::Secondary,
                                        ),
                                    )
                                    .clicked()
                                {
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    self.folder_picker = Some(rx);
                                    let directory = path.clone();
                                    let ctx = ui.ctx().clone();
                                    std::thread::spawn(move || {
                                        let result = rfd::FileDialog::new()
                                            .set_directory(directory)
                                            .pick_folder();
                                        let _ = tx.send((models, result));
                                        ctx.request_repaint();
                                    });
                                }
                            },
                        );
                    }
                });
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
                theme::settings_disclosure(ui, "Для опытных", |ui| {
                    let mut forever = settings.retention_days.is_none();
                    if toggle(ui, &mut forever, "Хранить аудио всегда").changed()
                    {
                        settings.retention_days = if forever { None } else { Some(7) };
                    }
                    if let Some(days) = &mut settings.retention_days {
                        setting(ui, "Хранить аудио", |ui| {
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
            setting(ui, label, |ui| {
                crate::theme::select("provider")
                    .selected_text(if choice.provider.is_empty() {
                        "По умолчанию"
                    } else {
                        &choice.provider
                    })
                    .show_ui(ui, |ui| {
                        crate::theme::select_value(
                            ui,
                            &mut choice.provider,
                            String::new(),
                            "По умолчанию",
                        );
                        for provider in &llm.providers {
                            crate::theme::select_value(
                                ui,
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
        "Для Локально облачный сценарий заменяется локальным провайдером по умолчанию.",
    );
}
fn providers(ui: &mut egui::Ui, llm: &mut LlmSettings, edits: &mut Edits) {
    theme::section_caption(ui, "Провайдеры");
    hint(
        ui,
        "Только имена переменных окружения. Ключи вводятся в подключении AI. Для Локально разрешён только loopback.",
    );
    edits.text(ui, "По умолчанию Локально", &mut llm.defaults.local);
    if !llm.providers.iter().any(|p| p.id == llm.defaults.local) {
        ui.colored_label(Palette::of(ui).red, "Провайдер Локально не найден");
    }
    edits.text(ui, "По умолчанию Гибрид", &mut llm.defaults.hybrid);
    if !llm.providers.iter().any(|p| p.id == llm.defaults.hybrid) {
        ui.colored_label(Palette::of(ui).red, "Провайдер Гибрид не найден");
    }
    let mut remove = None;
    for (index, provider) in llm.providers.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.separator();
            edits.text(ui, "id", &mut provider.id);
            setting(ui, "Тип", |ui| {
                crate::theme::select("kind")
                    .selected_text(&provider.kind)
                    .show_ui(ui, |ui| {
                        for kind in ["openai", "anthropic", "claude-cli", "codex-cli"] {
                            crate::theme::select_value(ui, &mut provider.kind, kind.into(), kind);
                        }
                    });
            });
            edits.text(ui, "base_url", &mut provider.base_url);
            edits.text(ui, "model", &mut provider.model);
            setting(ui, "key_source", |ui| {
                crate::theme::select("key_source")
                    .selected_text(&provider.key_source)
                    .show_ui(ui, |ui| {
                        for source in ["env", "credential"] {
                            crate::theme::select_value(
                                ui,
                                &mut provider.key_source,
                                source.into(),
                                source,
                            );
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
            setting(ui, "context_tokens", |ui| {
                edits.number(
                    ui.add(egui::DragValue::new(&mut provider.context_tokens).range(1..=2_000_000)),
                );
            });
            error(ui, provider.config().map(|_| ()));
            if crate::theme::button(ui, "Удалить провайдер", crate::theme::ButtonStyle::Ghost)
                .clicked()
            {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        llm.providers.remove(index);
    }
    if crate::theme::button(ui, "Добавить провайдер", crate::theme::ButtonStyle::Ghost).clicked()
    {
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

fn toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    setting(ui, label, |ui| theme::switch(ui, value, label))
}
fn setting<R>(ui: &mut egui::Ui, label: &str, value: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let description = match label {
        "Режим новых встреч" => Some(
            "Локально: локальный AI. Гибрид: текст может отправляться провайдеру. Аудио остаётся на компьютере.",
        ),
        "Запоминать голоса" => {
            Some("Голосовые отпечатки хранятся только локально.")
        }
        "Автостоп по тишине" => {
            Some("0 минут: выключен. Завершение звонка не останавливает запись.")
        }
        "Модель распознавания" => {
            Some("Выбранная модель загрузится автоматически.")
        }
        "Проверять обновления" => Some("Раз в час и при запуске."),
        "Словарь (один термин на строку)" => {
            Some("Алиасы: werf = верф, вёрф.")
        }
        "URL календаря" => Some("Укажите URL, затем перейдите к другому полю."),
        "Масштаб интерфейса" => Some(if cfg!(target_os = "macos") {
            "Cmd+= / Cmd+- / Cmd+0: изменить или сбросить масштаб."
        } else {
            "Ctrl+= / Ctrl+- / Ctrl+0: изменить или сбросить масштаб."
        }),
        _ => None,
    };
    theme::settings_row_desc(ui, label, description, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editing_profile_keeps_draft_until_saved_and_survives_worker_reply() {
        let (mut state, commands, events) = crate::test_ui::state();
        state.ready = true;
        state.settings.user_name = "Before".into();
        state.settings_draft = state.settings.clone();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut panel = Panel::default();
        let mut maintenance = crate::maintenance::Maintenance::default();
        let mut organization = crate::organization::Panel::default();
        let mut open = true;
        {
            let mut draw = |ui: &mut egui::Ui| {
                panel.show(
                    ui,
                    &mut open,
                    &mut state,
                    &mut maintenance,
                    &mut organization,
                );
            };
            crate::test_ui::replace_text(&ctx, &mut draw, "Before", "After");
        }
        assert_eq!(state.settings_draft.user_name, "After");
        assert_eq!(state.settings.user_name, "Before");
        panel.blur(&mut state);
        let (settings, llm, persist_llm) = commands
            .try_iter()
            .find_map(|command| match command {
                Command::Settings {
                    settings,
                    llm,
                    persist_llm,
                    ..
                } => Some((settings, llm, persist_llm)),
                _ => None,
            })
            .expect("profile edit must schedule a settings save");
        assert_eq!(settings.user_name, "After");
        events
            .send(crate::worker::Event::SettingsSaved(
                Ok((settings, llm)),
                persist_llm,
            ))
            .unwrap();
        state.poll();
        assert_eq!(state.settings.user_name, "After");
        assert_eq!(state.settings_draft.user_name, "After");
        assert!(!state.settings_pending);
    }
}
