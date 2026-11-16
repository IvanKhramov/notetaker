//! One recording per background task. The queue, not media, survives a restart.
use crate::{
    ktalk::Control,
    state::AppState,
    theme::{self, ButtonStyle},
};
use anyhow::{Context, Result};
use notetaker_core::Library;
use notetaker_llm::{
    OsCredentialStore,
    ktalk::{self, LearningPlan, LearningQueue, Settings},
};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

enum Completed {
    Plan(Result<LearningPlan>),
    Record {
        id: String,
        result: Result<notetaker_pipeline::HistoricalEnrollment>,
        cancelled: bool,
    },
}
#[derive(Default)]
pub struct LearningUi {
    root: Option<PathBuf>,
    pub queue: LearningQueue,
    pub plan: Option<LearningPlan>,
    receiver: Option<Receiver<Completed>>,
    pub control: Control,
    pub days: u32,
    pub status: String,
    offer: bool,
    prepare_after_consent: bool,
    paused: bool,
}
impl LearningUi {
    pub fn busy(&self) -> bool {
        self.receiver.is_some()
    }
    fn can_schedule(&self, blocked: bool) -> bool {
        self.queue.active && !blocked && !self.busy()
    }
    fn persist(&self) -> Result<()> {
        let root = self.root.as_ref().context("Не выбрана библиотека")?;
        Ok(Library::open(root)?.save_voice_learning_state(&self.queue)?)
    }
    fn spawn(&mut self, job: impl FnOnce(Control) -> Completed + Send + 'static) {
        self.control = Control::default();
        let control = self.control.clone();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        std::thread::spawn(move || {
            let message = job(control);
            let _ = tx.send(message);
            crate::worker::wake_ui();
        });
    }
    fn prepare(&mut self, settings: Settings, cutoff: chrono::DateTime<chrono::Utc>) {
        self.prepare_after_consent = false;
        self.plan = None;
        self.status = "Получаю список записей Толка…".into();
        self.spawn(move |control| {
            Completed::Plan((|| {
                let client = settings
                    .client(&OsCredentialStore)?
                    .with_cancellation(control.token.clone());
                ktalk::learning_plan(&client, cutoff)
            })())
        });
    }
    fn load(&mut self, root: &Path) -> Result<()> {
        self.control.cancel();
        self.root = Some(root.into());
        self.plan = None;
        self.queue = LearningQueue::default();
        let path = Library::open(root)?.voice_learning_path();
        self.queue = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("Некорректный журнал обучения голосов")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LearningQueue::default(),
            Err(e) => return Err(e.into()),
        };
        if self.queue.days > 0 {
            self.days = self.queue.days;
        }
        if self.days == 0 {
            self.days = 60;
        }
        Ok(())
    }
}
fn recording_job(
    root: PathBuf,
    settings: Settings,
    recording: ktalk::Recording,
    control: Control,
) -> Completed {
    let id = recording.id.clone();
    let result = (|| -> Result<notetaker_pipeline::HistoricalEnrollment> {
        let library = Library::open(&root)?;
        let client = settings
            .client(&OsCredentialStore)?
            .with_cancellation(control.token.clone());
        let owner = client.profile()?;
        let remote = client.transcript(&id)?;
        let evidence = ktalk::recording_voice_evidence(&remote, &recording, &owner)?;
        let source = ktalk::learning_source(&ktalk::learning_origin(&settings)?, &id);
        let mut worker = None;
        notetaker_pipeline::learn_recording(
            &library,
            &source,
            &evidence,
            |file| {
                client.download_recording(&recording, file)?;
                Ok(())
            },
            |wav, ranges| {
                control.check()?;
                if worker.is_none() {
                    let config = notetaker_pipeline::AsrConfig::new("auto")?;
                    let mut process = crate::worker::background_process(&config.binary);
                    process.env(
                        "NOTETAKER_MODELS_DIR",
                        crate::config::Settings::load()?.models_path(),
                    );
                    let handle = notetaker_asr_proto::CancellationHandle::default();
                    control.attach(handle.clone());
                    worker = Some(notetaker_asr_proto::AsrClient::spawn_command_cancellable(
                        process,
                        Duration::from_secs(300),
                        handle,
                    )?);
                }
                Ok(worker
                    .as_mut()
                    .context("ASR worker не запущен")?
                    .embed(wav, ranges)?)
            },
            || control.token.is_cancelled(),
        )
    })();
    Completed::Record {
        id,
        result,
        cancelled: control.token.is_cancelled(),
    }
}
/// Called before the regular sync scheduler. No ASR task starts while recording.
pub fn poll(state: &mut AppState) -> bool {
    let learning = &mut state.ktalk.learning;
    if (!state.ready || !state.ktalk.saved || state.ktalk.settings.is_none()) && !learning.busy() {
        return false;
    }
    let changed_root = learning
        .root
        .as_deref()
        .is_some_and(|root| root != state.settings.library);
    if changed_root {
        learning.control.cancel();
        learning.queue.active = false;
    }
    if learning.root.as_deref() != Some(&state.settings.library)
        && !learning.busy()
        && let Err(error) = learning.load(&state.settings.library)
    {
        learning.status = error.to_string();
        learning.queue.active = false;
    }
    let message = match learning.receiver.as_ref().map(|rx| rx.try_recv()) {
        Some(Ok(message)) => Some(message),
        Some(Err(mpsc::TryRecvError::Disconnected)) => {
            learning.receiver = None;
            learning.queue.active = false;
            learning.status = "Фоновое обучение завершилось без результата".into();
            None
        }
        _ => None,
    };
    if let Some(message) = message {
        learning.receiver = None;
        match message {
            Completed::Plan(result) => match result {
                Ok(plan) => {
                    learning.status = format!(
                        "Доступно записей: {} · оценка загрузки: до {} МБ",
                        plan.recordings.len(),
                        plan.estimated_bytes / 1_000_000
                    );
                    learning.plan = Some(plan);
                }
                Err(error) => {
                    learning.status = error.to_string();
                    if !learning.control.token.is_cancelled() {
                        learning.queue.active = false;
                    }
                }
            },
            Completed::Record {
                id,
                result,
                cancelled,
            } => {
                if !cancelled && !changed_root {
                    match result {
                        Ok(report) => {
                            learning
                                .queue
                                .finish_recording(&id, report.people, report.no_speech);
                            learning.status.clear();
                            if !learning.queue.active {
                                learning.plan = None;
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            learning.queue.remaining.retain(|next| next != &id);
                            learning.queue.attempted += 1;
                            if error.is::<ktalk::NetworkError>() {
                                learning.queue.network_errors += 1;
                            } else {
                                learning.queue.other_errors += 1;
                            }
                            learning.status = message;
                            if learning.queue.remaining.is_empty() {
                                learning.queue.active = false;
                            }
                        }
                    }
                    if let Err(error) = learning.persist() {
                        learning.status = error.to_string();
                        learning.queue.active = false;
                    }
                    match Library::open(&state.settings.library).and_then(|lib| lib.read_voices()) {
                        Ok(bank) => state.voices = bank,
                        Err(error) => {
                            learning.status = error.to_string();
                            learning.queue.active = false;
                        }
                    }
                }
            }
        }
    }
    let hard_blocked = state.exit_requested
        || !state.settings.voice_library
        || !state.settings_draft.voice_library;
    let blocked = state.quickstart.models.diarization != crate::quickstart::Status::Ready
        || state.recording.busy()
        || hard_blocked;
    if blocked {
        if learning.busy() && (hard_blocked || learning.plan.is_some()) {
            learning.control.cancel();
        }
        learning.paused = learning.queue.active;
        return learning.busy();
    }
    learning.paused = false;
    if learning.prepare_after_consent
        && !learning.busy()
        && !state.settings_pending
        && let Some(settings) = state.ktalk.settings.clone()
    {
        learning.prepare(
            settings,
            chrono::Utc::now() - chrono::Duration::days(i64::from(learning.days)),
        );
        return true;
    }
    if learning.busy() {
        return true;
    }
    if !learning.can_schedule(false) || !state.ready || state.settings_pending || state.ktalk.busy {
        return false;
    }
    let Some(settings) = state.ktalk.settings.clone() else {
        return false;
    };
    let origin = match ktalk::learning_origin(&settings) {
        Ok(origin) => origin,
        Err(error) => {
            learning.status = error.to_string();
            learning.queue.active = false;
            return false;
        }
    };
    if learning.queue.origin != origin {
        learning.status = "Пространство Толка изменилось; запустите обучение заново".into();
        learning.queue.active = false;
        return false;
    }
    if learning.plan.is_none() {
        if let Some(cutoff) = learning.queue.cutoff {
            learning.prepare(settings, cutoff);
        }
        return true;
    }
    if let Some(id) = learning.queue.remaining.first().cloned() {
        let recording = learning
            .plan
            .as_ref()
            .and_then(|p| p.recordings.iter().find(|r| r.id == id))
            .cloned();
        if let Some(recording) = recording {
            let root = state.settings.library.clone();
            learning.status = format!(
                "Учу голоса: запись {} из {} · выучено людей: {}",
                learning.queue.attempted + 1,
                learning.queue.total,
                learning.queue.people.len()
            );
            learning.spawn(move |control| recording_job(root, settings, recording, control));
        } else {
            learning.status = "Запись больше недоступна в Толке".into();
            learning.queue.remaining.remove(0);
            learning.queue.attempted += 1;
            learning.queue.other_errors += 1;
            if learning.queue.remaining.is_empty() {
                learning.queue.active = false;
            }
            if let Err(error) = learning.persist() {
                learning.status = error.to_string();
                learning.queue.active = false;
            }
            crate::worker::wake_ui();
        }
    } else {
        learning.queue.active = false;
    }
    true
}
pub fn settings(ui: &mut eframe::egui::Ui, state: &mut AppState) {
    theme::settings_group(ui, "Обучение голосов", |ui| {
        ui.label("Записи скачиваются по одной во временный файл на этом компьютере и удаляются после обработки. Остаются только локальные отпечатки голосов. Ничего не отправляется.");
        let learning = &mut state.ktalk.learning;
        if learning.days == 0 {
            learning.days = 60;
        }
        let previous_days = learning.days;
        ui.add_enabled_ui(!learning.queue.active && !learning.busy(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Период:");
                for days in [30, 60, 180] {
                    ui.selectable_value(&mut learning.days, days, format!("{days} дней"));
                }
            })
        });
        if learning.days != previous_days {
            learning.plan = None;
            learning.status.clear();
        }
        if ui
            .add_enabled(
                state.ktalk.saved && !learning.busy() && !learning.queue.active,
                theme::styled_button(
                    theme::Palette::of(ui),
                    "Выучить голоса по записям Толка",
                    ButtonStyle::Secondary,
                ),
            )
            .clicked()
        {
            if !state.settings.voice_library {
                learning.offer = true;
            } else if let Some(settings) = state.ktalk.settings.clone() {
                learning.prepare(
                    settings,
                    chrono::Utc::now() - chrono::Duration::days(i64::from(learning.days)),
                );
            }
        }
        if learning.offer
            && !state.settings.voice_library
            && let Some(enable) = crate::ktalk::voice_consent(ui)
        {
            learning.offer = false;
            state.ktalk.voice_offer_seen = true;
            if let Err(error) = crate::ktalk::mark_learning_offer(&state.settings.library) {
                learning.status = error.to_string();
            }
            if enable {
                state.ktalk.learning.prepare_after_consent = true;
                state.settings_draft.voice_library = true;
                state.save_preferences();
            }
        }
        let learning = &mut state.ktalk.learning;
        if let Some(plan) = &learning.plan
            && !learning.queue.active
            && !learning.busy()
        {
            ui.weak("Оценка для 512 кбит/с; фактический размер зависит от записи, не больше 300 МБ на файл. Уже изученные пары запись/человек пропускаются.");
            if ui
                .add_enabled(
                    state.settings.voice_library
                        && !plan.recordings.is_empty()
                        && !state.settings_pending
                        && state.quickstart.models.diarization == crate::quickstart::Status::Ready,
                    theme::styled_button(theme::Palette::of(ui), "Начать", ButtonStyle::Primary),
                )
                .clicked()
                && let Some(settings) = &state.ktalk.settings
            {
                match ktalk::learning_origin(settings) {
                    Ok(origin) if origin == plan.origin => {
                        let processed = if learning.queue.origin == origin {
                            std::mem::take(&mut learning.queue.processed)
                        } else {
                            Default::default()
                        };
                        learning.queue = LearningQueue::begin(origin, plan);
                        learning.queue.processed = processed;
                        learning.queue.days = learning.days;
                        if let Err(error) = learning.persist() {
                            learning.status = error.to_string();
                            learning.queue.active = false;
                        }
                    }
                    Ok(_) => {
                        learning.status =
                            "Пространство изменилось; получите список записей заново".into()
                    }
                    Err(error) => learning.status = error.to_string(),
                }
            }
        }
        if learning.queue.active || learning.busy() {
            if learning.paused {
                ui.weak("Обучение приостановлено: ASR занят или библиотека выключена");
            } else {
                ui.spinner();
            }
            if theme::button(ui, "Остановить", ButtonStyle::Ghost).clicked() {
                learning.queue.active = false;
                learning.control.cancel();
                if let Err(error) = learning.persist() {
                    learning.status = error.to_string();
                }
            }
        }
        if !learning.queue.active && learning.queue.total > 0 {
            ui.label(format!("Выучено людей: {}, из {} записей; пропущено: {} (нет чистой речи), {} (ошибка сети), {} (другие ошибки)",learning.queue.people.len(),learning.queue.attempted,learning.queue.no_speech,learning.queue.network_errors,learning.queue.other_errors));
        }
        if state.quickstart.models.diarization != crate::quickstart::Status::Ready {
            ui.weak("Для обучения нужны модели голосов. Скачайте их в настройках распознавания.");
        }
        if !learning.status.is_empty() {
            ui.weak(&learning.status);
        }
    });
}

pub fn voice_settings(
    ui: &mut eframe::egui::Ui,
    settings: &mut crate::config::Settings,
    bank: &notetaker_core::VoiceLibrary,
) {
    ui.add_enabled_ui(settings.voice_library, |ui| {
        theme::toggle(
            ui,
            &mut settings.automatic_voice_names,
            "Подставлять имя по голосу без вопроса",
        );
        ui.weak("При уверенном совпадении. Ошибку можно снять кнопкой «Это не он».");
    });
    let (wrong, total) = bank.recognition_counts();
    ui.weak(format!(
        "Толк поправил распознавание по голосу: {wrong} раз из {total}"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_resumes_after_restart_and_pause_does_not_consume_a_recording() {
        let (mut state, _, _) = crate::test_ui::state();
        let root = tempfile::tempdir().unwrap();
        state.settings.library = root.path().into();
        state.settings.voice_library = true;
        state.settings_draft = state.settings.clone();
        state.ready = true;
        state.quickstart.models.diarization = crate::quickstart::Status::Ready;
        state.ktalk.saved = true;
        state.ktalk.settings = Some(Settings {
            base_url: "https://neutral.ktalk.ru".into(),
            credential_id: "work".into(),
            api_key_env: None,
            extra: Default::default(),
        });
        state.ktalk.learning.load(root.path()).unwrap();
        state.ktalk.learning.queue = LearningQueue {
            origin: "https://neutral.ktalk.ru".into(),
            remaining: vec!["one".into()],
            total: 1,
            active: true,
            cutoff: Some(chrono::Utc::now()),
            days: 60,
            ..Default::default()
        };
        state.ktalk.learning.persist().unwrap();
        let mut restarted = LearningUi::default();
        restarted.load(root.path()).unwrap();
        assert!(restarted.queue.active);
        assert_eq!(restarted.queue.remaining, vec!["one"]);
        assert_eq!(restarted.days, 60);
        let (tx, rx) = mpsc::channel();
        state.ktalk.learning.receiver = Some(rx);
        // A loaded plan distinguishes a media/ASR task from metadata planning.
        state.ktalk.learning.plan = Some(LearningPlan {
            origin: "https://neutral.ktalk.ru".into(),
            recordings: vec![],
            estimated_bytes: 0,
            cutoff: chrono::Utc::now(),
        });
        state.recording.phase = crate::state::Phase::Recording;
        assert!(poll(&mut state));
        assert!(state.ktalk.learning.control.token.is_cancelled());
        assert!(state.ktalk.learning.paused);
        tx.send(Completed::Record {
            id: "one".into(),
            result: Err(anyhow::anyhow!("cancelled")),
            cancelled: true,
        })
        .unwrap();
        poll(&mut state);
        assert_eq!(state.ktalk.learning.queue.remaining, vec!["one"]);
        assert_eq!(state.ktalk.learning.queue.attempted, 0);
        assert!(!state.ktalk.learning.can_schedule(true));
        assert!(state.ktalk.learning.can_schedule(false));
        state.ktalk.learning.queue.active = false;
        state.ktalk.learning.persist().unwrap();
        restarted.load(root.path()).unwrap();
        assert!(!restarted.queue.active);
    }
    #[test]
    fn consent_and_voice_controls_are_visible_without_starting_network() {
        let ctx = eframe::egui::Context::default();
        theme::install(&ctx);
        let (mut state, _, _) = crate::test_ui::state();
        state.ktalk.learning.offer = true;
        let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            settings(ui, &mut state);
        });
        crate::test_ui::text_rect(&output.shapes, "Включить");
        output.drop_without_applying_deltas();
        state.ktalk.learning.plan = Some(LearningPlan {
            origin: "https://neutral.ktalk.ru".into(),
            recordings: vec![],
            estimated_bytes: 0,
            cutoff: chrono::Utc::now(),
        });
        crate::test_ui::click(&ctx, &mut |ui| settings(ui, &mut state), "30 дней");
        assert_eq!(state.ktalk.learning.days, 30);
        assert!(state.ktalk.learning.plan.is_none());
        state.settings_draft.voice_library = true;
        let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            voice_settings(ui, &mut state.settings_draft, &state.voices)
        });
        crate::test_ui::text_rect(&output.shapes, "Подставлять имя по голосу без вопроса");
        output.drop_without_applying_deltas();
    }
}
