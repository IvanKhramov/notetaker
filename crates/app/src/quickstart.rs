//! Small first-run flow. Model setup runs independently in the background.
use crate::{
    state::AppState,
    theme::{self, Palette},
    worker::{self, Command},
};
use eframe::egui;
use std::{
    process::Stdio,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Ready,
    #[default]
    Missing,
    Error(String),
}
#[derive(Clone, Debug, Default)]
pub struct Models {
    pub gigaam: Status,
    pub parakeet: Status,
    pub auto: Status,
    pub diarization: Status,
    pub semantic: Status,
}
impl Models {
    pub fn get(&self, model: &str) -> &Status {
        match model {
            "gigaam-v3" => &self.gigaam,
            "parakeet-v3" => &self.parakeet,
            "auto" => &self.auto,
            "semantic" => &self.semantic,
            _ => &self.diarization,
        }
    }
    pub fn speech_ready(&self, model: &str) -> bool {
        self.get(model) == &Status::Ready
    }
    pub fn missing<'a>(&self, model: &'a str) -> Option<&'a str> {
        [model, "diarization", "semantic"]
            .into_iter()
            .find(|id| self.get(id) != &Status::Ready)
    }
}
#[derive(Default)]
pub struct Runtime {
    pub models: Models,
    pub download_model: Option<String>,
    pub download_error: Option<String>,
    pub calendar_error: Option<String>,
    pub probe_busy: bool,
    pub retry_at: Option<Instant>,
    checked_at: Option<Instant>,
}

/// Download in priority order, retry with a delay, and periodically detect removed
/// files. Never overlap downloads with recording or a folder preference change.
pub fn auto_download(state: &mut AppState) {
    if !state.ready
        || state.downloading
        || state.recording.busy()
        || state.settings_pending
        || state.settings_autosave.pending
        || state.exit_requested
    {
        return;
    }
    if state
        .quickstart
        .retry_at
        .is_some_and(|at| Instant::now() < at)
    {
        return;
    }
    if let Some(model) = state
        .quickstart
        .models
        .missing(&state.settings.model)
        .map(str::to_owned)
    {
        state.quickstart.retry_at = None;
        state.download(model);
    } else if state
        .quickstart
        .checked_at
        .is_some_and(|at| at.elapsed() > Duration::from_secs(60))
    {
        state.quickstart.checked_at = Some(Instant::now());
        state.send(Command::CheckModels);
    } else {
        state.quickstart.checked_at.get_or_insert_with(Instant::now);
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
        semantic: if notetaker_core::embedding::installed()
            && worker::background_process(&notetaker_core::embedding::worker_path())
                .arg("check-model")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        {
            Status::Ready
        } else {
            Status::Missing
        },
    }
}

/// One quiet status line shared by welcome and storage. Progress is in phases,
/// so byte totals or percentages would be misleading.
pub fn model_status(ui: &mut egui::Ui, state: &mut AppState) {
    if state.downloading {
        let ready = [&*state.settings.model, "diarization", "semantic"]
            .into_iter()
            .filter(|id| state.quickstart.models.get(id) == &Status::Ready)
            .count();
        ui.horizontal_wrapped(|ui| {
            ui.spinner();
            ui.weak(format!("Подготовка к записи: скачиваю {} из 3…", ready + 1));
        });
    } else if let Some(error) = state.quickstart.download_error.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(Palette::of(ui).red, error);
            if theme::button(ui, "Повторить", crate::theme::ButtonStyle::Ghost).clicked() {
                state.quickstart.retry_at = None;
            }
        });
        ui.small("Повторю автоматически через минуту.");
    } else if state
        .quickstart
        .models
        .missing(&state.settings.model)
        .is_none()
    {
        ui.weak("Все модели на месте");
    } else {
        ui.weak("Подготовка к записи…");
    }
}

#[derive(Default)]
pub struct Panel {
    pub open: bool,
    startup_checked: bool,
    step: usize,
    selected_ai: usize,
    calendar_url: String,
    probe: Option<Receiver<ProbeEvent>>,
    probe_status: Option<[Status; 2]>,
}
impl Panel {
    pub fn reopen(&mut self) {
        self.open = true;
        self.step = 0;
    }
    fn accept_calendar(&mut self, state: &mut AppState) {
        if self.calendar_url.trim().is_empty() {
            return;
        }
        let id = state
            .settings_draft
            .calendars
            .first()
            .map(|c| c.id.clone())
            .unwrap_or_else(|| format!("calendar-{}", uuid::Uuid::now_v7()));
        if let Err(error) = crate::settings::validate_calendar_secret(&id, &self.calendar_url) {
            state.quickstart.calendar_error = Some(error.to_string());
            return;
        }
        if state.settings_draft.calendars.is_empty() {
            state
                .settings_draft
                .calendars
                .push(notetaker_llm::calendar::Subscription {
                    id: id.clone(),
                    name: "Календарь".into(),
                });
        }
        state
            .calendar_secrets
            .insert(id, self.calendar_url.trim().to_owned());
        self.calendar_url.clear();
        state.quickstart.calendar_error = None;
        state.save_preferences();
    }
    pub fn blur(&mut self, state: &mut AppState) {
        state.connections.blur();
        if cfg!(any(windows, target_os = "macos")) {
            self.accept_calendar(state);
        }
    }
    pub fn tick(&mut self, state: &mut AppState) {
        if state.ready && !self.startup_checked {
            self.startup_checked = true;
            self.open = state.settings.needs_onboarding();
            if self.open {
                // Mark the first presentation, so even dismissing the window is
                // a one-time experience. Explicit reruns never reset this flag.
                state.settings_draft.onboarding_complete = Some(true);
                state.save_preferences();
            }
        }
        if let Some(receiver) = &self.probe {
            match receiver.try_recv() {
                Ok(ProbeEvent::Done(result)) => {
                    self.probe_status = Some(result.unwrap_or_else(|error| {
                        [Status::Error(error.clone()), Status::Error(error)]
                    }));
                    state.quickstart.probe_busy = false;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.probe_status =
                        Some([Status::Error("Проверка прервалась".into()), Status::Missing]);
                    state.quickstart.probe_busy = false;
                }
            }
            if !state.quickstart.probe_busy {
                self.probe = None;
            }
        }
    }
    fn start_probe(&mut self, state: &mut AppState) {
        if !cfg!(any(windows, target_os = "macos"))
            || state.quickstart.probe_busy
            || state.recording.busy()
            || state.exit_requested
        {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.probe = Some(receiver);
        self.probe_status = None;
        state.quickstart.probe_busy = true;
        std::thread::spawn(move || {
            let result = probe().map_err(|error| format!("{error:#}"));
            let _ = sender.send(ProbeEvent::Done(result));
        });
    }
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if !self.open || !state.ready {
            return;
        }
        let mut open = true;
        let mut next = false;
        let alpha = theme::appearance(ctx, egui::Id::unique("quickstart-fade"));
        let modal=egui::Modal::new(egui::Id::unique("quickstart"))
            .backdrop_color(egui::Color32::from_black_alpha(((if ctx.global_style().visuals.dark_mode {140.0} else {89.0})*alpha) as u8))
            .frame(egui::Frame::window(&ctx.global_style()))
            .show(ctx, |ui| {
                ui.multiply_opacity(alpha);
                ui.set_width((ctx.content_rect().width() - 64.0).min(520.0));
                ui.style_mut().text_styles.insert(egui::TextStyle::Heading,theme::semibold(20.0));
                ui.horizontal(|ui| {
                    for step in 0..3 {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 3.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect,255,if step == self.step { Palette::of(ui).text } else { Palette::of(ui).border_strong });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, theme::icons::X, "Закрыть").clicked() { open = false; }
                    });
                });
                ui.add_space(8.0);
                // Reserve the bar's lane so it never covers the right edge of fields.
                ui.style_mut().spacing.scroll.floating_allocated_width = theme::SCROLL_LANE;
                egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - if self.step == 0 { 320.0 } else { 180.0 }).max(80.0))
                    .show(ui, |ui| {
                    match self.step {
                        0 => {
                            ui.heading("Встреча идёт. Заметки остаются.");
                            ui.label("Пишите главное. notetaker запишет разговор и поможет с конспектом.");
                            ui.add_space(16.0);
                            ui.label("Проверим звук?");
                            ui.label("Говорите и включите звук на компьютере. Проверка займёт 5 секунд, аудио не сохраняется.");
                            for (index, title) in ["Микрофон", "Системный звук"].into_iter().enumerate() {
                                let status = self.probe_status.as_ref().map(|s| &s[index]);
                                match status {
                                    Some(Status::Ready) => { ui.label(format!("✓ {title}: работает")); }
                                    Some(Status::Error(error)) => { ui.colored_label(Palette::of(ui).red, format!("{title}: {error}")); }
                                    _ => { ui.weak(format!("{title}: {}", if state.quickstart.probe_busy { "проверяю…" } else { "не проверен" })); }
                                }
                            }
                            #[cfg(target_os = "macos")]
                            ui.small("macOS: Настройки системы → Конфиденциальность и безопасность → Microphone и System Audio Recording. Разрешите доступ для notetaker.");
                            #[cfg(windows)]
                            ui.small("Если нет доступа: Параметры → Конфиденциальность → Микрофон. Разрешите доступ приложениям.");
                            #[cfg(not(any(windows, target_os = "macos")))]
                            ui.small("Проверка и запись звука доступны на macOS и Windows.");
                            if self.probe_status.is_some() && theme::button(ui, "Проверить ещё раз", crate::theme::ButtonStyle::Ghost).clicked() { self.start_probe(state); }

                        }
                        1 => {
                            ui.heading("Как вас зовут?");
                            ui.label("Этим именем отметим ваш голос в транскрипте.");
                            let response = ui.add(crate::theme::text_field(&mut state.settings_draft.user_name)
                                .hint_text("Ваше имя").desired_width(f32::INFINITY));
                            if response.changed() { state.settings_edited(true, false); }
                            if response.lost_focus() { state.save_preferences(); }
                        }
                        _ => {
                            ui.heading("Подключить помощников?");
                            ui.label("Необязательно. Всё можно настроить позже.");
                            crate::theme::disclosure("Календарь").show(ui, |ui| {
                                ui.label("Встречи из календаря появятся на главном экране.");
                                if state.settings.calendars.is_empty() {
                                    ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                                        let response = ui.add(crate::theme::text_field(&mut self.calendar_url)
                                            .password(true).hint_text("HTTPS URL ICS").desired_width(f32::INFINITY));
                                        if response.lost_focus() { self.accept_calendar(state); }
                                    });
                                    if let Some(error) = &state.quickstart.calendar_error { ui.colored_label(Palette::of(ui).red, error); }
                                } else { ui.label("Календарь подключён"); }
                                if ui.add(theme::styled_button(Palette::of(ui),"Пропустить календарь",theme::ButtonStyle::Ghost).icon(theme::icons::X)).clicked() { self.calendar_url.clear(); }
                            });
                            crate::theme::disclosure("AI для конспектов").show(ui, |ui| {
                                ui.label("Текст встречи будет отправлен выбранному AI. Аудио останется на компьютере.");
                                for action in state.connections.onboarding_ui(ui, state.ready, &mut self.selected_ai) {
                                    state.send(Command::Ai(action));
                                }
                                ui.weak("Пропустить: оставьте AI без подключения.");
                            });
                        }
                    }
                });
                if self.step == 0 {
                    ui.add_space(12.0);
                    model_status(ui, state);
                    ui.small(if state.quickstart.models.speech_ready(&state.settings.model) {
                        "Распознавание готово. Можно начинать запись."
                    } else { "Запись станет доступна после подготовки распознавания." });
                }
                ui.add_space(16.0);
                if let Some(error) = &state.settings_autosave.error { ui.colored_label(Palette::of(ui).red, error); }
                theme::footer(ui, |ui| {
                    let check_audio = self.step == 0 && self.probe_status.is_none();
                    let label = if self.step == 2 { "Начать работу" } else if check_audio { "Проверить звук" } else { "Продолжить" };
                    if ui.add_enabled(!state.quickstart.probe_busy, theme::styled_button(Palette::of(ui), label, theme::ButtonStyle::Primary)).clicked() {
                        if check_audio && cfg!(any(windows, target_os = "macos")) { self.start_probe(state); }
                        else { next = true; }
                    }
                    if theme::button(ui, "Пропустить", crate::theme::ButtonStyle::Ghost).clicked() { next = true; }
                });
            });
        if modal.should_close() {
            open = false;
        }
        if next {
            self.blur(state);
            state.save_preferences();
            if self.step == 2 {
                open = false;
            } else {
                self.step += 1;
            }
        }
        if !open {
            self.blur(state);
        }
        self.open = open;
    }
}

pub enum ProbeEvent {
    Done(Result<[Status; 2], String>),
}
fn probe() -> anyhow::Result<[Status; 2]> {
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
    }
    for source in &mut sources {
        if let Err(error) = source.stop()
            && result.is_ok()
        {
            result = Err(error);
        }
    }
    result?;
    Ok(meters.each_ref().map(|meter| {
        if meter.levels().samples == 0 {
            Status::Error("Нет доступа к звуку".into())
        } else if meter.levels().is_silent(-60.0) {
            Status::Error("Тишина. Включите звук и повторите проверку".into())
        } else {
            Status::Ready
        }
    }))
}
