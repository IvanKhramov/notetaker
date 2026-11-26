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
    pub fn set(&mut self, model: &str, status: Status) {
        match model {
            "gigaam-v3" => self.gigaam = status,
            "parakeet-v3" => self.parakeet = status,
            "auto" => self.auto = status,
            "diarization" => self.diarization = status,
            "semantic" => self.semantic = status,
            _ => {}
        }
    }
    pub fn speech_ready(&self, model: &str) -> bool {
        self.get(model) == &Status::Ready
    }
    pub fn missing<'a>(&self, model: &'a str) -> Option<&'a str> {
        (!self.speech_ready(model)).then_some(model)
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
    installed: std::collections::BTreeSet<String>,
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
            installed: Default::default(),
        }
    }
    pub fn update(&mut self, line: &str, now: Instant) {
        if let Some(file) = line.strip_prefix("NOTETAKER_MODEL_INSTALLED ") {
            self.installed.insert(file.into());
            return;
        }
        if let Some(known) = line.strip_prefix("NOTETAKER_DOWNLOAD_PLAN_END ") {
            self.plan_complete = known == "1";
            return;
        }
        if let Some(line) = line.strip_prefix("NOTETAKER_DOWNLOAD_PLAN ") {
            let fields: Vec<_> = line.split_whitespace().collect();
            if let [file, total, installed] = fields.as_slice()
                && let Ok(total) = total.parse::<u64>()
            {
                if *installed == "1" {
                    self.installed.insert((*file).into());
                }
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
    pub fn text(&self, model: &str, _phase: usize) -> String {
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
        let prefix = if optional(model) {
            "Дополнительно"
        } else {
            "Подготовка к записи"
        };
        let count = if self.plan_complete && !optional(model) {
            format!(
                " · готово файлов {} из {}",
                self.installed.len(),
                self.files.len()
            )
        } else {
            String::new()
        };
        format!("{prefix}: {label} · {progress}{count}{eta}")
    }
}
#[derive(Default)]
pub struct Runtime {
    pub cancel: notetaker_llm::Cancellation,
    pub models: Models,
    pub download_model: Option<String>,
    pub transfer: Option<Transfer>,
    pub retry_requested: bool,
    pub download_error: Option<String>,
    pub optional_errors: std::collections::BTreeMap<String, String>,
    pub probe_busy: bool,
    pub retry_at: Option<Instant>,
    checked_at: Option<Instant>,
}

pub fn optional(model: &str) -> bool {
    matches!(model, "diarization" | "semantic")
}
fn next_download(state: &AppState) -> Option<String> {
    let models = &state.quickstart.models;
    if !models.speech_ready(&state.settings.model) {
        return Some(state.settings.model.clone());
    }
    if (state.settings.diarize || state.settings.voice_library)
        && models.diarization != Status::Ready
        && !state.quickstart.optional_errors.contains_key("diarization")
    {
        return Some("diarization".into());
    }
    if state.settings.semantic_search
        && state.llm.embeddings.provider.is_empty()
        && models.semantic != Status::Ready
        && !state.quickstart.optional_errors.contains_key("semantic")
    {
        return Some("semantic".into());
    }
    None
}
pub fn auto_download(state: &mut AppState) {
    if !state.ready
        || state.recording.busy()
        || state.settings_pending
        || state.settings_autosave.pending
        || state.exit_requested
    {
        return;
    }
    if state.downloading {
        if state
            .quickstart
            .download_model
            .as_deref()
            .is_some_and(optional)
            && (!state.quickstart.models.speech_ready(&state.settings.model)
                || state.quickstart.download_model.as_deref() == Some("semantic")
                    && !state.settings.semantic_search)
            && !state.quickstart.retry_requested
        {
            state.retry_download();
        }
        return;
    }
    if let Some(model) = next_download(state) {
        if state.quickstart.download_model.as_ref() == Some(&model)
            && state
                .quickstart
                .retry_at
                .is_some_and(|at| Instant::now() < at)
        {
            return;
        }
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

pub fn check_speech(model: &str) -> Status {
    let result = worker::asr_binary().and_then(|binary| {
        Ok(notetaker_runtime::process::spawn(
            worker::background_process(&binary)
                .args(["check-model", model])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
            notetaker_runtime::process::Kind::Background,
        )?
        .wait()?)
    });
    match result {
        Ok(status) if status.success() => Status::Ready,
        Ok(_) => Status::Missing,
        Err(error) => Status::Error(format!("{error:#}")),
    }
}
pub fn check_models() -> Models {
    Models {
        gigaam: check_speech("gigaam-v3"),
        parakeet: check_speech("parakeet-v3"),
        auto: check_speech("auto"),
        diarization: check_speech("diarization"),
        semantic: if notetaker_core::embedding::installed()
            && notetaker_core::embedding::worker_path().is_file()
        {
            Status::Ready
        } else {
            Status::Missing
        },
    }
}
/// Shared phase and per-file byte progress for welcome, library and storage.
pub fn model_status(ui: &mut egui::Ui, state: &mut AppState) {
    let required_ready = state.quickstart.models.speech_ready(&state.settings.model);
    let required_download = !required_ready
        && state.downloading
        && state
            .quickstart
            .download_model
            .as_deref()
            .is_some_and(|id| !optional(id));
    if required_download {
        transfer_status(ui, state);
    } else if !required_ready {
        if let Some(error) = state.quickstart.download_error.clone() {
            // Retry is automatic. The worker's own message stays one hover away.
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Не удалось скачать модель распознавания. Повторим автоматически через минуту.",
                    )
                    .color(Palette::of(ui).red),
                )
                .wrap(),
            )
            .on_hover_text(error);
            if theme::button(ui, "Повторить сейчас", theme::ButtonStyle::Ghost).clicked()
            {
                state.retry_download();
            }
        } else {
            ui.weak("Подготовка к записи: нужна модель распознавания речи и определение пауз.");
        }
    } else {
        ui.weak("Распознавание готово. Запись доступна.");
    }
    if required_ready
        && state.downloading
        && state
            .quickstart
            .download_model
            .as_deref()
            .is_some_and(optional)
    {
        transfer_status(ui, state);
    }
}
fn transfer_status(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal_wrapped(|ui| {
        theme::spinner(ui);
        let text = state
            .quickstart
            .transfer
            .as_ref()
            .map(|t| {
                t.text(
                    state.quickstart.download_model.as_deref().unwrap_or("auto"),
                    0,
                )
            })
            .unwrap_or_else(|| "Подготовка к записи: подключение…".into());
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
        if transfer.stalled(Instant::now()) {
            ui.weak("Загрузка не идёт. Проверьте сеть.");
            if theme::button(ui, "Повторить", theme::ButtonStyle::Ghost).clicked() {
                state.retry_download();
            }
        }
    }
}
pub fn optional_switch(ui: &mut egui::Ui, state: &mut AppState) {
    let mut enabled = state.settings_draft.semantic_search;
    if theme::switch(ui, &mut enabled, "Локальный поиск по смыслу").changed()
    {
        state.settings_draft.semantic_search = enabled;
        state.save_preferences();
    }
}
pub fn optional_download_status(ui: &mut egui::Ui, state: &mut AppState) {
    if state.downloading
        && state
            .quickstart
            .download_model
            .as_deref()
            .is_some_and(optional)
    {
        transfer_status(ui, state);
    }
    for model in ["diarization", "semantic"] {
        if let Some(error) = state.quickstart.optional_errors.get(model).cloned() {
            ui.horizontal(|ui| {
                if theme::button(ui, "Повторить", theme::ButtonStyle::Ghost).clicked() {
                    state.quickstart.optional_errors.remove(model);
                    state.quickstart.retry_at = None;
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{}: {error}",
                            if model == "semantic" {
                                "Поиск по смыслу"
                            } else {
                                "Разделение собеседников"
                            }
                        ))
                        .small()
                        .color(Palette::of(ui).muted),
                    )
                    .truncate(),
                )
                .on_hover_text(error);
            });
        }
    }
}

#[derive(Default)]
pub struct Panel {
    pub open: bool,
    startup_checked: bool,
    probe: Option<Receiver<ProbeEvent>>,
    probe_status: Option<[Status; 2]>,
}
impl Panel {
    pub fn reopen(&mut self) {
        self.open = true;
    }
    pub fn tick(&mut self, state: &mut AppState) {
        if state.ready && !self.startup_checked {
            self.startup_checked = true;
            self.open = state.settings.needs_onboarding();
            if self.open {
                // Mark the first presentation, so even dismissing the window is
                // a one-time experience. Explicit reruns never reset this flag.
                state.settings_draft.onboarding_complete = Some(true);
                if state.settings_draft.user_name.trim().is_empty()
                    && let Some(name) = account_name()
                {
                    state.settings_draft.user_name = name;
                }
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
        let alpha = theme::appearance(ctx, egui::Id::unique("quickstart-fade"));
        let modal=egui::Modal::new(egui::Id::unique("quickstart"))
            .backdrop_color(egui::Color32::from_black_alpha(((if ctx.global_style().visuals.dark_mode {140.0} else {89.0})*alpha) as u8))
            .frame(egui::Frame::window(&ctx.global_style()))
            .show(ctx, |ui| {
                ui.multiply_opacity(alpha);
                ui.set_width((ctx.content_rect().width() - 64.0).min(520.0));
                ui.style_mut().text_styles.insert(egui::TextStyle::Heading,theme::semibold(20.0));
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, theme::icons::X, "Закрыть").clicked() { open = false; }
                    });
                });
                ui.add_space(8.0);
                // Reserve the bar's lane so it never covers the right edge of fields.
                ui.style_mut().spacing.scroll.floating_allocated_width = theme::SCROLL_LANE;
                egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 320.0).max(80.0))
                    .show(ui, |ui| {
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

                });
                ui.add_space(12.0);
                model_status(ui, state);
                ui.small(if state.quickstart.models.speech_ready(&state.settings.model) {
                    "Распознавание готово. Можно начинать запись."
                } else { "Запись станет доступна после подготовки распознавания." });
                ui.small("Календарь и AI для конспектов можно подключить позже в настройках.");
                ui.add_space(16.0);
                if let Some(error) = &state.settings_autosave.error { ui.colored_label(Palette::of(ui).red, error); }
                theme::footer(ui, |ui| {
                    let check_audio = self.probe_status.is_none() && cfg!(any(windows, target_os = "macos"));
                    let label = if check_audio { "Проверить звук" } else { "Начать работу" };
                    if ui.add_enabled(!state.quickstart.probe_busy, theme::styled_button(Palette::of(ui), label, theme::ButtonStyle::Primary)).clicked() {
                        if check_audio { self.start_probe(state); } else { open = false; }
                    }
                    if check_audio && theme::button(ui, "Пропустить", crate::theme::ButtonStyle::Ghost).clicked() { open = false; }
                });
            });
        if theme::modal_should_close(&modal) {
            open = false;
        }
        if !open {
            state.save_preferences();
        }
        self.open = open;
    }
}

/// The full name of the macOS account, used as the default speaker name.
fn account_name() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = std::process::Command::new("id").arg("-F").output().ok()?;
    let name = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !name.is_empty()).then_some(name)
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
    fn quickstart_is_one_screen_that_keeps_the_name_and_can_be_reopened() {
        let (mut state, _commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.settings.onboarding_complete = Some(false);
        state.settings_draft = state.settings.clone();
        state.settings_draft.user_name = "Before".into();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut panel = Panel::default();
        panel.tick(&mut state);
        assert!(panel.open);
        assert_eq!(state.settings_draft.onboarding_complete, Some(true));
        // An existing name is never replaced by the account name.
        assert_eq!(state.settings_draft.user_name, "Before");
        crate::test_ui::click(
            &ctx,
            &mut |ui| panel.show(ui.ctx(), &mut state),
            "Начать работу",
        );
        assert!(!panel.open);
        assert_eq!(state.settings_draft.user_name, "Before");
        panel.reopen();
        assert!(panel.open);
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
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Download(_, _)
        ));
        state.retry_download();
        assert!(state.quickstart.cancel.is_cancelled());
        assert!(commands.try_recv().is_err());
        auto_download(&mut state);
        assert!(commands.try_recv().is_err());
        state.apply(worker::Event::Downloaded(
            Err("Отменено".into()),
            Models::default(),
        ));
        assert!(state.quickstart.retry_at.is_none());
        auto_download(&mut state);
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Download(_, _)
        ));
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

#[cfg(test)]
mod required_models_tests {
    use super::*;
    #[test]
    fn cancelled_optional_download_preserves_recording_readiness() {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state
            .quickstart
            .models
            .set(&state.settings.model.clone(), Status::Ready);
        state.download("semantic".into());
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Download(_, _)
        ));
        state.retry_download();
        state.apply(worker::Event::Downloaded(
            Err("Отменено".into()),
            Models::default(),
        ));
        assert!(state.quickstart.models.speech_ready(&state.settings.model));
        assert!(state.can_prepare_recording());
    }
    #[test]
    fn recording_starts_while_optional_models_are_absent_or_downloading() {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.settings.diarize = true;
        state.settings.voice_library = true;
        state
            .quickstart
            .models
            .set(&state.record_model.clone(), Status::Ready);
        state.downloading = true;
        state.quickstart.download_model = Some("diarization".into());
        state.start_recording();
        let Command::Record(request) = commands.try_recv().unwrap() else {
            panic!("recording was not requested");
        };
        assert!(!request.settings.diarize);
        assert!(!request.settings.voice_library);
        assert!(state.settings.diarize);
        assert!(state.settings.voice_library);
        assert!(
            state
                .notices
                .iter()
                .any(|(_, text)| text.contains("без разделения"))
        );
    }
    #[test]
    fn only_speech_and_vad_readiness_gate_recording_not_optional_downloads() {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state
            .quickstart
            .models
            .set(&state.settings.model.clone(), Status::Ready);
        assert!(state.can_prepare_recording());
        state.downloading = true;
        state.quickstart.download_model = Some("semantic".into());
        assert!(state.can_prepare_recording());
        state.settings.diarize = false;
        state.settings.voice_library = false;
        state.downloading = false;
        auto_download(&mut state);
        assert!(commands.try_recv().is_err());
        assert!(!state.settings.semantic_search);
        state.settings.semantic_search = true;
        auto_download(&mut state);
        assert!(matches!(commands.try_recv().unwrap(),Command::Download(id, _) if id=="semantic"));
        state.apply(worker::Event::Downloaded(
            Err("network unavailable".into()),
            state.quickstart.models.clone(),
        ));
        assert!(state.can_prepare_recording());
        assert!(state.quickstart.download_error.is_none());
        assert!(state.quickstart.optional_errors.contains_key("semantic"));
    }
    #[test]
    fn installed_file_markers_update_only_the_required_counter() {
        let at = Instant::now();
        let mut transfer = Transfer::waiting(at);
        for line in [
            "NOTETAKER_DOWNLOAD_PLAN gigaam-v3 100 0",
            "NOTETAKER_DOWNLOAD_PLAN silero-vad 10 0",
            "NOTETAKER_DOWNLOAD_PLAN_END 1",
            "NOTETAKER_MODEL_INSTALLED gigaam-v3",
        ] {
            transfer.update(line, at);
        }
        let text = transfer.text("gigaam-v3", 0);
        assert!(text.contains("готово файлов 1 из 2"));
        assert!(!text.contains("из 3"));
        assert!(transfer.text("semantic", 0).starts_with("Дополнительно:"));
    }
    #[test]
    fn mandatory_speech_preempts_an_optional_transfer() {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.settings.semantic_search = true;
        state.downloading = true;
        state.quickstart.download_model = Some("semantic".into());
        auto_download(&mut state);
        assert!(state.quickstart.cancel.is_cancelled());
        assert!(commands.try_recv().is_err());
        auto_download(&mut state);
        assert!(commands.try_recv().is_err());
    }
}
