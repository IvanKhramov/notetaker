//! Startup checklist and shared setup controls. No model weights are loaded here.
use std::{
    process::Stdio,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

use eframe::egui;

use crate::{
    config::{LlmSettings, Settings},
    maintenance::Maintenance,
    state::AppState,
    theme::{self, Palette},
    worker::{self, Command},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Ready,
    #[default]
    Missing,
    Error(String),
}
impl Status {
    fn label(&self) -> &str {
        match self {
            Self::Ready => "[+] Готово",
            Self::Missing => "[ ] Не настроено",
            Self::Error(_) => "[!] Ошибка",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Models {
    pub gigaam: Status,
    pub parakeet: Status,
    pub auto: Status,
    pub diarization: Status,
}
impl Models {
    fn asr(&self) -> Status {
        if self.gigaam == Status::Ready || self.parakeet == Status::Ready {
            Status::Ready
        } else if let Status::Error(error) = &self.gigaam {
            Status::Error(error.clone())
        } else {
            self.parakeet.clone()
        }
    }
    fn get(&self, model: &str) -> &Status {
        match model {
            "gigaam-v3" => &self.gigaam,
            "parakeet-v3" => &self.parakeet,
            "auto" => &self.auto,
            _ => &self.diarization,
        }
    }
}

/// Pure checklist computation, including configured providers outside the cards.
pub fn step_states(
    models: &Models,
    settings: &Settings,
    llm: &LlmSettings,
    llm_configured: bool,
    provider_error: Option<&str>,
    calendar_error: Option<&str>,
) -> [Status; 6] {
    let configured = llm_configured
        && llm
            .providers
            .iter()
            .any(|p| p.id == llm.defaults.hybrid && p.config().is_ok());
    [
        if settings.model == "auto" {
            models.auto.clone()
        } else {
            models.asr()
        },
        models.diarization.clone(),
        if let Some(error) = provider_error {
            Status::Error(error.into())
        } else if configured {
            Status::Ready
        } else {
            Status::Missing
        },
        if settings.user_name.trim().is_empty() {
            Status::Missing
        } else {
            Status::Ready
        },
        if let Some(error) = calendar_error {
            Status::Error(error.into())
        } else if settings.calendars.is_empty() {
            Status::Missing
        } else {
            Status::Ready
        },
        // Both enabled and disabled are valid choices for call preferences.
        Status::Ready,
    ]
}

pub fn essential_missing(steps: &[Status; 6]) -> usize {
    [0, 2]
        .into_iter()
        .filter(|&index| steps[index] != Status::Ready)
        .count()
}

pub fn should_auto_download(settings: &Settings, models: &Models) -> bool {
    settings.auto_download_models
        && !settings.auto_diarization_attempted
        && models.asr() == Status::Ready
        && models.diarization == Status::Missing
}

#[derive(Default)]
pub struct Runtime {
    pub models: Models,
    pub download_model: Option<String>,
    pub download_error: Option<String>,
    pub calendar_error: Option<String>,
    pub probe_busy: bool,
    auto_attempted: bool,
    auto_pending: bool,
}

/// Persist the one-time attempt before starting network activity, also after the
/// first ASR download. A failed save/download does not retry in a loop.
pub fn auto_download(state: &mut AppState) {
    if state.quickstart.auto_pending
        && !state.settings_pending
        && !state.downloading
        && !state.recording.busy()
        && !state.quickstart.probe_busy
        && !state.exit_requested
    {
        state.quickstart.auto_pending = false;
        if state.settings.auto_diarization_attempted
            && state.settings.auto_download_models
            && state.quickstart.models.diarization == Status::Missing
        {
            state.notices.push((
                crate::theme::ToastLevel::Info,
                "Скачиваю модели разделения собеседников...".into(),
            ));
            state.download("diarization".into());
        }
    }
    if !state.quickstart.auto_attempted
        && !state.downloading
        && !state.quickstart.probe_busy
        && state.can_save_settings()
        && should_auto_download(&state.settings, &state.quickstart.models)
    {
        state.quickstart.auto_attempted = true;
        state.settings_draft.auto_diarization_attempted = true;
        state.save_preferences();
        state.quickstart.auto_pending = state.settings_pending;
    }
}

pub fn check_models() -> Models {
    let check = |model| -> Status {
        let result = worker::asr_binary().and_then(|binary| {
            Ok(worker::background_process(&binary)
                .args(["check-model", model])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?)
        });
        match result {
            Ok(status) if status.success() => Status::Ready,
            Ok(_) => Status::Missing,
            Err(error) => Status::Error(format!("{error:#}")),
        }
    };
    Models {
        gigaam: check("gigaam-v3"),
        parakeet: check("parakeet-v3"),
        auto: check("auto"),
        diarization: check("diarization"),
    }
}

/// Used by Settings and Quick start with the same worker and phase progress.
pub fn model_control(ui: &mut egui::Ui, state: &mut AppState, model: &str, title: &str) {
    let status = state.quickstart.models.get(model).clone();
    ui.horizontal_wrapped(|ui| {
        ui.label(title);
        ui.small(status.label());
        if status == Status::Ready && crate::config::MODELS.contains(&model) {
            if state.settings.model == model {
                ui.small("По умолчанию");
            } else if ui
                .add_enabled(state.can_save_settings(), egui::Button::new("Выбрать"))
                .clicked()
            {
                state.settings_draft.model = model.into();
                state.save_preferences();
            }
        }
        if status != Status::Ready
            && ui
                .add_enabled(
                    !state.downloading && !state.recording.busy(),
                    egui::Button::new("Скачать"),
                )
                .clicked()
        {
            state.download(model.into());
        }
    });
    if state.quickstart.download_model.as_deref() == Some(model) {
        if state.downloading {
            // The downloader reports phases, not byte counts. Do not invent a percentage.
            ui.add(
                egui::ProgressBar::new(0.0)
                    .animate(true)
                    .text("Загрузка и установка..."),
            );
            if let Some(message) = state.download_messages.back() {
                ui.small(message);
            }
        } else if let Some(error) = &state.quickstart.download_error {
            ui.colored_label(Palette::of(ui).red, error);
        }
    }
    if let Status::Error(error) = state.quickstart.models.get(model) {
        ui.colored_label(Palette::of(ui).red, error);
    }
}

#[derive(Default)]
pub struct Panel {
    pub open: bool,
    startup_checked: bool,
    banner_dismissed: bool,
    edit_ai: bool,
    ai_connecting: bool,
    selected_ai: usize,
    calendar_url: String,
    calendar_connecting: bool,
    probe: Option<Receiver<ProbeEvent>>,
    levels: [f32; 2],
    probe_status: Option<Status>,
}
impl Panel {
    pub fn tick(&mut self, state: &mut AppState) {
        if state.ready && !self.startup_checked {
            self.startup_checked = true;
            self.open = !state.settings.hide_quickstart;
        }
        if self.ai_connecting && !state.settings_pending {
            self.ai_connecting = false;
            self.edit_ai = false;
        }
        if self.calendar_connecting && !state.settings_pending {
            self.calendar_connecting = false;
            if state.quickstart.calendar_error.is_none() {
                self.calendar_url.clear();
            }
        }
        if let Some(receiver) = &self.probe {
            loop {
                match receiver.try_recv() {
                    Ok(ProbeEvent::Levels(levels)) => self.levels = levels,
                    Ok(ProbeEvent::Done(result)) => {
                        self.probe_status = Some(match result {
                            Ok(()) => Status::Ready,
                            Err(error) => Status::Error(error),
                        });
                        state.quickstart.probe_busy = false;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.probe_status = Some(Status::Error("Проверка звука прервалась".into()));
                        state.quickstart.probe_busy = false;
                        break;
                    }
                }
            }
            if !state.quickstart.probe_busy {
                self.probe = None;
            }
        }
    }
    fn steps(&self, state: &AppState) -> [Status; 6] {
        let mut steps = step_states(
            &state.quickstart.models,
            &state.settings,
            &state.llm,
            state.llm_configured,
            state.connections.provider_error(&state.llm.defaults.hybrid),
            state.quickstart.calendar_error.as_deref(),
        );
        if let Some(error) = &state.quickstart.download_error
            && state.quickstart.download_model.as_deref() != Some("semantic")
        {
            let index = if state.quickstart.download_model.as_deref() == Some("diarization") {
                1
            } else {
                0
            };
            if steps[index] != Status::Ready {
                steps[index] = Status::Error(error.clone());
            }
        }
        steps
    }
    pub fn header(&mut self, ui: &mut egui::Ui) {
        if theme::rail_button(ui, theme::Icon::Help, "Быстрый старт", self.open).clicked()
        {
            self.open = true;
        }
    }
    pub fn sidebar(&mut self, ui: &mut egui::Ui, state: &AppState) {
        let missing = essential_missing(&self.steps(state));
        if state.ready && missing > 0 && !self.banner_dismissed {
            theme::bar(Palette::of(ui)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.small(format!(
                    "Настройка не завершена: {missing} {}",
                    if missing == 1 { "шаг" } else { "шага" }
                ));
                ui.horizontal(|ui| {
                    if ui.small_button("Продолжить").clicked() {
                        self.open = true;
                    }
                    if ui.small_button("Скрыть").clicked() {
                        self.banner_dismissed = true;
                    }
                });
            });
        }
    }
    /// Returns true when the user chooses the full Settings dialog.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &mut AppState,
        maintenance: &mut Maintenance,
        organization: &mut crate::organization::Panel,
    ) -> bool {
        if !self.open || !state.ready {
            return false;
        }
        let steps = self.steps(state);
        let mut open = self.open;
        let mut settings = false;
        let mut done = false;
        let mut save = false;
        theme::window(ctx, "Быстрый старт")
            .id(egui::Id::unique("quickstart"))
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(620.0)
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                theme::section_caption(ui, "Быстрый старт");
                let completed = steps.iter().filter(|s| **s == Status::Ready).count();
                ui.heading(format!("Настроено {completed} из 6"));
                ui.add(egui::ProgressBar::new(completed as f32 / 6.0).fill(Palette::of(ui).accent).desired_height(4.0));
                ui.small("Настройте запись и конспекты здесь. Календарь и проверку звука можно пропустить.");
                egui::ScrollArea::vertical().id_salt("quickstart_steps").max_height((ctx.content_rect().height() - 350.0).max(100.0)).show(ui, |ui| {
                    semantic_step(ui, state);
                    ui.separator();
                    organization.shared_step(ui, !state.semantic_running && state.can_save_settings());
                    ui.separator();
                    row(ui, &steps[0], "Распознавание речи", "Модели работают локально. Для Авто нужны обе модели и Whisper tiny (~103 МБ).");
                    model_control(ui, state, "gigaam-v3", "GigaAM v3 · русский");
                    model_control(ui, state, "parakeet-v3", "Parakeet v3 · многоязычный");
                    model_control(ui, state, "auto", "Авто (рус/англ)");
                    row(ui, &steps[1], "Разделение собеседников", "Отмечает, кто говорил. Модели занимают около 47 МБ.");
                    model_control(ui, state, "diarization", "Модели собеседников");
                    ui.add_enabled_ui(state.can_save_settings(), |ui| {
                        save |= ui.checkbox(&mut state.settings_draft.auto_download_models, "Скачивать модели автоматически").changed();
                    });
                    row(ui, &steps[2], "AI для конспектов", "Провайдер по умолчанию для конспектов и чата.");
                    if steps[2] == Status::Ready && !self.edit_ai {
                        let id = &state.llm.defaults.hybrid;
                        let model = state.llm.providers.iter().find(|p| &p.id == id).map_or("", |p| p.model.as_str());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("Подключено: {}, {model}", state.connections.provider_title(id)));
                            if ui.link("Изменить").clicked() {
                                self.selected_ai = state.connections.provider_index(id).unwrap_or(self.selected_ai);
                                self.edit_ai = true;
                            }
                        });
                    } else {
                        let can_default = state.can_save_settings();
                        for action in state.connections.quickstart_ui(ui, &state.llm_draft.defaults.hybrid, can_default, &mut self.selected_ai) {
                            self.ai_connecting |= action.operation == crate::ai::Operation::Default;
                            state.send(Command::Ai(action));
                        }
                        if steps[2] == Status::Ready && ui.link("Свернуть").clicked() { self.edit_ai = false; }
                    }
                    if state.settings.mode == notetaker_core::Mode::Hybrid {
                        ui.small("Hybrid: тексты встреч отправляются выбранному AI. Аудио остаётся на компьютере.");
                    } else {
                        ui.small("Подключение облачного AI включит Hybrid для новых встреч.");
                    }
                    row(ui, &steps[3], "Как вас зовут", "Для фильтра «Мои» и имени вашего спикера.");
                    ui.add_enabled_ui(state.can_save_settings(), |ui| {
                        let blur = identity_control(ui, &mut state.settings_draft);
                        save |= blur && (state.settings_draft.user_name != state.settings.user_name || state.settings_draft.user_aliases != state.settings.user_aliases);
                    });
                    row(ui, &steps[4], "Календарь (необязательно)", "Приватная HTTPS ICS-подписка, URL хранится в Credential Manager.");
                    if state.settings.calendars.is_empty() {
                        ui.add_enabled_ui(state.can_save_settings() && cfg!(any(windows, target_os = "macos")), |ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.calendar_url).password(true).hint_text("HTTPS URL ICS"));
                            if ui.add_enabled(!self.calendar_url.trim().is_empty(), egui::Button::new("Подключить")).clicked() {
                                let id = if let Some(calendar) = state.settings_draft.calendars.first() { calendar.id.clone() } else {
                                    let id = format!("calendar-{}", uuid::Uuid::now_v7());
                                    state.settings_draft.calendars.push(notetaker_llm::calendar::Subscription { id: id.clone(), name: "Календарь".into() });
                                    id
                                };
                                state.calendar_secrets.insert(id, self.calendar_url.trim().to_owned());
                                self.calendar_connecting = true;
                                save = true;
                            }
                        });
                        #[cfg(not(any(windows, target_os = "macos")))]
                        ui.small("Сохранение URL доступно на Windows.");
                    } else {
                        let today = chrono::Local::now().date_naive();
                        let count = state.calendar_events.iter().filter(|event| event.start.with_timezone(&chrono::Local).date_naive() == today).count();
                        ui.label(format!("{count} событий сегодня"));
                        ui.add_enabled_ui(state.can_save_settings() && cfg!(any(windows, target_os = "macos")), |ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.calendar_url).password(true).hint_text("Новый HTTPS URL ICS (при необходимости)"));
                            if ui.add_enabled(!self.calendar_url.trim().is_empty(), egui::Button::new("Подключить")).clicked() {
                                let id = state.settings.calendars[0].id.clone();
                                state.calendar_secrets.insert(id, self.calendar_url.trim().to_owned());
                                self.calendar_connecting = true;
                                save = true;
                            }
                        });
                        if ui.add_enabled(!state.calendar_busy, egui::Button::new("Обновить")).clicked() { state.refresh_calendar(); }
                        if state.calendar_busy { ui.spinner(); }
                        ui.small(&state.calendar_status);
                    }
                    row(ui, &steps[5], "Звонки", if cfg!(target_os = "macos") { "Предложения записи и запуск приложения при входе в macOS." } else { "Предложения записи и запуск приложения вместе с Windows." });
                    ui.add_enabled_ui(state.can_save_settings() && cfg!(any(windows, target_os = "macos")), |ui| {
                        save |= ui.checkbox(&mut state.settings_draft.detect_calls, "Предлагать запись при звонке").changed();
                    });
                    maintenance.windows_settings(ui);
                    row(ui, self.probe_status.as_ref().unwrap_or(&Status::Missing), "Проверка звука (необязательно)", "5 секунд: говорите и включите звук звонка. Аудио не сохраняется.");
                    if ui.add_enabled(cfg!(any(windows, target_os = "macos")) && !state.quickstart.probe_busy && !state.recording.busy() && !state.settings_pending && !state.exit_requested, egui::Button::new("Проверить")).clicked() {
                        let (sender, receiver) = mpsc::channel();
                        self.probe = Some(receiver);
                        self.levels = [0.0; 2];
                        self.probe_status = None;
                        state.quickstart.probe_busy = true;
                        std::thread::spawn(move || {
                            let result = probe(&sender).map_err(|error| format!("{error:#}"));
                            let _ = sender.send(ProbeEvent::Done(result));
                        });
                    }
                    for (label, level) in ["Микрофон", "Системный звук"].into_iter().zip(self.levels) {
                        ui.add(egui::ProgressBar::new(level).text(label));
                    }
                    if state.quickstart.probe_busy { ui.spinner(); ctx.request_repaint_after(Duration::from_millis(50)); }
                });
                ui.separator();
                ui.add_enabled_ui(state.can_save_settings(), |ui| {
                    save |= ui.checkbox(&mut state.settings_draft.hide_quickstart, "Больше не показывать при запуске").changed();
                });
                ui.horizontal(|ui| {
                    if theme::button(ui, "Готово", theme::ButtonStyle::Primary).clicked() { done = true; }
                    if ui.button("Открыть все настройки").clicked() { settings = true; }
                    if state.settings_pending { ui.spinner(); }
                });
            });
        if done || !open || settings {
            save |= state.settings_draft.user_name != state.settings.user_name
                || state.settings_draft.user_aliases != state.settings.user_aliases;
        }
        if save {
            state.save_preferences();
        }
        self.open = open && !done && !settings;
        settings
    }
}

fn row(ui: &mut egui::Ui, status: &Status, title: &str, explanation: &str) {
    ui.add_space(16.0);
    ui.horizontal_wrapped(|ui| {
        if matches!(status, Status::Error(_)) {
            ui.colored_label(Palette::of(ui).red, status.label());
        } else {
            ui.small(status.label());
        }
        ui.strong(title);
    });
    ui.small(explanation);
    if let Status::Error(error) = status {
        ui.colored_label(Palette::of(ui).red, error);
    }
}

/// Shared fields. The caller decides whether to autosave or use Settings' Save.
pub fn identity_control(ui: &mut egui::Ui, settings: &mut Settings) -> bool {
    let name = ui.add(egui::TextEdit::singleline(&mut settings.user_name).hint_text("Ваше имя"));
    let mut save = name.lost_focus();
    ui.collapsing("Алиасы (необязательно)", |ui| {
        save |= ui
            .add(egui::TextEdit::singleline(&mut settings.user_aliases).hint_text("Через запятую"))
            .lost_focus();
    });
    save
}

pub enum ProbeEvent {
    Levels([f32; 2]),
    Done(Result<(), String>),
}
fn probe(sender: &Sender<ProbeEvent>) -> anyhow::Result<()> {
    use notetaker_capture::{
        AudioSource, CaptureEvent, CaptureMessage, Channel, LevelMeter, LiveOptions, live_sources,
    };
    let origin = Instant::now();
    let mut sources = live_sources(LiveOptions {
        origin,
        ..Default::default()
    })?;
    let (tx, rx) = mpsc::sync_channel(128);
    let mut result = Ok(());
    for source in &mut sources {
        if let Err(error) = source.start(tx.clone()) {
            result = Err(error);
            break;
        }
    }
    drop(tx);
    let mut meters = [LevelMeter::default(), LevelMeter::default()];
    let mut last_update = Instant::now();
    while result.is_ok() && origin.elapsed() < Duration::from_secs(5) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(CaptureMessage::Frame(frame)) => {
                let index = if frame.channel == Channel::Mic { 0 } else { 1 };
                meters[index].push(&frame.samples);
            }
            Ok(CaptureMessage::Event(CaptureEvent::Error { message, .. })) => {
                result = Err(anyhow::anyhow!(message));
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                result = Err(anyhow::anyhow!("Захват звука завершился раньше времени"));
            }
        }
        if last_update.elapsed() >= Duration::from_millis(100) {
            let levels = meters
                .each_ref()
                .map(|meter| ((meter.levels().average_dbfs + 60.0) / 60.0).clamp(0.0, 1.0) as f32);
            let _ = sender.send(ProbeEvent::Levels(levels));
            last_update = Instant::now();
        }
    }
    for source in &mut sources {
        if let Err(error) = source.stop()
            && result.is_ok()
        {
            result = Err(error);
        }
    }
    result?;
    anyhow::ensure!(
        meters.iter().all(|meter| meter.levels().samples > 0),
        "Не получен звук с одного из каналов"
    );
    anyhow::ensure!(
        meters.iter().all(|meter| !meter.levels().is_silent(-60.0)),
        "На одном из каналов тишина. Говорите и включите системный звук, затем повторите проверку."
    );
    Ok(())
}

/// Optional model: excluded from the six required setup steps and auto downloads.
pub fn semantic_step(ui: &mut egui::Ui, state: &mut AppState) {
    ui.strong("Поиск по смыслу (необязательно)");
    ui.small("Локальная модель: 135 МБ. Тексты встреч остаются на компьютере.");
    if notetaker_core::embedding::installed() {
        ui.label("Модель установлена");
        if !notetaker_core::embedding::worker_path().is_file()
            && ui
                .add_enabled(
                    !state.downloading,
                    egui::Button::new("Установить воркер поиска по смыслу"),
                )
                .clicked()
        {
            state.download("semantic".into());
        }
        if ui
            .add_enabled(
                !state.semantic_running,
                egui::Button::new("Обновить индекс по смыслу"),
            )
            .clicked()
        {
            state.start_semantic_index();
        }
    } else if ui
        .add_enabled(
            !state.downloading && !state.recording.busy(),
            egui::Button::new("Скачать модель поиска по смыслу"),
        )
        .clicked()
    {
        state.download("semantic".into());
    }
    if state.quickstart.download_model.as_deref() == Some("semantic") {
        if state.downloading {
            ui.spinner();
            if let Some(message) = state.download_messages.back() {
                ui.small(message);
            }
        }
        if let Some(error) = &state.quickstart.download_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }
    if state.semantic_running {
        ui.spinner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_readiness_requires_language_identifier_as_well_as_both_asr_models() {
        let models = Models {
            gigaam: Status::Ready,
            parakeet: Status::Ready,
            ..Default::default()
        };
        let settings = Settings {
            model: "auto".into(),
            ..Settings::default()
        };
        let llm = LlmSettings::default();
        assert_eq!(
            step_states(&models, &settings, &llm, false, None, None)[0],
            Status::Missing
        );
        let models = Models {
            auto: Status::Ready,
            ..models
        };
        assert_eq!(
            step_states(&models, &settings, &llm, false, None, None)[0],
            Status::Ready
        );
    }

    #[test]
    fn checklist_distinguishes_models_provider_identity_and_optional_calendar() {
        let mut settings = Settings::default();
        let mut models = Models::default();
        let llm = LlmSettings::default();
        let steps = step_states(&models, &settings, &llm, false, None, None);
        assert_eq!(
            steps,
            [
                Status::Missing,
                Status::Missing,
                Status::Missing,
                Status::Missing,
                Status::Missing,
                Status::Ready
            ]
        );
        assert_eq!(essential_missing(&steps), 2);
        for english in [false, true] {
            models = Models::default();
            if english {
                models.parakeet = Status::Ready;
            } else {
                models.gigaam = Status::Ready;
            }
            let steps = step_states(&models, &settings, &llm, true, None, None);
            assert_eq!(essential_missing(&steps), 0);
            assert_eq!(steps[1], Status::Missing);
        }
        models.diarization = Status::Ready;
        settings.user_name = "Иван".into();
        settings
            .calendars
            .push(notetaker_llm::calendar::Subscription {
                id: "calendar-test".into(),
                name: "Личный".into(),
            });
        assert!(
            step_states(&models, &settings, &llm, true, None, None)
                .iter()
                .all(|s| *s == Status::Ready)
        );
        settings.detect_calls = false;
        settings.hide_quickstart = true;
        assert_eq!(
            essential_missing(&step_states(&models, &settings, &llm, true, None, None)),
            0
        );
        assert_eq!(
            step_states(
                &models,
                &settings,
                &llm,
                true,
                Some("Нет доступа"),
                Some("Сеть")
            )[2],
            Status::Error("Нет доступа".into())
        );
        let mut invalid = llm;
        invalid.defaults.hybrid = "unknown".into();
        assert_eq!(
            step_states(&models, &settings, &invalid, true, None, None)[2],
            Status::Missing
        );
        models.gigaam = Status::Error("worker".into());
        models.parakeet = Status::Missing;
        assert!(matches!(
            step_states(&models, &settings, &invalid, true, None, None)[0],
            Status::Error(_)
        ));
    }

    #[test]
    fn auto_download_requires_asr_and_runs_once_unless_disabled() {
        let mut settings = Settings::default();
        let mut models = Models::default();
        assert!(!should_auto_download(&settings, &models));
        models.gigaam = Status::Ready;
        assert!(should_auto_download(&settings, &models));
        settings.auto_diarization_attempted = true;
        assert!(!should_auto_download(&settings, &models));
        settings.auto_diarization_attempted = false;
        settings.auto_download_models = false;
        assert!(!should_auto_download(&settings, &models));
        settings.auto_download_models = true;
        models.diarization = Status::Ready;
        assert!(!should_auto_download(&settings, &models));
        models.diarization = Status::Error("worker".into());
        assert!(!should_auto_download(&settings, &models));
    }
}
