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
    Warning(String),
    Denied,
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
#[derive(Clone, Debug)]
pub struct Transfer {
    pub file: String,
    pub received: u64,
    pub total: u64,
    pub last_byte: Instant,
    files: std::collections::BTreeMap<String, (u64, u64)>,
    plan_complete: bool,
    started: Instant,
    network_bytes: u64,
}
impl Transfer {
    pub fn waiting(now: Instant) -> Self {
        Self {
            file: String::new(),
            received: 0,
            total: 0,
            last_byte: now,
            files: Default::default(),
            plan_complete: false,
            started: now,
            network_bytes: 0,
        }
    }
    pub fn update(&mut self, line: &str, now: Instant) {
        if let Some(known) = line.strip_prefix("NOTETAKER_DOWNLOAD_PLAN_END ") {
            self.plan_complete = known == "1";
            return;
        }
        if let Some(line) = line.strip_prefix("NOTETAKER_DOWNLOAD_PLAN ") {
            let fields: Vec<_> = line.split_whitespace().collect();
            if let [file, total, installed] = fields.as_slice()
                && let Ok(total) = total.parse::<u64>()
            {
                self.files.insert(
                    (*file).into(),
                    (if *installed == "1" { total } else { 0 }, total),
                );
            }
            return;
        }
        let Some(line) = line.strip_prefix("NOTETAKER_DOWNLOAD ") else {
            return;
        };
        let fields: Vec<_> = line.split_whitespace().collect();
        if let [file, received, total] = fields.as_slice()
            && let (Ok(received), Ok(total)) = (received.parse::<u64>(), total.parse::<u64>())
        {
            let previous = self.files.get(*file).map_or(0, |value| value.0);
            self.network_bytes += received.saturating_sub(previous);
            self.files.insert((*file).into(), (received, total));
            if *file != self.file || received > self.received {
                self.last_byte = now;
            }
            self.file = (*file).into();
            self.received = received;
            self.total = total;
        }
    }
    pub fn overall(&self) -> (u64, u64) {
        self.files
            .values()
            .fold((0, 0), |(done, total), (file_done, file_total)| {
                (done + file_done.min(file_total), total + file_total)
            })
    }
    pub fn fraction(&self) -> Option<f32> {
        let (done, total) = self.overall();
        (self.plan_complete && total > 0 && self.files.values().all(|(_, total)| *total > 0))
            .then(|| done as f32 / total as f32)
    }
    pub fn remaining_minutes(&self, now: Instant) -> Option<u64> {
        let seconds = now.saturating_duration_since(self.started).as_secs_f64();
        let (done, total) = self.overall();
        if self.fraction().is_none()
            || seconds < 3.0
            || self.network_bytes < 1_000_000
            || done >= total
            || self.stalled(now)
        {
            return None;
        }
        Some(
            (((total - done) as f64 / (self.network_bytes as f64 / seconds) / 60.0).ceil() as u64)
                .max(1),
        )
    }
    pub fn stalled(&self, now: Instant) -> bool {
        (self.total == 0 || self.received < self.total)
            && now.saturating_duration_since(self.last_byte) >= Duration::from_secs(30)
    }
    pub fn text(&self, model: &str, phase: usize) -> String {
        let label = match self.file.as_str() {
            "gigaam-v3" => "модель русской речи",
            "parakeet-v3" => "модель многоязычной речи",
            "silero-vad" => "определение пауз",
            "whisper-tiny" => "определение языка",
            "pyannote-3.0" => "разделение собеседников",
            "eres2net-base" => "распознавание голосов",
            "model.onnx" => "модель поиска по смыслу",
            "tokenizer.json" => "словарь поиска по смыслу",
            _ => match model {
                "diarization" => "разделение собеседников",
                "semantic" => "поиск по смыслу",
                _ => "модель распознавания речи",
            },
        };
        let progress = if self.total == 0 {
            format!("{} МБ · размер уточняется", self.received / 1_000_000)
        } else {
            format!(
                "{:.1} из {:.1} МБ",
                self.received as f64 / 1_000_000.0,
                self.total as f64 / 1_000_000.0
            )
        };
        let eta = self
            .remaining_minutes(Instant::now())
            .map_or(String::new(), |minutes| {
                format!(" · осталось около {minutes} мин")
            });
        format!("Подготовка к записи: {label}, этап {phase} из 3 · {progress}{eta}")
    }
}
#[derive(Default)]
pub struct Runtime {
    pub models: Models,
    pub download_model: Option<String>,
    pub transfer: Option<Transfer>,
    pub retry_requested: bool,
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
pub fn wake_after(state: &AppState, now: Instant) -> Option<Duration> {
    if !state.ready
        || state.downloading
        || state.recording.busy()
        || state.settings_pending
        || state.settings_autosave.pending
        || state.exit_requested
    {
        return None;
    }
    let due = if state
        .quickstart
        .models
        .missing(&state.settings.model)
        .is_some()
    {
        state.quickstart.retry_at?
    } else {
        state.quickstart.checked_at? + Duration::from_secs(60)
    };
    Some(due.saturating_duration_since(now))
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

/// Shared phase and per-file byte progress for welcome, library and storage.
pub fn model_status(ui: &mut egui::Ui, state: &mut AppState) {
    if state.downloading {
        let ready = [&*state.settings.model, "diarization", "semantic"]
            .into_iter()
            .filter(|id| state.quickstart.models.get(id) == &Status::Ready)
            .count();
        ui.horizontal_wrapped(|ui| {
            crate::theme::spinner(ui);
            let text = state
                .quickstart
                .transfer
                .as_ref()
                .map(|transfer| {
                    transfer.text(
                        state.quickstart.download_model.as_deref().unwrap_or("auto"),
                        ready + 1,
                    )
                })
                .unwrap_or_else(|| {
                    format!("Подготовка к записи: подключение, {} из 3…", ready + 1)
                });
            ui.add(egui::Label::new(egui::RichText::new(text).color(Palette::of(ui).muted)).wrap());
        });
        let fraction = state
            .quickstart
            .transfer
            .as_ref()
            .and_then(Transfer::fraction);
        theme::progress(ui, fraction, ui.available_width());
        if let Some(transfer) = &state.quickstart.transfer {
            let (done, total) = transfer.overall();
            if fraction.is_some() {
                ui.weak(format!(
                    "Всего: {:.1} из {:.1} МБ",
                    done as f64 / 1_000_000.0,
                    total as f64 / 1_000_000.0
                ));
            }
        }
        if state
            .quickstart
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.stalled(Instant::now()))
        {
            ui.weak("Загрузка не идёт. Проверьте сеть.");
            if theme::button(ui, "Повторить", theme::ButtonStyle::Ghost).clicked() {
                state.retry_download();
            }
        }
    } else if let Some(error) = state.quickstart.download_error.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(Palette::of(ui).red, error);
            if theme::button(ui, "Повторить", crate::theme::ButtonStyle::Ghost).clicked() {
                state.retry_download();
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
                    state.permissions.refresh();
                    if cfg!(any(target_os = "macos", windows))
                        && let Ok(statuses) = &result
                    {
                        for (index, status) in statuses.iter().enumerate() {
                            match status {
                                Status::Ready => state.permissions.audio_received(index),
                                Status::Denied => {
                                    state.permissions.channels[index] =
                                        notetaker_capture::Permission::Denied;
                                    state.permissions.failed[index] = true;
                                }
                                Status::Error(_) => state.permissions.capture_failed(index),
                                Status::Warning(_)
                                    if index == 0
                                        && (state.permissions.query)()[0]
                                            == notetaker_capture::Permission::Unknown =>
                                {
                                    state.permissions.channels[0] =
                                        notetaker_capture::Permission::Unknown;
                                    state.permissions.failed[0] = false;
                                }
                                _ => {}
                            }
                        }
                    }
                    self.probe_status = Some(result.unwrap_or_else(|error| {
                        [
                            Status::Error(error.clone()),
                            if cfg!(target_os = "macos") {
                                sound_status(0, true, true)
                            } else {
                                Status::Error(error)
                            },
                        ]
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
    pub(crate) fn start_probe(&mut self, state: &mut AppState) {
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
        state.permissions.refresh();
        if matches!(
            state.permissions.channels[0],
            notetaker_capture::Permission::NotAsked | notetaker_capture::Permission::Denied
        ) {
            state.permissions.repeat_after_microphone_prompt = true;
        }
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
                                let denied = Status::Denied;
                                let status = if state.permissions.channels[index] == notetaker_capture::Permission::Denied { Some(&denied) } else { self.probe_status.as_ref().map(|statuses| &statuses[index]) };
                                probe_line(ui, title, status, index == 1 && cfg!(target_os = "macos"), state.quickstart.probe_busy);
                            }
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
                                crate::calendar_ui::setup_help(ui);
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
        if theme::modal_should_close(&modal) {
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
    let mut errors: [Option<String>; 2] = [None, None];
    let mut denied = [false; 2];
    for (index, source) in sources.iter_mut().enumerate() {
        if let Err(error) = source.start(tx.clone()) {
            errors[index] = Some(format!("{error:#}"));
        }
    }
    drop(tx);
    let mut meters = [LevelMeter::default(), LevelMeter::default()];
    // Start the measurement after synchronous startup, which may include a prompt.
    let measuring = Instant::now();
    while measuring.elapsed() < Duration::from_secs(5) {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(CaptureMessage::Frame(frame)) => {
                let index = if frame.channel == Channel::Mic { 0 } else { 1 };
                if frame.samples.iter().any(|sample| sample.abs() > 0.0005) {
                    denied[index] = false;
                    errors[index] = None;
                }
                meters[index].push(&frame.samples);
            }
            Ok(CaptureMessage::Event(CaptureEvent::PermissionDenied { channel })) => {
                denied[if channel == Channel::Mic { 0 } else { 1 }] = true;
            }
            Ok(CaptureMessage::Event(CaptureEvent::Error { channel, message })) => {
                errors[if channel == Channel::Mic { 0 } else { 1 }] = Some(message);
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for (index, source) in sources.iter_mut().enumerate() {
        if let Err(error) = source.stop()
            && errors[index].is_none()
        {
            errors[index] = Some(format!("{error:#}"));
        }
    }
    Ok(std::array::from_fn(|index| {
        if denied[index] {
            return Status::Denied;
        }
        if let Some(error) = errors[index].take() {
            return Status::Error(error);
        }
        let levels = meters[index].levels();
        sound_status(
            levels.samples,
            levels.is_silent(-60.0),
            index == 1 && cfg!(target_os = "macos"),
        )
    }))
}

pub fn sound_status(samples: u64, silent: bool, mac_system: bool) -> Status {
    if samples > 0 && !silent {
        Status::Ready
    } else if mac_system {
        Status::Warning("нет звука или доступа. Включите звук и проверьте разрешение на запись системного звука".into())
    } else {
        Status::Warning("тишина. Включите любой звук и повторите проверку".into())
    }
}
pub fn probe_line(
    ui: &mut egui::Ui,
    title: &str,
    status: Option<&Status>,
    mac_system: bool,
    busy: bool,
) {
    match status {
        Some(Status::Ready) => {
            ui.label(format!("✓ {title}: работает"));
        }
        Some(Status::Denied) => {
            ui.colored_label(
                Palette::of(ui).red,
                if mac_system {
                    "Нет доступа к системному звуку"
                } else {
                    crate::permissions::MICROPHONE_DENIED
                },
            );
            crate::permissions::open_settings(ui);
        }
        Some(Status::Warning(reason)) => {
            ui.colored_label(Palette::of(ui).warning, format!("{title}: {reason}"));
        }
        Some(Status::Error(reason)) => {
            ui.colored_label(
                if mac_system {
                    Palette::of(ui).warning
                } else {
                    Palette::of(ui).red
                },
                format!("{title}: {reason}"),
            );
        }
        _ => {
            ui.weak(format!(
                "{title}: {}",
                if busy {
                    "проверяю…"
                } else {
                    "не проверен"
                }
            ));
        }
    }
    if mac_system && matches!(status, Some(Status::Warning(_) | Status::Error(_))) {
        ui.small("Настройки системы → Конфиденциальность и безопасность → Запись экрана и системного звука. Разрешите доступ для notetaker.");
        crate::permissions::open_settings(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quickstart_advances_three_steps_preserving_name_and_reopens_at_start() {
        let (mut state, _commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.settings.onboarding_complete = Some(false);
        state.settings_draft = state.settings.clone();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut panel = Panel::default();
        panel.reopen();
        panel.tick(&mut state);
        assert_eq!(state.settings_draft.onboarding_complete, Some(true));
        state.settings_draft.user_name = "Before".into();
        crate::test_ui::click(
            &ctx,
            &mut |ui| panel.show(ui.ctx(), &mut state),
            "Пропустить",
        );
        assert_eq!(panel.step, 1);
        crate::test_ui::replace_text(
            &ctx,
            &mut |ui| panel.show(ui.ctx(), &mut state),
            "Before",
            "After",
        );
        crate::test_ui::click(
            &ctx,
            &mut |ui| panel.show(ui.ctx(), &mut state),
            "Продолжить",
        );
        assert_eq!(panel.step, 2);
        assert_eq!(state.settings_draft.user_name, "After");
        crate::test_ui::click(
            &ctx,
            &mut |ui| panel.show(ui.ctx(), &mut state),
            "Начать работу",
        );
        assert!(!panel.open);
        assert_eq!(state.settings_draft.user_name, "After");
        panel.reopen();
        assert!(panel.open);
        assert_eq!(panel.step, 0);
        assert_eq!(state.settings_draft.user_name, "After");
    }
}

#[cfg(test)]
mod sound_regression_tests {
    #[test]
    fn tap_without_samples_and_silent_tap_are_honestly_ambiguous() {
        for (samples, silent) in [(0, true), (100, true)] {
            assert!(
                matches!(super::sound_status(samples, silent, true), super::Status::Warning(text) if text.contains("нет звука или доступа"))
            );
        }
        assert_eq!(super::sound_status(100, false, true), super::Status::Ready);
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;
    #[test]
    fn byte_progress_stalls_only_after_thirty_seconds_without_new_bytes() {
        let start = Instant::now();
        let mut transfer = Transfer::waiting(start);
        transfer.update("NOTETAKER_DOWNLOAD neutral 184000000 620000000", start);
        assert!(transfer.text("auto", 1).contains("184.0 из 620.0 МБ"));
        assert!(!transfer.stalled(start + Duration::from_secs(29)));
        assert!(transfer.stalled(start + Duration::from_secs(30)));
        transfer.update(
            "NOTETAKER_DOWNLOAD neutral 184000000 620000000",
            start + Duration::from_secs(31),
        );
        assert!(transfer.stalled(start + Duration::from_secs(31)));
        transfer.update(
            "NOTETAKER_DOWNLOAD neutral 185000000 620000000",
            start + Duration::from_secs(32),
        );
        assert!(!transfer.stalled(start + Duration::from_secs(32)));
        transfer.update(
            "NOTETAKER_DOWNLOAD neutral 620000000 620000000",
            start + Duration::from_secs(33),
        );
        assert!(!transfer.stalled(start + Duration::from_secs(100)));
    }
    #[test]
    fn retry_cancels_the_active_worker_before_starting_a_second_download() {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.download("auto".into());
        assert!(matches!(commands.try_recv().unwrap(), Command::Download(_)));
        state.retry_download();
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::CancelDownload
        ));
        auto_download(&mut state);
        assert!(commands.try_recv().is_err());
        state.apply(worker::Event::Downloaded(
            Err("Отменено".into()),
            Models::default(),
        ));
        assert!(state.quickstart.retry_at.is_none());
        auto_download(&mut state);
        assert!(matches!(commands.try_recv().unwrap(), Command::Download(_)));
    }
}

#[cfg(test)]
mod aggregate_progress_tests {
    use super::*;
    #[test]
    fn aggregate_does_not_reset_when_the_next_file_begins_and_eta_uses_real_bytes() {
        let at = Instant::now();
        let mut transfer = Transfer::waiting(at);
        for line in [
            "NOTETAKER_DOWNLOAD_PLAN gigaam-v3 100000000 0",
            "NOTETAKER_DOWNLOAD_PLAN silero-vad 100000000 0",
            "NOTETAKER_DOWNLOAD_PLAN_END 1",
        ] {
            transfer.update(line, at);
        }
        assert_eq!(transfer.fraction(), Some(0.0));
        assert_eq!(transfer.remaining_minutes(at), None);
        transfer.update(
            "NOTETAKER_DOWNLOAD gigaam-v3 100000000 100000000",
            at + Duration::from_secs(10),
        );
        assert_eq!(transfer.fraction(), Some(0.5));
        transfer.update(
            "NOTETAKER_DOWNLOAD silero-vad 0 100000000",
            at + Duration::from_secs(11),
        );
        assert_eq!(transfer.fraction(), Some(0.5));
        transfer.update(
            "NOTETAKER_DOWNLOAD silero-vad 50000000 100000000",
            at + Duration::from_secs(20),
        );
        assert_eq!(transfer.fraction(), Some(0.75));
        assert_eq!(
            transfer.remaining_minutes(at + Duration::from_secs(20)),
            Some(1)
        );
        assert!(transfer.text("auto", 1).contains("определение пауз"));
        assert!(!transfer.text("auto", 1).contains("silero-vad"));
    }
    #[test]
    fn unknown_plan_never_invents_a_percentage_and_cached_files_need_no_bandwidth() {
        let at = Instant::now();
        let mut transfer = Transfer::waiting(at);
        transfer.update("NOTETAKER_DOWNLOAD file 2000000 10000000", at);
        assert_eq!(transfer.fraction(), None);
        transfer.update("NOTETAKER_DOWNLOAD_PLAN cached 10000000 1", at);
        transfer.update("NOTETAKER_DOWNLOAD_PLAN_END 0", at);
        assert_eq!(transfer.fraction(), None);
        assert_eq!(
            transfer.remaining_minutes(at + Duration::from_secs(10)),
            None
        );
        transfer.update("NOTETAKER_DOWNLOAD_PLAN_END 1", at);
        assert_eq!(transfer.overall(), (12000000, 20000000));
        assert_eq!(transfer.network_bytes, 2000000);
        assert_eq!(
            Transfer::waiting(at).remaining_minutes(at + Duration::from_secs(5)),
            None
        );
    }
}
