//! Tolk work is serialized by the UI state and executed on a background worker.
use crate::worker::DocumentData;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use notetaker_core::{Library, Meeting, Revision, Transcript};
use notetaker_llm::{
    CredentialStore, OsCredentialStore,
    ktalk::{self, Plan, Recording, Settings},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct Control {
    pub token: notetaker_llm::Cancellation,
    asr: std::sync::Arc<std::sync::Mutex<Option<notetaker_asr_proto::CancellationHandle>>>,
}
impl Control {
    pub fn cancel(&self) {
        self.token.cancel();
        if let Some(handle) = self.asr.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            handle.cancel();
        }
    }
    fn check(&self) -> Result<()> {
        anyhow::ensure!(!self.token.is_cancelled(), "Операция Толка отменена");
        Ok(())
    }
    fn attach(&self, handle: notetaker_asr_proto::CancellationHandle) {
        if self.token.is_cancelled() {
            handle.cancel();
        }
        *self.asr.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncId {
    pub meeting: uuid::Uuid,
    pub revision: Revision,
}
#[derive(Clone, Copy)]
struct Moment {
    wall: DateTime<Utc>,
    monotonic: Instant,
}
impl Moment {
    fn now() -> Self {
        Self {
            wall: Utc::now(),
            monotonic: Instant::now(),
        }
    }
}
struct EnrollmentClock {
    started: Instant,
    applied: std::collections::HashMap<SyncId, Instant>,
}
impl Default for EnrollmentClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            applied: Default::default(),
        }
    }
}
impl EnrollmentClock {
    fn ready(&self, pending: &PendingEnrollment, now: Moment) -> bool {
        if let Some(applied) = self.applied.get(&pending.sync()) {
            now.monotonic.saturating_duration_since(*applied) >= Duration::from_secs(120)
        } else {
            now.monotonic.saturating_duration_since(self.started) >= Duration::from_secs(120)
                && now.wall >= pending.not_before
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEnrollment {
    pub meeting: uuid::Uuid,
    pub recording_id: String,
    pub revision: Revision,
    pub not_before: DateTime<Utc>,
}
impl PendingEnrollment {
    fn sync(&self) -> SyncId {
        SyncId {
            meeting: self.meeting,
            revision: self.revision,
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default)]
    pub recorded_here: bool,
    #[serde(default)]
    pub answered: u8,
    #[serde(default)]
    pub network_failures: u8,
    #[serde(default)]
    pub enrol_pending: bool,
    #[serde(default)]
    pub pending_enrollment: Option<PendingEnrollment>,
    #[serde(default)]
    pub recording_id: Option<String>,
    #[serde(default)]
    pub enrol_status: String,
    #[serde(default)]
    pub guest_ranges: BTreeMap<String, Vec<(u64, u64)>>,
    #[serde(default)]
    pub origin: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source_time: Option<DateTime<Utc>>,
    pub next: Option<DateTime<Utc>>,
    pub done: bool,
    pub reason: Option<String>,
    pub voice_offer_seen: bool,
}
#[derive(Clone)]
pub struct Task {
    pub meeting: Meeting,
    pub ended: DateTime<Utc>,
    pub progress: Progress,
}
impl Task {
    pub fn after(&mut self, outcome: &Outcome, now: DateTime<Utc>) {
        match outcome {
            Outcome::Refused(reason) => {
                self.progress.done = true;
                self.progress.reason = Some(reason.clone());
            }
            Outcome::Applied(applied) => {
                self.progress.done = true;
                self.progress.reason = None;
                self.progress.source_time = Some(applied.recording.created_date);
            }
            Outcome::Busy => self.progress.next = Some(now + chrono::Duration::seconds(15)),
            Outcome::Network(_) => self.network_retry(now),
            Outcome::Waiting => {
                self.progress.network_failures = 0;
                self.progress.answered += 1;
                if let Some(minutes) =
                    [3, 7, 20].get(self.progress.answered.saturating_sub(1) as usize)
                {
                    self.progress.next = Some(now + chrono::Duration::minutes(*minutes));
                } else {
                    self.progress.done = true;
                }
            }
            _ => {}
        }
    }
    fn network_retry(&mut self, now: DateTime<Utc>) {
        let minutes = [1, 5, 15]
            .get(self.progress.network_failures as usize)
            .copied()
            .unwrap_or(30);
        self.progress.network_failures = self.progress.network_failures.saturating_add(1);
        self.progress.next = Some(now + chrono::Duration::minutes(minutes));
    }
}
#[derive(Clone)]
pub struct Undo {
    pub sync: SyncId,
    pub meeting: Meeting,
    pub before: Transcript,
    pub after: Revision,
}
pub struct Applied {
    pub applied_at: Instant,
    pub document: Box<DocumentData>,
    pub undo: Undo,
    pub segments: usize,
    pub recording: Recording,
    pub voice_status: String,
}
pub enum Outcome {
    Preview(Box<Plan>),
    Choices {
        reason: String,
        recordings: Vec<Recording>,
    },
    Applied(Box<Applied>),
    Refused(String),
    Waiting,
    Network(String),
    Busy,
    Cancelled,
}
pub enum Job {
    SkipEnrollment(Task, String),
    Name {
        meeting: Meeting,
        expected: Transcript,
        edit: notetaker_core::SpeakerEdit,
    },
    Load(PathBuf),
    Save {
        settings: Settings,
        key: Option<String>,
    },
    Check(Settings),
    Sync {
        task: Task,
        settings: Settings,
        explicit: Option<String>,
        automatic: bool,
        voices: bool,
    },
    Apply {
        task: Task,
        plan: Box<Plan>,
        voices: bool,
    },
    Undo(Undo),
    Enroll(Task),
    Resume(Task, Settings),
    Seen(Task),
    Inspect(Meeting),
    Merge {
        root: PathBuf,
        from: String,
        into: notetaker_core::Person,
    },
}
pub enum Message {
    Named {
        path: PathBuf,
        result: Box<Result<notetaker_core::SpeakerEditResult, String>>,
        data: Option<Box<DocumentData>>,
        voice_status: String,
    },
    Loaded {
        settings: Option<Settings>,
        saved: bool,
        tasks: Vec<Task>,
        people: Vec<notetaker_core::Person>,
        voice_offer_seen: bool,
    },
    Saved(Settings),
    Checked(String),
    Synced {
        task: Task,
        outcome: Outcome,
        automatic: bool,
        journal_error: Option<String>,
    },
    Undone {
        document: Box<DocumentData>,
        progress: Progress,
    },
    Enrolled {
        path: PathBuf,
        status: String,
        task: Task,
    },
    Error(String),
    Seen,
    Inspected(Task),
    People(Vec<notetaker_core::Person>),
}
#[derive(Default)]
pub struct State {
    pub biometric: bool,
    pub cancel: Control,
    pub settings: Option<Settings>,
    pub saved: bool,
    pub busy: bool,
    pub tasks: BTreeMap<PathBuf, Task>,
    pub preview: Option<(Task, Outcome)>,
    pub preview_open: bool,
    pub status: String,
    pub voice_status: BTreeMap<PathBuf, String>,
    pub sources: BTreeMap<PathBuf, Recording>,
    pub successes: std::collections::VecDeque<(Undo, usize)>,
    pub people: Vec<notetaker_core::Person>,
    pub merge: Option<(String, notetaker_core::Person)>,
    pub inspected: std::collections::BTreeSet<PathBuf>,
    pub enroll_pending: Option<Task>,
    pub voice_offer_seen: bool,
    pub undo_pending: Option<Undo>,
    pub undo_visible: std::collections::HashSet<SyncId>,
    undo_closed: std::collections::HashSet<SyncId>,
    clock: EnrollmentClock,
    pub load_root: Option<PathBuf>,
    pub address: String,
    key: String,
    replacing: bool,
    automatic_draft: Option<bool>,
    settings_edited: bool,
    busy_path: Option<PathBuf>,
}
impl State {
    pub fn editing(&self, path: &Path) -> bool {
        self.busy_path.as_deref() == Some(path)
    }
    fn enrollment_ready(&self, task: &Task, now: Moment) -> bool {
        task.progress.enrol_pending
            && task
                .progress
                .pending_enrollment
                .as_ref()
                .is_some_and(|pending| {
                    self.clock.ready(pending, now)
                        && !self.undo_visible.contains(&pending.sync())
                        && !self
                            .successes
                            .iter()
                            .any(|(undo, _)| undo.sync == pending.sync())
                })
            && task.progress.next.is_none_or(|next| next <= now.wall)
    }
    pub fn next(&self, now: DateTime<Utc>, blocked: impl Fn(&Path) -> bool) -> Option<Task> {
        if self.busy || !self.saved || !self.settings.as_ref().is_some_and(Settings::auto_sync) {
            return None;
        }
        self.tasks
            .values()
            .find(|task| {
                task.progress.recorded_here
                    && !task.progress.done
                    && now <= task.ended + chrono::Duration::hours(24)
                    && !blocked(task.meeting.path())
                    && task.progress.next.is_none_or(|next| next <= now)
            })
            .cloned()
    }
}
fn journal(meeting: &Meeting) -> Result<PathBuf> {
    let library = Library::open(meeting.path().parent().context("Нет библиотеки")?)?;
    Ok(library
        .private_state_dir()
        .join(format!("ktalk/{}.json", meeting.read_meta()?.id)))
}
fn persist(task: &Task) -> Result<()> {
    let mut progress = task.progress.clone();
    // Only register_recording may mint provenance; stale manual tasks cannot erase it.
    progress.recorded_here = read_progress(&task.meeting)?.recorded_here;
    crate::config::atomic_write(&journal(&task.meeting)?, &serde_json::to_string(&progress)?)
}
fn offer_path(root: &Path) -> Result<PathBuf> {
    Ok(Library::open(root)?
        .private_state_dir()
        .join("ktalk/voice-offer-seen"))
}
fn mark_offer_seen(task: &Task) -> Result<()> {
    crate::config::atomic_write(
        &offer_path(task.meeting.path().parent().context("Нет библиотеки")?)?,
        "1",
    )
}
pub fn register_recording(meeting: &Meeting) -> Result<Task> {
    let ended = meeting
        .read_meta()?
        .ended_at
        .context("Запись ещё не завершена")?;
    let task = Task {
        meeting: meeting.clone(),
        ended,
        progress: Progress {
            recorded_here: true,
            origin: Some(Utc::now()),
            ..Default::default()
        },
    };
    crate::config::atomic_write(&journal(meeting)?, &serde_json::to_string(&task.progress)?)?;
    Ok(task)
}
pub(crate) fn recorded_here(meeting: &Meeting) -> Result<bool> {
    Ok(read_progress(meeting)?.recorded_here)
}
pub fn pickup(root: &Path, now: DateTime<Utc>) -> Result<Vec<Task>> {
    let library = Library::open(root)?;
    library.discard_voice_ledgers()?;
    let mut tasks = Vec::new();
    for meeting in library.list_meetings()? {
        let meta = meeting.read_meta()?;
        let mut progress = read_progress(&meeting)?;
        let ended = meta.ended_at.unwrap_or(meta.started_at);
        if progress.enrol_pending && progress.pending_enrollment.is_none() {
            progress.enrol_pending = false;
            persist(&Task {
                meeting: meeting.clone(),
                ended,
                progress: progress.clone(),
            })?;
        }
        if !progress.recorded_here && !progress.enrol_pending {
            continue;
        }
        if progress.enrol_pending {
            progress.done = true;
        } else if meta.ended_at.is_none()
            || ended > now
            || ended < now - chrono::Duration::hours(24)
        {
            continue;
        }
        if !meeting.read_transcript()?.ktalk_sources.is_empty() {
            progress.done = true;
        }
        tasks.push(Task {
            meeting,
            ended,
            progress,
        });
    }
    Ok(tasks)
}
fn read_progress(meeting: &Meeting) -> Result<Progress> {
    Ok(match std::fs::read(journal(meeting)?) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Progress::default(),
        Err(error) => return Err(error.into()),
    })
}
fn load(root: &Path, store: &dyn CredentialStore) -> Result<Message> {
    let tasks = pickup(root, Utc::now())?;
    let settings = Settings::from_path(&notetaker_llm::Config::path()?)?;
    let saved = settings.as_ref().is_some_and(|settings| {
        if let Some(env) = &settings.api_key_env {
            std::env::var(env).is_ok_and(|key| !key.is_empty())
        } else {
            store
                .read(&format!("ktalk/{}", settings.credential_id))
                .is_ok_and(|key| !key.is_empty())
        }
    });
    Ok(Message::Loaded {
        settings,
        saved,
        tasks,
        people: Library::open(root)?.people(false, "", "")?,
        voice_offer_seen: offer_path(root)?.is_file(),
    })
}
fn timed_out(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            )
        })
}
fn plan_for_task(
    task: &Task,
    settings: &Settings,
    explicit: Option<&str>,
    store: &dyn CredentialStore,
    control: &Control,
) -> Result<Plan> {
    control.check()?;
    let client = settings
        .client(store)?
        .with_cancellation(control.token.clone());
    let recording = ktalk::select_recording(&client, &task.meeting, explicit)?;
    let remote = client.transcript(&recording.id)?;
    let owner = if recorded_here(&task.meeting)? {
        Some(client.profile()?)
    } else {
        None
    };
    ktalk::plan(
        &task.meeting.read_transcript()?,
        task.meeting.read_meta()?.started_at,
        recording,
        &remote,
        owner.as_ref(),
    )
    .map_err(|error| ktalk::InvalidTranscript(error.to_string()).into())
}
fn pending_matches(task: &Task) -> Result<bool> {
    let fresh = read_progress(&task.meeting)?;
    let Some(pending) = &task.progress.pending_enrollment else {
        return Ok(false);
    };
    let loaded = task.meeting.load_transcript()?;
    Ok(fresh.enrol_pending
        && fresh.pending_enrollment.as_ref() == Some(pending)
        && pending.meeting == task.meeting.read_meta()?.id
        && loaded.revision == pending.revision
        && loaded
            .value
            .ktalk_sources
            .values()
            .any(|id| id == &pending.recording_id))
}
fn enroll_plan(task: &Task, plan: &Plan, control: &Control) -> Result<String> {
    control.check()?;
    anyhow::ensure!(
        pending_matches(task)?,
        "Транскрипт изменился после сверки; обучение пропущено"
    );
    anyhow::ensure!(
        task.meeting.mic_audio_path().is_file() || task.meeting.system_audio_path().is_file(),
        "Аудио не хранится: голоса не запомнены"
    );
    let transcript = task.meeting.read_transcript()?;
    if !plan.voices.iter().any(|voice| voice.registered)
        && (!recorded_here(&task.meeting)? || !transcript.speaker_names.contains_key("me"))
    {
        return Ok("Гости автоматически не запоминаются".into());
    }
    let config = notetaker_pipeline::AsrConfig::new("auto")?;
    let mut process = crate::worker::background_process(&config.binary);
    process.env(
        "NOTETAKER_MODELS_DIR",
        crate::config::Settings::load()?.models_path(),
    );
    let handle = notetaker_asr_proto::CancellationHandle::default();
    control.attach(handle.clone());
    let mut client = notetaker_asr_proto::AsrClient::spawn_command_cancellable(
        process,
        Duration::from_secs(300),
        handle,
    )?;
    let report = notetaker_pipeline::remember_named_voices(
        &task.meeting,
        &transcript,
        &plan.voices,
        recorded_here(&task.meeting)?,
        task.progress
            .pending_enrollment
            .as_ref()
            .context("Нет запроса обучения")?
            .revision,
        |wav, ranges| {
            control.check()?;
            let result = client.embed(wav, ranges)?;
            control.check()?;
            anyhow::ensure!(
                pending_matches(task)?,
                "Транскрипт изменился после сверки; обучение пропущено"
            );
            Ok(result)
        },
        || control.token.is_cancelled(),
    )?;
    Ok(format!(
        "Запомнено голосов: {}{}",
        report.count,
        if report.skipped.is_empty() {
            String::new()
        } else {
            format!(". {}", report.skipped.join("; "))
        }
    ))
}
fn apply_names(
    task: &mut Task,
    plan: &Plan,
    voices: bool,
    control: &Control,
) -> Result<(ktalk::AppliedTranscript, usize)> {
    control.check()?;
    anyhow::ensure!(
        !plan.names_owner || recorded_here(&task.meeting)?,
        "Запись не подтверждена этой установкой; повторите сверку"
    );
    let meeting = task.meeting.clone();
    let library = Library::open(meeting.path().parent().context("Нет библиотеки")?)?;
    let applied = ktalk::apply(&library, &meeting, plan)?;
    let segments = applied
        .after
        .value
        .segments
        .iter()
        .zip(&applied.before.segments)
        .filter(|(a, b)| {
            a.speaker != b.speaker
                || applied.after.value.speaker_name(&a.speaker.0)
                    != applied.before.speaker_name(&b.speaker.0)
        })
        .count();
    task.progress.done = true;
    task.progress.recording_id = Some(plan.recording.id.clone());
    task.progress.source_time = Some(plan.recording.created_date);
    task.progress.enrol_pending = voices;
    task.progress.pending_enrollment = Some(PendingEnrollment {
        meeting: meeting.read_meta()?.id,
        recording_id: plan.recording.id.clone(),
        revision: applied.after.revision,
        not_before: Utc::now() + chrono::Duration::minutes(2),
    });
    task.progress.next = None;
    task.progress.guest_ranges = plan
        .voices
        .iter()
        .filter(|voice| !voice.registered)
        .map(|voice| (voice.speaker.clone(), voice.ranges.clone()))
        .collect();
    // Finish the durable apply boundary even if cancellation arrived during the write.
    persist(task)?;
    Ok((applied, segments))
}
fn apply(task: &mut Task, plan: &Plan, voices: bool, control: &Control) -> Result<Applied> {
    let (applied, segments) = apply_names(task, plan, voices, control)?;
    let applied_at = Instant::now();
    let meeting = task.meeting.clone();
    let mut voice_status = if voices {
        "Голоса будут сохранены после закрытия уведомления, не раньше чем через 2 минуты".into()
    } else {
        String::new()
    };
    if let Some(error) = applied.index_error {
        voice_status.push_str(&format!(" Поиск не обновлён: {error}"));
    }
    task.progress.enrol_status = voice_status.clone();
    persist(task)?;
    Ok(Applied {
        applied_at,
        document: Box::new(DocumentData::read(meeting.clone())?),
        undo: Undo {
            sync: task
                .progress
                .pending_enrollment
                .as_ref()
                .context("Нет запроса сверки")?
                .sync(),
            meeting: meeting.clone(),
            before: applied.before,
            after: applied.after.revision,
        },
        segments,
        recording: plan.recording.clone(),
        voice_status,
    })
}
fn sync(
    task: &mut Task,
    settings: &Settings,
    explicit: Option<&str>,
    automatic: bool,
    voices: bool,
    store: &dyn CredentialStore,
    control: &Control,
) -> Outcome {
    let plan = match plan_for_task(task, settings, explicit, store, control) {
        Ok(plan) => plan,
        Err(_) if control.token.is_cancelled() => return Outcome::Cancelled,
        Err(error) => {
            return match error.downcast_ref::<ktalk::SelectionError>() {
                Some(ktalk::SelectionError::NotFound) => Outcome::Waiting,
                Some(ktalk::SelectionError::Refused(reason)) => Outcome::Refused(reason.clone()),
                Some(ktalk::SelectionError::Ambiguous { reason, recordings }) => {
                    if automatic {
                        Outcome::Refused(reason.clone())
                    } else {
                        Outcome::Choices {
                            reason: reason.clone(),
                            recordings: recordings.clone(),
                        }
                    }
                }
                None if error.is::<ktalk::TranscriptNotReady>() => Outcome::Waiting,
                None if error.is::<ktalk::InvalidTranscript>() => {
                    Outcome::Refused(error.to_string())
                }
                None => Outcome::Network(format!("{error:#}")),
            };
        }
    };
    if !automatic {
        return Outcome::Preview(Box::new(plan));
    }
    match apply(task, &plan, voices, control) {
        Ok(applied) => Outcome::Applied(Box::new(applied)),
        Err(_) if control.token.is_cancelled() => Outcome::Cancelled,
        Err(error) if timed_out(&error) => Outcome::Busy,
        Err(error) => Outcome::Refused(format!("{error:#}")),
    }
}
pub fn run(job: Job, store: &dyn CredentialStore) -> Message {
    run_control(job, store, &Control::default())
}
pub fn run_control(job: Job, store: &dyn CredentialStore, control: &Control) -> Message {
    let result = (|| -> Result<Message> {
        if !matches!(&job, Job::Enroll(_)) {
            control.check()?;
        }
        Ok(match job {
            Job::Name {
                meeting,
                expected,
                edit,
            } => {
                let mut voice_status = String::new();
                let library = Library::open(meeting.path().parent().context("Нет библиотеки")?)?;
                let own_allowed = recorded_here(&meeting)?;
                let mut fresh_embedding = None;
                let mut voices = true;
                if let Some(reason) =
                    library
                        .read_voices()?
                        .speaker_name_status(&expected, &edit, own_allowed)
                {
                    voice_status = reason;
                    voices = false;
                }
                if voices && let notetaker_core::SpeakerEdit::Rename { speaker, .. } = &edit {
                    let progress = read_progress(&meeting)?;
                    if let Some(ranges) = progress.guest_ranges.get(speaker) {
                        let prepare = (|| -> Result<Vec<f32>> {
                            control.check()?;
                            let config = notetaker_pipeline::AsrConfig::new("auto")?;
                            let mut process = crate::worker::background_process(&config.binary);
                            process.env(
                                "NOTETAKER_MODELS_DIR",
                                crate::config::Settings::load()?.models_path(),
                            );
                            let handle = notetaker_asr_proto::CancellationHandle::default();
                            control.attach(handle.clone());
                            let mut client =
                                notetaker_asr_proto::AsrClient::spawn_command_cancellable(
                                    process,
                                    Duration::from_secs(300),
                                    handle,
                                )?;
                            notetaker_pipeline::named_voice_embedding(
                                &meeting,
                                &expected,
                                ranges,
                                |wav, ranges| {
                                    control.check()?;
                                    let result = client.embed(wav, ranges)?;
                                    control.check()?;
                                    Ok(result)
                                },
                                || control.token.is_cancelled(),
                            )
                        })();
                        match prepare {
                            Ok(embedding) => fresh_embedding = Some(embedding),
                            Err(error) => {
                                voice_status = format!("Голос не запомнен: {error:#}");
                                voices = false;
                            }
                        }
                    }
                }
                control.check()?;
                let result = library
                    .edit_speakers_owned(
                        &meeting,
                        &expected,
                        edit,
                        voices,
                        own_allowed,
                        fresh_embedding,
                    )
                    .map_err(|error| error.to_string());
                Message::Named {
                    path: meeting.path().into(),
                    result: Box::new(result),
                    data: DocumentData::read(meeting).ok().map(Box::new),
                    voice_status,
                }
            }
            Job::Load(root) => return load(&root, store),
            Job::Save { settings, key } => {
                settings.validate()?;
                if let Some(key) = key {
                    settings.save_key(&key, store)?;
                }
                settings.save_to(&notetaker_llm::Config::path()?)?;
                Message::Saved(settings)
            }
            Job::Check(settings) => Message::Checked(
                settings
                    .client(store)?
                    .with_cancellation(control.token.clone())
                    .profile()?
                    .name(),
            ),
            Job::Sync {
                mut task,
                settings,
                explicit,
                automatic,
                voices,
            } => {
                let outcome = sync(
                    &mut task,
                    &settings,
                    explicit.as_deref(),
                    automatic,
                    voices,
                    store,
                    control,
                );
                let journal_error = if automatic {
                    task.after(&outcome, Utc::now());
                    persist(&task)
                        .err()
                        .map(|error| format!("Состояние Толка не сохранено: {error:#}"))
                } else {
                    None
                };
                Message::Synced {
                    task,
                    outcome,
                    automatic,
                    journal_error,
                }
            }
            Job::Apply {
                mut task,
                plan,
                voices,
            } => {
                let outcome = match apply(&mut task, &plan, voices, control) {
                    Ok(applied) => Outcome::Applied(Box::new(applied)),
                    Err(error) if timed_out(&error) => Outcome::Busy,
                    Err(error) => Outcome::Refused(format!("{error:#}")),
                };
                task.after(&outcome, Utc::now());
                let journal_error = persist(&task)
                    .err()
                    .map(|error| format!("Состояние Толка не сохранено: {error:#}"));
                Message::Synced {
                    task,
                    outcome,
                    automatic: false,
                    journal_error,
                }
            }
            Job::Undo(undo) => {
                control.check()?;
                let progress = read_progress(&undo.meeting)?;
                anyhow::ensure!(
                    progress
                        .pending_enrollment
                        .as_ref()
                        .is_some_and(|pending| pending.revision == undo.after),
                    "Транскрипт изменился после сверки, отмена недоступна"
                );
                undo.meeting.restore_transcript(&undo.before, undo.after)?;
                let mut task = Task {
                    ended: undo.meeting.read_meta()?.ended_at.unwrap_or_else(Utc::now),
                    progress: read_progress(&undo.meeting)?,
                    meeting: undo.meeting.clone(),
                };
                task.progress.enrol_pending = false;
                task.progress.pending_enrollment = None;
                task.progress.enrol_status = "Сверка отменена".into();
                persist(&task)?;
                Library::open(undo.meeting.path().parent().context("Нет библиотеки")?)?
                    .reindex_incremental()?;
                Message::Undone {
                    document: Box::new(DocumentData::read(undo.meeting)?),
                    progress: task.progress,
                }
            }
            Job::Enroll(mut task) => {
                task.progress.voice_offer_seen = true;
                task.progress.enrol_pending = true;
                if task.progress.recording_id.is_none() {
                    let ids: std::collections::BTreeSet<_> = task
                        .meeting
                        .read_transcript()?
                        .ktalk_sources
                        .into_values()
                        .collect();
                    anyhow::ensure!(
                        ids.len() == 1,
                        "Выберите запись Толка вручную перед обучением"
                    );
                    task.progress.recording_id = ids.into_iter().next();
                }
                anyhow::ensure!(
                    task.progress.pending_enrollment.is_some(),
                    "Повторите сверку Толка перед обучением"
                );
                task.progress.next = None;
                persist(&task)?;
                mark_offer_seen(&task)?;
                Message::Enrolled {
                    path: task.meeting.path().into(),
                    status: "Голоса будут сохранены после закрытия уведомления, не раньше чем через 2 минуты после сверки".into(),
                    task,
                }
            }
            Job::SkipEnrollment(mut task, status) => {
                task.progress.enrol_pending = false;
                task.progress.pending_enrollment = None;
                task.progress.enrol_status = status.clone();
                persist(&task)?;
                Message::Enrolled {
                    path: task.meeting.path().into(),
                    status,
                    task,
                }
            }
            Job::Resume(mut task, settings) => {
                if !pending_matches(&task)? {
                    task.progress.enrol_pending = false;
                    task.progress.pending_enrollment = None;
                    task.progress.enrol_status =
                        "Транскрипт изменился после сверки; обучение пропущено".into();
                    persist(&task)?;
                    return Ok(Message::Enrolled {
                        path: task.meeting.path().into(),
                        status: task.progress.enrol_status.clone(),
                        task,
                    });
                }
                let result = if !task.meeting.mic_audio_path().is_file()
                    && !task.meeting.system_audio_path().is_file()
                {
                    task.progress.enrol_pending = false;
                    Ok("Аудио не хранится: голоса не запомнены".into())
                } else {
                    plan_for_task(
                        &task,
                        &settings,
                        task.progress.recording_id.as_deref(),
                        store,
                        control,
                    )
                    .and_then(|plan| enroll_plan(&task, &plan, control))
                };
                let status = match result {
                    Ok(status) => {
                        task.progress.enrol_pending = false;
                        task.progress.pending_enrollment = None;
                        status
                    }
                    Err(_) if control.token.is_cancelled() => {
                        task.progress.next = None;
                        "Обучение будет продолжено после запуска".into()
                    }
                    Err(error) => {
                        task.network_retry(Utc::now());
                        format!("Обучение ожидает повтора: {error:#}")
                    }
                };
                task.progress.enrol_status = status.clone();
                persist(&task)?;
                Message::Enrolled {
                    path: task.meeting.path().into(),
                    status,
                    task,
                }
            }
            Job::Seen(mut task) => {
                task.progress.voice_offer_seen = true;
                persist(&task)?;
                mark_offer_seen(&task)?;
                Message::Seen
            }
            Job::Inspect(meeting) => Message::Inspected(Task {
                progress: {
                    let mut progress = read_progress(&meeting)?;
                    progress.done |= !meeting.read_transcript()?.ktalk_sources.is_empty();
                    progress
                },
                ended: meeting.read_meta()?.ended_at.unwrap_or_else(Utc::now),
                meeting,
            }),
            Job::Merge { root, from, into } => {
                let library = Library::open(&root)?;
                let people = library.people(false, "", "")?;
                let matches: Vec<_> = people
                    .iter()
                    .filter(|person| person.matches(&from))
                    .collect();
                anyhow::ensure!(
                    matches.len() <= 1,
                    "Имя участника неоднозначно; выберите человека в разделе «Люди»"
                );
                let person = matches
                    .first()
                    .cloned()
                    .cloned()
                    .or_else(|| notetaker_core::person_from_name(&from))
                    .context("Нет имени участника")?;
                let mut edits = library.read_people_edits()?;
                edits.merge(&person, &into);
                library.write_people_edits(&edits)?;
                Message::People(library.people(false, "", "")?)
            }
        })
    })();
    result.unwrap_or_else(|error| Message::Error(format!("{error:#}")))
}
pub fn dispatch(job: Job) -> Message {
    run(job, &OsCredentialStore)
}

fn settings_draft(state: &State) -> Settings {
    let mut settings = state.settings.clone().unwrap_or_else(|| Settings {
        base_url: String::new(),
        credential_id: "desktop".into(),
        api_key_env: None,
        extra: Default::default(),
    });
    settings.base_url = state.address.trim().into();
    settings.set_auto_sync(
        state
            .automatic_draft
            .unwrap_or_else(|| settings.auto_sync()),
    );
    settings
}
pub fn settings_blur(state: &mut State) -> Option<Job> {
    if state.busy || !state.settings_edited {
        return None;
    }
    let mut settings = settings_draft(state);
    let same_origin = state
        .settings
        .as_ref()
        .is_some_and(|old| old.base_url == settings.base_url);
    if settings.validate().is_err() || state.key.is_empty() && (!state.saved || !same_origin) {
        return None;
    }
    let key = (!state.key.is_empty()).then(|| std::mem::take(&mut state.key));
    if key.is_some() {
        settings.api_key_env = None;
    }
    state.replacing = false;
    state.settings_edited = false;
    Some(Job::Save { settings, key })
}
pub fn settings_ui(ui: &mut eframe::egui::Ui, state: &mut State) -> Option<Job> {
    use crate::theme::{self, ButtonStyle, StatusStyle};
    let mut job = None;
    let mut flush = false;
    let mut settings = settings_draft(state);
    theme::settings_group(ui, "Пространство Толка", |ui| {
        ui.add_enabled_ui(!state.busy,|ui| {
            theme::setting_row(ui,"Адрес пространства",|ui| {
                let response=ui.add(theme::text_field(&mut state.address).hint_text("https://space.ktalk.ru").desired_width(ui.available_width()));
                state.settings_edited|=response.changed();flush|=response.lost_focus();
            });
            theme::setting_row(ui,"Персональный ключ",|ui| {
                ui.vertical(|ui| {
                    if state.saved && !state.replacing {
                        ui.horizontal(|ui| {
                            ui.weak("сохранён");
                            if theme::button(ui,"Заменить",ButtonStyle::Ghost).clicked() {state.replacing=true;}
                        });
                    } else {
                        let response=ui.add(theme::text_field(&mut state.key).password(true).desired_width(ui.available_width()));
                        state.settings_edited|=response.changed();flush|=response.lost_focus();
                    }
                    ui.hyperlink_to(eframe::egui::RichText::new("Где взять ключ").small().color(theme::Palette::of(ui).muted),
                        "https://support.kontur.ru/talk/86797-personalnyj_apiklyuch_dostupa_v_tolke");
                });
            });
            let mut automatic=state.automatic_draft.unwrap_or_else(||settings.auto_sync());
            let response=theme::toggle(ui,&mut automatic,"Подписывать реплики именами участников после встречи");
            state.settings_edited|=response.changed();flush|=response.changed();
            state.automatic_draft=Some(automatic);settings.set_auto_sync(automatic);
        });
        settings.base_url = state.address.trim().into();
        let same_origin = state
            .settings
            .as_ref()
            .is_some_and(|old| old.base_url == settings.base_url);
        let valid = settings.validate();
        theme::settings_footer(ui, |ui| {
            if ui
                .add_enabled(
                    !state.busy && state.saved && same_origin && valid.is_ok(),
                    theme::styled_button(
                        theme::Palette::of(ui),
                        "Проверить",
                        ButtonStyle::Secondary,
                    ),
                )
                .clicked()
            {
                job = Some(Job::Check(settings.clone()));
            }
            if state.busy {
                ui.spinner();
            }
            let (text, style) = if let Err(error) = &valid
                && !state.address.is_empty()
            {
                (error.to_string(), StatusStyle::Error)
            } else if state.settings_edited
                || state.replacing
                || !same_origin
                || state.status.is_empty()
                || state.status == "Сохранено"
                || state.status == "Люди объединены"
            {
                ("Не проверено".into(), StatusStyle::Muted)
            } else if state.status.starts_with("Подключено:") {
                (state.status.clone(), StatusStyle::Positive)
            } else {
                (state.status.clone(), StatusStyle::Error)
            };
            theme::status_chip(ui, &text, style);
        });
        if ui.is_enabled()
            && flush
            && let Some(save) = settings_blur(state)
        {
            job = Some(save);
        }
    });
    theme::callout(ui, theme::icons::INFO, |ui| {
        ui.weak("Только чтение из Толка. Ничего из библиотеки не отправляется. Текст транскрипции Толка не сохраняется. Работает и для Local-встреч.");
    });
    job
}
fn merge_people(ui: &eframe::egui::Ui, name: &str) -> Vec<notetaker_core::Person> {
    ui.ctx()
        .data(|data| {
            data.get_temp::<Vec<notetaker_core::Person>>(eframe::egui::Id::unique("ktalk-people"))
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|person| person.name != name)
        .collect()
}
fn merge_options(ui: &mut eframe::egui::Ui, name: &str, people: &[notetaker_core::Person]) {
    for person in people {
        if crate::theme::menu_item(ui, "", &person.name, false, false).clicked() {
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    eframe::egui::Id::unique("ktalk-merge"),
                    (name.to_string(), person.clone()),
                )
            });
            ui.close();
        }
    }
}
pub fn merge_menu(ui: &mut eframe::egui::Ui, name: &str) {
    let people = merge_people(ui, name);
    crate::theme::select(("ktalk-person", name))
        .selected_text("Объединить с человеком…")
        .show_ui(ui, |ui| merge_options(ui, name, &people));
}
fn merge_icon(ui: &mut eframe::egui::Ui, name: &str) {
    let people = merge_people(ui, name);
    if people.is_empty() {
        ui.allocate_space(eframe::egui::vec2(
            crate::theme::COMPACT_HEIGHT,
            crate::theme::COMPACT_HEIGHT,
        ));
        return;
    }
    let button = crate::theme::icon_button(
        ui,
        crate::theme::icons::USERS,
        "Объединить с человеком из списка",
    );
    eframe::egui::Popup::menu(&button).show(|ui| {
        eframe::egui::ScrollArea::vertical()
            .max_height(240.0)
            .show(ui, |ui| merge_options(ui, name, &people));
    });
}
fn preview_speaker(transcript: Option<&Transcript>, speaker: &str, rows: &[ktalk::Row]) -> String {
    if speaker == "me"
        || transcript.is_some_and(|text| {
            text.self_speaker.as_deref() == Some(speaker)
                || text.segments.iter().any(|segment| {
                    segment.speaker.0 == speaker && segment.channel == notetaker_core::Channel::Mic
                })
        })
    {
        return "Я".into();
    }
    if let Some(name) = transcript.and_then(|text| text.speaker_names.get(speaker)) {
        return name.clone();
    }
    let mut ids = Vec::new();
    for row in rows {
        if row.speaker != "me" && !ids.contains(&row.speaker.as_str()) {
            ids.push(row.speaker.as_str());
        }
    }
    let number = ids
        .iter()
        .position(|id| *id == speaker)
        .map_or(1, |index| index + 1);
    format!("Собеседник {number}")
}
fn preview_participant<'a>(state: &'a State, name: &'a str) -> &'a str {
    if let Some((from, person)) = &state.merge
        && from == name
    {
        return &person.name;
    }
    let mut people = state.people.iter().filter(|person| person.matches(name));
    if let Some(person) = people.next()
        && people.next().is_none()
    {
        &person.name
    } else {
        name
    }
}
fn preview_table(
    ui: &mut eframe::egui::Ui,
    state: &State,
    plan: &Plan,
    transcript: Option<&Transcript>,
) {
    use crate::theme;
    let height = theme::COMPACT_HEIGHT;
    let gap = ui.spacing().item_spacing.x;
    let numeric = 52.0;
    let name_width = ((ui.available_width() - 2.0 * numeric - height - 4.0 * gap) / 2.0).max(32.0);
    eframe::egui::Grid::new("ktalk-preview-table")
        .num_columns(5)
        .min_col_width(0.0)
        .min_row_height(height)
        .spacing(eframe::egui::vec2(gap, 0.0))
        .striped(true)
        .show(ui, |ui| {
            for (label, width) in [
                ("Сейчас", name_width),
                ("Станет", name_width),
                ("Реплик", numeric),
                ("Время", numeric),
                ("", height),
            ] {
                ui.add_sized(
                    [width, height],
                    eframe::egui::Label::new(
                        eframe::egui::RichText::new(label)
                            .small()
                            .color(theme::Palette::of(ui).muted),
                    )
                    .truncate(),
                );
            }
            ui.end_row();
            for row in &plan.rows {
                let current = preview_speaker(transcript, &row.speaker, &plan.rows);
                let next = preview_participant(state, &row.participant);
                for text in [current.as_str(), next] {
                    ui.add_sized(
                        [name_width, height],
                        eframe::egui::Label::new(text).truncate(),
                    )
                    .on_hover_text(text);
                }
                ui.add_sized(
                    [numeric, height],
                    eframe::egui::Label::new(row.segments.to_string()),
                );
                let seconds = row.seconds.round() as u64;
                ui.add_sized(
                    [numeric, height],
                    eframe::egui::Label::new(format!("{}:{:02}", seconds / 60, seconds % 60)),
                );
                ui.push_id((&row.speaker, &row.participant), |ui| {
                    merge_icon(ui, &row.participant)
                });
                ui.end_row();
            }
        });
    ui.add_space(theme::SPACE[2]);
    let offset = format!("{:.1}", plan.offset_ms as f64 / 1000.0).replace('.', ",");
    ui.label(
        eframe::egui::RichText::new(format!(
            "Сверено по тексту: совпадение {:.0}%, сдвиг времени {offset} с",
            plan.validation_score * 100.0
        ))
        .small()
        .color(theme::Palette::of(ui).muted),
    );
}
pub fn windows(ctx: &eframe::egui::Context, state: &mut crate::state::AppState) {
    use crate::theme::{self, ButtonStyle};
    ctx.data_mut(|data| {
        data.insert_temp(
            eframe::egui::Id::unique("ktalk-people"),
            state.ktalk.people.clone(),
        )
    });
    ctx.data_mut(|data| {
        data.insert_temp(
            eframe::egui::Id::unique("ktalk-source-times"),
            state
                .ktalk
                .tasks
                .iter()
                .filter_map(|(path, task)| {
                    task.progress.source_time.map(|time| (path.clone(), time))
                })
                .collect::<BTreeMap<_, _>>(),
        )
    });
    let mut job = None;
    let mut explicit = None;
    let mut close = false;
    if state.ktalk.preview_open {
        let busy = state.ktalk.busy;
        let can_apply = matches!(state.ktalk.preview, Some((_, Outcome::Preview(_))));
        let mut apply = false;
        close = theme::modal(ctx, "Имена из Толка")
            .default_width(620.0)
            .show_footer(
                ctx,
                |ui| {
                    if state.ktalk.busy {
                        ui.spinner();
                        ui.weak("Сопоставляю запись…");
                    }
                    if let Some((task, outcome)) = &state.ktalk.preview {
                        match outcome {
                            Outcome::Preview(plan) => {
                                ui.label(
                                    eframe::egui::RichText::new(&plan.recording.title)
                                        .font(theme::medium(theme::BODY)),
                                );
                                ui.weak(
                                    plan.recording
                                        .created_date
                                        .with_timezone(&chrono::Local)
                                        .format("%d.%m.%Y %H:%M")
                                        .to_string(),
                                );
                                preview_table(
                                    ui,
                                    &state.ktalk,
                                    plan,
                                    state
                                        .documents
                                        .get(task.meeting.path())
                                        .map(|document| &document.data.transcript),
                                );
                            }
                            Outcome::Choices { reason, recordings } => {
                                ui.label(reason);
                                for recording in recordings {
                                    if theme::button(
                                        ui,
                                        &format!(
                                            "{} · {}",
                                            recording.title,
                                            recording.created_date.format("%d.%m %H:%M")
                                        ),
                                        ButtonStyle::Secondary,
                                    )
                                    .clicked()
                                    {
                                        explicit = Some((
                                            task.meeting.path().to_path_buf(),
                                            recording.id.clone(),
                                        ));
                                    }
                                }
                            }
                            Outcome::Refused(reason) => {
                                ui.label(reason);
                            }
                            _ => {}
                        }
                    }
                },
                |ui| {
                    theme::footer(ui, |ui| {
                        close |= theme::button(ui, "Отмена", ButtonStyle::Ghost).clicked();
                        if can_apply {
                            apply = ui
                                .add_enabled(
                                    !busy,
                                    theme::styled_button(
                                        theme::Palette::of(ui),
                                        "Применить",
                                        ButtonStyle::Primary,
                                    ),
                                )
                                .clicked();
                        }
                    });
                },
            )
            || close;
        if apply && let Some((task, Outcome::Preview(plan))) = state.ktalk.preview.take() {
            job = Some(Job::Apply {
                task,
                plan,
                voices: state.settings.voice_library,
            });
        }
    }
    if close && !state.ktalk.busy {
        state.ktalk.preview_open = false;
        state.ktalk.preview = None;
    }
    if let Some((path, id)) = explicit {
        state.ktalk_manual(&path, Some(id));
    }
    if let Some(request) = ctx.data_mut(|data| {
        data.remove_temp::<(String, notetaker_core::Person)>(eframe::egui::Id::unique(
            "ktalk-merge",
        ))
    }) {
        state.ktalk.merge = Some(request);
    }
    if let Some((from, into)) = state.ktalk.merge.clone() {
        let closed = theme::modal(ctx, "Объединить людей?").show(ctx, |ui| {
            ui.label(format!(
                "Объединить «{from}» с «{}»? Имена и связи будут объединены.",
                into.name
            ));
            theme::footer(ui, |ui| {
                if theme::button(ui, "Отмена", ButtonStyle::Ghost).clicked() {
                    state.ktalk.merge = None;
                }
                if theme::button(ui, "Объединить", ButtonStyle::DangerFilled).clicked() {
                    job = Some(Job::Merge {
                        root: state.settings.library.clone(),
                        from: from.clone(),
                        into: into.clone(),
                    });
                }
            });
        });
        if closed {
            state.ktalk.merge = None;
        }
    }
    if let Some(job) = job {
        state.ktalk_job(job);
    }
}
pub fn source_note(ui: &mut eframe::egui::Ui, path: &Path, transcript: &Transcript) {
    if transcript.ktalk_sources.is_empty() {
        return;
    }
    let time = ui
        .ctx()
        .data(|data| {
            data.get_temp::<BTreeMap<PathBuf, DateTime<Utc>>>(eframe::egui::Id::unique(
                "ktalk-source-times",
            ))
        })
        .and_then(|times| times.get(path).copied())
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("Запись Толка: %d.%m.%Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Время записи Толка не сохранено этим приложением".into());
    ui.label(
        eframe::egui::RichText::new("Имена из Толка")
            .small()
            .color(crate::theme::Palette::of(ui).muted),
    )
    .on_hover_text(time);
}
pub fn transcript_ui(ui: &mut eframe::egui::Ui, state: &mut crate::state::AppState, path: &Path) {
    use crate::theme::{self, ButtonStyle};
    let Some(document) = state.documents.get(path) else {
        return;
    };
    let source = !document.data.transcript.ktalk_sources.is_empty();
    let meeting = document.data.meeting.clone();
    if source {
        source_note(ui, path, &document.data.transcript);
    }
    if source && !state.ktalk.inspected.contains(path) && !state.ktalk.busy {
        state.ktalk_job(Job::Inspect(meeting));
        return;
    }
    let mut enable = false;
    let mut dismiss = false;
    let show_offer = source
        && !state.settings.voice_library
        && !state.ktalk.voice_offer_seen
        && state
            .ktalk
            .tasks
            .get(path)
            .is_some_and(|task| !task.progress.voice_offer_seen);
    if show_offer {
        theme::callout(ui, theme::icons::INFO, |ui| {
            ui.label("Запоминать голоса этих людей, чтобы узнавать их на встречах без записи Толка? Отпечатки голосов хранятся только на этом компьютере.");
            ui.horizontal_wrapped(|ui| {
                enable = theme::button(ui, "Включить", ButtonStyle::Primary).clicked();
                dismiss = theme::button(ui, "Не сейчас", ButtonStyle::Ghost).clicked();
            });
        });
    }
    if (enable || dismiss)
        && !state.ktalk.busy
        && let Some(task) = state.ktalk.tasks.get_mut(path)
    {
        task.progress.voice_offer_seen = true;
        state.ktalk.voice_offer_seen = true;
        let task = task.clone();
        if enable {
            state.ktalk.enroll_pending = Some(task);
            state.settings_draft.voice_library = true;
            state.save_preferences();
        } else {
            state.ktalk_job(Job::Seen(task));
        }
    }
    if let Some(status) = state
        .ktalk
        .voice_status
        .get(path)
        .filter(|text| !text.is_empty())
    {
        ui.weak(status);
    }
    if let Some(reason) = state
        .ktalk
        .tasks
        .get(path)
        .and_then(|task| task.progress.reason.as_ref())
    {
        ui.weak(reason);
    }
}

impl crate::state::AppState {
    pub fn ktalk_end_undo(&mut self, path: &Path) {
        if let Some(pending) = self
            .ktalk
            .tasks
            .get(path)
            .and_then(|task| task.progress.pending_enrollment.as_ref())
        {
            self.ktalk.undo_closed.insert(pending.sync());
            self.ktalk.undo_visible.remove(&pending.sync());
        }
    }
    pub fn ktalk_sync_current(&self, undo: &Undo) -> bool {
        !self.ktalk.undo_closed.contains(&undo.sync)
            && self
                .ktalk
                .tasks
                .get(undo.meeting.path())
                .is_none_or(|task| {
                    task.progress
                        .pending_enrollment
                        .as_ref()
                        .is_some_and(|pending| pending.sync() == undo.sync)
                })
            && self
                .documents
                .get(undo.meeting.path())
                .is_none_or(|doc| doc.data.revisions.transcript == undo.after)
    }
    pub fn ktalk_can_undo(&self, undo: &Undo) -> bool {
        self.ktalk_sync_current(undo)
            && (self.ktalk.undo_visible.contains(&undo.sync)
                || self
                    .ktalk
                    .undo_pending
                    .as_ref()
                    .is_some_and(|pending| pending.sync == undo.sync))
            && self
                .documents
                .get(undo.meeting.path())
                .is_none_or(|document| {
                    !document.dirty.any()
                        && !document.saving
                        && document.data.revisions.transcript == undo.after
                })
    }
    pub fn ktalk_job(&mut self, job: Job) {
        self.ktalk_job_at(job, Moment::now());
    }
    fn ktalk_job_at(&mut self, job: Job, now: Moment) {
        if self.ktalk.busy || self.settings_pending {
            return;
        }
        if self.exit_requested {
            return;
        }
        if let Job::Resume(task, _) = &job
            && let Some(pending) = &task.progress.pending_enrollment
        {
            if !self.ktalk.enrollment_ready(task, now) {
                return;
            }
            self.ktalk.undo_closed.insert(pending.sync());
            self.ktalk.undo_visible.remove(&pending.sync());
        }
        if let Job::Name { meeting, .. } = &job {
            self.ktalk_end_undo(meeting.path());
        }
        self.ktalk.biometric = matches!(
            &job,
            Job::Sync { voices: true, .. }
                | Job::Apply { voices: true, .. }
                | Job::Enroll(_)
                | Job::Resume(..)
                | Job::Name { .. }
        );
        self.ktalk.cancel = Control::default();
        self.ktalk.busy = true;
        if let Job::Sync { task, .. } | Job::Apply { task, .. } = &job {
            self.ktalk
                .tasks
                .entry(task.meeting.path().into())
                .or_insert_with(|| task.clone());
        }
        self.ktalk.busy_path = match &job {
            Job::Sync { task, .. }
            | Job::Apply { task, .. }
            | Job::Enroll(task)
            | Job::Resume(task, _) => Some(task.meeting.path().into()),
            Job::Undo(undo) => Some(undo.meeting.path().into()),
            Job::Name { meeting, .. } => Some(meeting.path().into()),
            _ => None,
        };
        self.send(crate::worker::Command::Ktalk(
            Box::new(job),
            self.ktalk.cancel.clone(),
        ));
    }
    pub fn ktalk_manual(&mut self, path: &Path, explicit: Option<String>) {
        if self.ktalk.busy
            || self.settings_pending
            || self.recording.path.as_deref() == Some(path) && self.recording.busy()
            || self
                .documents
                .get(path)
                .is_some_and(|doc| doc.dirty.any() || doc.saving || doc.speaker_ui.busy)
        {
            return;
        }
        let Some(settings) = self.ktalk.settings.clone() else {
            self.ktalk.status = "Настройте Толк в настройках".into();
            self.notices.push((
                crate::theme::ToastLevel::Info,
                "Откройте «Настройки → Толк» и сохраните персональный ключ.".into(),
            ));
            return;
        };
        let task = self.ktalk.tasks.get(path).cloned().or_else(|| {
            self.documents.get(path).map(|document| Task {
                meeting: document.data.meeting.clone(),
                ended: document.data.meta.ended_at.unwrap_or_else(Utc::now),
                progress: Progress {
                    done: true,
                    ..Default::default()
                },
            })
        });
        if let Some(task) = task {
            self.ktalk.preview_open = true;
            self.ktalk.preview = None;
            self.ktalk_job(Job::Sync {
                task,
                settings,
                explicit,
                automatic: false,
                voices: self.settings.voice_library,
            });
        }
    }
    pub fn poll_ktalk(&mut self) {
        self.poll_ktalk_at(Moment::now());
    }
    fn poll_ktalk_at(&mut self, now: Moment) {
        if self.exit_requested
            || !self.ready
            || self.settings_pending
            || self.settings_autosave.pending
            || self.ktalk.preview_open
            || self.recording.phase == crate::state::Phase::Finalizing
        {
            return;
        }
        if !self.ktalk.busy
            && let Some(root) = self.ktalk.load_root.take()
        {
            self.ktalk_job(Job::Load(root));
            return;
        }
        if !self.ktalk.busy
            && let Some(undo) = self.ktalk.undo_pending.clone()
        {
            let allowed = self.ktalk_can_undo(&undo);
            self.ktalk.undo_pending = None;
            if !allowed {
                self.error = Some("Транскрипт изменился после сверки, отмена недоступна".into());
            } else {
                self.ktalk_job(Job::Undo(undo));
            }
            return;
        }
        if !self.ktalk.busy
            && self.ktalk.saved
            && let Some(task) = self
                .ktalk
                .tasks
                .values()
                .find(|task| {
                    self.ktalk.enrollment_ready(task, now)
                        && self
                            .documents
                            .get(task.meeting.path())
                            .is_none_or(|document| {
                                !document.dirty.any()
                                    && !document.saving
                                    && !document.speaker_ui.busy
                            })
                })
                .cloned()
            && let Some(settings) = self.ktalk.settings.clone()
        {
            if !self.settings.voice_library {
                self.ktalk_job(Job::SkipEnrollment(
                    task,
                    "Библиотека голосов выключена".into(),
                ));
            } else {
                self.ktalk_job_at(Job::Resume(task, settings), now);
            }
            return;
        }
        if self.settings.voice_library
            && !self.ktalk.busy
            && let Some(task) = self.ktalk.enroll_pending.take()
        {
            self.ktalk_job(Job::Enroll(task));
            return;
        }
        if let Some(task) = self.ktalk.next(Utc::now(), |path| {
            self.recording.busy() && self.recording.path.as_deref() == Some(path)
                || self
                    .documents
                    .get(path)
                    .is_some_and(|doc| doc.dirty.any() || doc.saving || doc.speaker_ui.busy)
        }) && let Some(settings) = self.ktalk.settings.clone()
        {
            self.ktalk_job(Job::Sync {
                task,
                settings,
                explicit: None,
                automatic: true,
                voices: self.settings.voice_library,
            });
        }
    }
    pub fn ktalk_result(&mut self, message: Message) {
        self.ktalk.busy = false;
        self.ktalk.biometric = false;
        let path = self.ktalk.busy_path.take();
        match message {
            Message::Named {
                path,
                result,
                data,
                voice_status,
            } => {
                self.ktalk.voice_status.insert(path.clone(), voice_status);
                self.apply(crate::worker::Event::SpeakersEdited {
                    path,
                    result: *result,
                    data,
                });
                self.send(crate::worker::Command::Voices {
                    root: self.settings.library.clone(),
                    delete: None,
                });
            }
            Message::Loaded {
                settings,
                saved,
                tasks,
                people,
                voice_offer_seen,
            } => {
                self.ktalk.voice_offer_seen = voice_offer_seen;
                self.ktalk.people = people;
                self.ktalk.address = settings
                    .as_ref()
                    .map(|s| s.base_url.clone())
                    .unwrap_or_default();
                self.ktalk.settings = settings;
                self.ktalk.saved = saved;
                for task in tasks {
                    self.ktalk.inspected.insert(task.meeting.path().into());
                    self.ktalk
                        .tasks
                        .entry(task.meeting.path().into())
                        .or_insert(task);
                }
            }
            Message::Saved(settings) => {
                self.ktalk.automatic_draft = None;
                self.ktalk.saved = true;
                self.llm.ktalk = Some(settings.clone());
                self.llm_draft.ktalk = Some(settings.clone());
                self.ktalk.settings = Some(settings);
                self.ktalk.status = "Сохранено".into();
            }
            Message::Checked(name) => self.ktalk.status = format!("Подключено: {name}"),
            Message::Error(error) => {
                self.ktalk.status = error.clone();
                if self.ktalk.merge.is_some() {
                    self.error = Some(error.clone());
                }
                if let Some(path) = path {
                    if let Some(document) = self.documents.get_mut(&path) {
                        document.speaker_ui.busy = false;
                    }
                    if self.ktalk.preview_open
                        && let Some(task) = self.ktalk.tasks.get(&path)
                    {
                        self.ktalk.preview = Some((task.clone(), Outcome::Refused(error.clone())));
                    }
                    self.error = Some(error);
                }
            }
            Message::Synced {
                task,
                outcome,
                automatic,
                journal_error,
            } => {
                if let Some(error) = journal_error {
                    self.ktalk.status = error.clone();
                    self.notices
                        .push((crate::theme::ToastLevel::Warning, error));
                }
                let path = task.meeting.path().to_path_buf();
                match outcome {
                    Outcome::Applied(applied) => {
                        self.ktalk.inspected.insert(path.clone());
                        let applied = *applied;
                        self.ktalk
                            .clock
                            .applied
                            .insert(applied.undo.sync, applied.applied_at);
                        self.ktalk
                            .successes
                            .retain(|(undo, _)| undo.sync.meeting != applied.undo.sync.meeting);
                        self.ktalk
                            .undo_visible
                            .retain(|sync| sync.meeting != applied.undo.sync.meeting);
                        if self
                            .ktalk
                            .undo_pending
                            .as_ref()
                            .is_some_and(|undo| undo.sync.meeting == applied.undo.sync.meeting)
                        {
                            self.ktalk.undo_pending = None;
                        }
                        if let Some(document) = self.documents.get_mut(&path) {
                            document.data.transcript = applied.document.transcript;
                            document.data.revisions.transcript =
                                applied.document.revisions.transcript;
                            document.speaker_ui.invalidate_transcript();
                        }
                        self.ktalk.sources.insert(path.clone(), applied.recording);
                        self.ktalk
                            .voice_status
                            .insert(path.clone(), applied.voice_status);
                        self.ktalk
                            .successes
                            .push_back((applied.undo, applied.segments));
                        self.ktalk.preview = None;
                        self.ktalk.preview_open = false;
                        self.refresh();
                    }
                    Outcome::Network(error) => {
                        self.ktalk.status = error.clone();
                        if !automatic {
                            self.ktalk.preview = Some((task.clone(), Outcome::Refused(error)));
                        }
                    }
                    Outcome::Waiting if automatic => {}
                    Outcome::Busy if automatic => {}
                    outcome => {
                        let outcome = match outcome {
                            Outcome::Waiting => Outcome::Refused(
                                "Подходящая запись Толка не найдена или транскрипция ещё не готова"
                                    .into(),
                            ),
                            Outcome::Busy => {
                                Outcome::Refused("Встреча занята. Повторите позже.".into())
                            }
                            other => other,
                        };
                        if !automatic {
                            self.ktalk.preview = Some((task.clone(), outcome));
                        }
                    }
                }
                self.ktalk.tasks.insert(path, task);
            }
            Message::Undone { document, progress } => {
                if self
                    .ktalk
                    .enroll_pending
                    .as_ref()
                    .is_some_and(|task| task.meeting.path() == document.meeting.path())
                {
                    self.ktalk.enroll_pending = None;
                }
                self.ktalk.voice_status.insert(
                    document.meeting.path().into(),
                    progress.enrol_status.clone(),
                );
                if let Some(task) = self.ktalk.tasks.get_mut(document.meeting.path()) {
                    task.progress = progress;
                }
                if let Some(doc) = self.documents.get_mut(document.meeting.path()) {
                    doc.data.transcript = document.transcript;
                    doc.data.revisions.transcript = document.revisions.transcript;
                    doc.speaker_ui.invalidate_transcript();
                }
                self.refresh();
            }
            Message::Enrolled { path, status, task } => {
                if task.progress.enrol_pending {
                    self.ktalk.status = status.clone();
                }
                self.ktalk.tasks.insert(path.clone(), task);
                self.ktalk.voice_status.insert(path, status);
                self.send(crate::worker::Command::Voices {
                    root: self.settings.library.clone(),
                    delete: None,
                });
            }
            Message::Seen => {}
            Message::Inspected(task) => {
                self.ktalk.inspected.insert(task.meeting.path().into());
                self.ktalk.tasks.insert(task.meeting.path().into(), task);
            }
            Message::People(people) => {
                self.ktalk.people = people;
                self.ktalk.merge = None;
                self.ktalk.status = "Люди объединены".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn undo_window_writes_no_biometrics_and_restores_the_transcript() {
        let (root, mut task, plan) = fixture();
        let library = Library::open(root.path()).unwrap();
        library
            .write_meeting_voices(
                &task.meeting,
                BTreeMap::from([("me".into(), vec![1.0, 0.0])]),
            )
            .unwrap();
        let bank = library.private_state_dir().join("voices.json");
        let cache = library.private_state_dir().join(format!(
            "speaker-embeddings/{}.json",
            task.meeting.read_meta().unwrap().id
        ));
        let before_bank = std::fs::read(&bank).ok();
        let before_cache = std::fs::read(&cache).unwrap();
        let original = task.meeting.read_transcript().unwrap();
        let applied = apply(&mut task, &plan, true, &Control::default()).unwrap();
        assert_eq!(std::fs::read(&bank).ok(), before_bank);
        assert_eq!(std::fs::read(&cache).unwrap(), before_cache);
        assert!(matches!(
            run(Job::Undo(applied.undo), &Store::default()),
            Message::Undone { .. }
        ));
        assert_eq!(task.meeting.read_transcript().unwrap(), original);
        assert_eq!(std::fs::read(&bank).ok(), before_bank);
        assert_eq!(std::fs::read(&cache).unwrap(), before_cache);
        let progress = read_progress(&task.meeting).unwrap();
        assert!(!progress.enrol_pending && progress.pending_enrollment.is_none());
        assert!(!pending_matches(&task).unwrap());
        assert!(
            !library
                .private_state_dir()
                .join("voice-enrollments")
                .exists()
        );
    }
    #[test]
    fn monotonic_timer_ignores_wall_clock_jumps_and_waits_for_the_exact_toast() {
        let (_root, mut task, plan) = fixture();
        let applied = apply(&mut task, &plan, true, &Control::default()).unwrap();
        let pending = task.progress.pending_enrollment.as_ref().unwrap();
        let start = Moment {
            wall: pending.not_before - chrono::Duration::seconds(120),
            monotonic: applied.applied_at,
        };
        let (mut state, _commands, _events) = crate::test_ui::state();
        state
            .ktalk
            .clock
            .applied
            .insert(applied.undo.sync, start.monotonic);
        state.ktalk.undo_visible.insert(applied.undo.sync);
        let forward = Moment {
            wall: start.wall + chrono::Duration::minutes(5),
            monotonic: start.monotonic + Duration::from_secs(15),
        };
        assert!(!state.ktalk.enrollment_ready(&task, forward));
        let elapsed = Moment {
            wall: start.wall - chrono::Duration::hours(1),
            monotonic: start.monotonic + Duration::from_secs(121),
        };
        assert!(!state.ktalk.enrollment_ready(&task, elapsed));
        state.ktalk.undo_visible.remove(&applied.undo.sync);
        assert!(state.ktalk.enrollment_ready(&task, elapsed));
        state.ktalk_job_at(Job::Resume(task.clone(), settings()), elapsed);
        state.ktalk.undo_visible.insert(applied.undo.sync);
        assert!(!state.ktalk_can_undo(&applied.undo));
    }
    #[test]
    fn restart_waits_for_both_session_time_and_the_stored_deadline_without_undo() {
        let (root, mut task, plan) = fixture();
        let applied = apply(&mut task, &plan, true, &Control::default()).unwrap();
        let pending = task.progress.pending_enrollment.as_ref().unwrap().clone();
        let picked = pickup(root.path(), Utc::now()).unwrap();
        let restored = &picked[0];
        assert_eq!(
            restored.progress.pending_enrollment.as_ref(),
            Some(&pending)
        );
        let (state, _commands, _events) = crate::test_ui::state();
        let start = state.ktalk.clock.started;
        assert!(!state.ktalk_can_undo(&applied.undo));
        for (seconds, wall, expected) in [
            (15, pending.not_before + chrono::Duration::minutes(5), false),
            (
                121,
                pending.not_before - chrono::Duration::seconds(1),
                false,
            ),
            (121, pending.not_before, true),
        ] {
            assert_eq!(
                state.ktalk.enrollment_ready(
                    restored,
                    Moment {
                        wall,
                        monotonic: start + Duration::from_secs(seconds)
                    }
                ),
                expected
            );
        }
        assert_eq!(
            serde_json::to_value(&pending)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            4
        );
    }
    #[test]
    fn closing_an_older_sync_does_not_release_the_newer_toast() {
        let (_root, mut task, plan) = fixture();
        let first = apply(&mut task, &plan, true, &Control::default()).unwrap();
        let older = first.undo.sync;
        task.meeting
            .update_transcript(|text| {
                text.rename_speaker("me", "Новое имя");
            })
            .unwrap();
        let pending = task.progress.pending_enrollment.as_mut().unwrap();
        pending.revision = task.meeting.load_transcript().unwrap().revision;
        let newer = pending.sync();
        let mut state = State::default();
        let start = Instant::now();
        state.clock.applied.insert(newer, start);
        state.undo_visible.extend([older, newer]);
        state.undo_visible.remove(&older);
        let now = Moment {
            wall: Utc::now() + chrono::Duration::minutes(5),
            monotonic: start + Duration::from_secs(121),
        };
        assert!(!state.enrollment_ready(&task, now));
        state.undo_visible.remove(&newer);
        assert!(state.enrollment_ready(&task, now));
    }
    #[test]
    fn pending_pickup_ignores_age_and_future_meeting_end_times() {
        for hours in [25, -25] {
            let (root, mut task, plan) = fixture();
            apply(&mut task, &plan, true, &Control::default()).unwrap();
            let wall = task.ended + chrono::Duration::hours(hours);
            let mut picked = pickup(root.path(), wall).unwrap();
            assert_eq!(picked.len(), 1);
            assert!(picked[0].progress.enrol_pending);
            if hours < 0 {
                picked = pickup(root.path(), Utc::now()).unwrap();
                assert_eq!(picked.len(), 1);
            }
            let restored = picked.pop().unwrap();
            assert!(pending_matches(&restored).unwrap());
            let (mut state, commands, _events) = crate::test_ui::state();
            state.ready = true;
            state.settings.voice_library = true;
            state.ktalk.saved = true;
            state.ktalk.settings = Some(settings());
            state
                .ktalk
                .tasks
                .insert(restored.meeting.path().into(), restored);
            let now = Moment {
                wall: wall.max(Utc::now()) + chrono::Duration::minutes(3),
                monotonic: state.ktalk.clock.started + Duration::from_secs(121),
            };
            state.poll_ktalk_at(now);
            let job = commands
                .try_iter()
                .find_map(|command| match command {
                    crate::worker::Command::Ktalk(job, _) => Some(job),
                    _ => None,
                })
                .unwrap();
            assert!(matches!(*job, Job::Resume(..)));
            let reply = run(*job, &Store::default());
            assert!(
                matches!(reply, Message::Enrolled { status, .. } if status.contains("Аудио не хранится"))
            );
            assert!(
                read_progress(&task.meeting)
                    .unwrap()
                    .pending_enrollment
                    .is_none()
            );
        }
    }
    #[test]
    fn a_guest_name_collision_writes_neither_embedding_nor_transcript() {
        let (root, mut task, plan) = fixture();
        apply(&mut task, &plan, true, &Control::default()).unwrap();
        let library = Library::open(root.path()).unwrap();
        let text = task.meeting.read_transcript().unwrap();
        let guest = task.progress.guest_ranges.keys().next().unwrap().clone();
        let name = text.speaker_names[&guest].clone();
        library
            .remember_tolk_voices(
                &task.meeting,
                &text,
                vec![notetaker_core::VoiceSample {
                    speaker: "ktalk-another-person".into(),
                    name: name.clone(),
                    embedding: vec![1.0, 0.0],
                    own: false,
                }],
                task.meeting.load_transcript().unwrap().revision,
                || false,
            )
            .unwrap();
        let bank = library.private_state_dir().join("voices.json");
        let cache = library.private_state_dir().join(format!(
            "speaker-embeddings/{}.json",
            task.meeting.read_meta().unwrap().id
        ));
        let before = [
            std::fs::read(&bank).unwrap(),
            std::fs::read(&cache).unwrap(),
            std::fs::read(task.meeting.path().join("transcript.json")).unwrap(),
        ];
        let (mut state, commands, _events) = crate::test_ui::state();
        state
            .ktalk
            .undo_visible
            .insert(task.progress.pending_enrollment.as_ref().unwrap().sync());
        state
            .ktalk
            .tasks
            .insert(task.meeting.path().into(), task.clone());
        state.ktalk_job(Job::Name {
            meeting: task.meeting.clone(),
            expected: text,
            edit: notetaker_core::SpeakerEdit::Rename {
                speaker: guest,
                name,
                is_me: false,
                everywhere: false,
            },
        });
        assert!(state.ktalk.undo_visible.is_empty());
        let job = commands
            .try_iter()
            .find_map(|command| match command {
                crate::worker::Command::Ktalk(job, _) => Some(job),
                _ => None,
            })
            .unwrap();
        let reply = run(*job, &Store::default());
        assert!(
            matches!(reply, Message::Named { voice_status, result, .. } if voice_status == notetaker_core::VOICE_NAME_TAKEN && result.is_ok())
        );
        assert_eq!(
            [
                std::fs::read(bank).unwrap(),
                std::fs::read(cache).unwrap(),
                std::fs::read(task.meeting.path().join("transcript.json")).unwrap()
            ],
            before
        );
    }
    #[test]
    fn edits_or_undo_drop_pending_before_any_network_or_embedding() {
        for undo in [false, true] {
            let (_root, mut task, plan) = fixture();
            let applied = apply(&mut task, &plan, true, &Control::default()).unwrap();
            if undo {
                assert!(matches!(
                    run(Job::Undo(applied.undo), &Store::default()),
                    Message::Undone { .. }
                ));
            } else {
                task.meeting
                    .update_transcript(|text| text.segments[0].text = "Поздняя правка".into())
                    .unwrap();
            }
            let reply = run(Job::Resume(task.clone(), settings()), &Store::default());
            assert!(matches!(reply, Message::Enrolled { .. }));
            let progress = read_progress(&task.meeting).unwrap();
            assert!(!progress.enrol_pending && progress.pending_enrollment.is_none());
        }
    }
    #[test]
    fn startup_removes_unreleased_biometric_snapshots() {
        let (root, task, _plan) = fixture();
        let library = Library::open(root.path()).unwrap();
        // Publish the obsolete directory atomically: other startup tests may clean it too.
        let staged = tempfile::tempdir_in(library.private_state_dir()).unwrap();
        std::fs::write(
            staged.path().join("old.json"),
            b"{\"before\":{\"embeddings\":[[1,0]]}}",
        )
        .unwrap();
        let obsolete = library.private_state_dir().join("voice-enrollments");
        std::fs::rename(staged.path(), &obsolete).unwrap();
        pickup(root.path(), Utc::now()).unwrap();
        assert!(!obsolete.exists());
        assert!(recorded_here(&task.meeting).unwrap());
    }
    #[test]
    fn shared_meetings_and_imports_never_gain_local_provenance() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        for title in ["Встреча коллеги", "Переименованный импорт"]
        {
            let mut meta =
                notetaker_core::MeetingMeta::new(title, Utc::now() - chrono::Duration::minutes(10));
            meta.ended_at = Some(Utc::now());
            meta.title_source = Some(notetaker_core::TitleSource::User);
            let meeting = library.create_meeting(meta).unwrap();
            let (text, _) = crate::ktalk_fixture::plan();
            meeting.write_transcript(&text).unwrap();
            std::fs::write(meeting.mic_audio_path(), b"not a proof").unwrap();
            assert!(!recorded_here(&meeting).unwrap());
            let mut task = Task {
                meeting: meeting.clone(),
                ended: Utc::now(),
                progress: Progress {
                    recorded_here: true,
                    ..Default::default()
                },
            };
            persist(&task).unwrap();
            assert!(!recorded_here(&meeting).unwrap());
            task.progress.recorded_here = false;
        }
        assert!(pickup(root.path(), Utc::now()).unwrap().is_empty());
    }
    #[test]
    fn dirty_editor_blocks_undo_without_a_worker_write() {
        let (_root, mut task, plan) = fixture();
        let applied = apply(&mut task, &plan, false, &Control::default()).unwrap();
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        let path = task.meeting.path().to_path_buf();
        let before = std::fs::read(path.join("transcript.json")).unwrap();
        let mut document = crate::state::Document::new(*applied.document);
        document.data.transcript.segments[0].text = "Несохранённый текст редактора".into();
        document.dirty.transcript = true;
        state.documents.insert(path.clone(), document);
        state.ktalk.undo_pending = Some(applied.undo);
        state.poll_ktalk();
        assert_eq!(
            state.error.as_deref(),
            Some("Транскрипт изменился после сверки, отмена недоступна")
        );
        assert!(
            commands
                .try_iter()
                .all(|command| !matches!(command, crate::worker::Command::Ktalk(..)))
        );
        assert_eq!(std::fs::read(path.join("transcript.json")).unwrap(), before);
        assert_eq!(
            state.documents[&path].data.transcript.segments[0].text,
            "Несохранённый текст редактора"
        );
    }
    #[test]
    fn network_errors_do_not_consume_successful_schedule_points() {
        let (_root, mut task, _) = fixture();
        let now = task.ended;
        for minute in [0, 1, 6, 21, 30] {
            task.after(
                &Outcome::Network("Нет сети".into()),
                now + chrono::Duration::minutes(minute),
            );
            assert_eq!(task.progress.answered, 0);
            assert!(!task.progress.done);
        }
        task.after(&Outcome::Waiting, now + chrono::Duration::minutes(30));
        assert_eq!(task.progress.answered, 1);
        assert_eq!(
            task.progress.next,
            Some(now + chrono::Duration::minutes(33))
        );
        assert!(!task.progress.done);
        for minute in [33, 40, 60] {
            task.after(&Outcome::Waiting, now + chrono::Duration::minutes(minute));
        }
        assert!(task.progress.done);
    }
    #[test]
    fn apply_persists_enrollment_before_cancellation_and_startup_resumes_it() {
        let (root, mut task, plan) = fixture();
        let control = Control::default();
        let (applied, _) = apply_names(&mut task, &plan, true, &control).unwrap();
        assert!(!applied.after.value.ktalk_sources.is_empty());
        assert!(read_progress(&task.meeting).unwrap().enrol_pending);
        control.cancel();
        assert!(enroll_plan(&task, &plan, &control).is_err());
        let picked = pickup(root.path(), Utc::now()).unwrap();
        assert_eq!(picked.len(), 1);
        assert!(picked[0].progress.done && picked[0].progress.enrol_pending);
        // A failed re-validation leaves the enrollment request durable.
        std::fs::write(task.meeting.system_audio_path(), b"audio exists").unwrap();
        let message = run(Job::Resume(task.clone(), settings()), &Store::default());
        assert!(matches!(message, Message::Enrolled { .. }));
        assert!(read_progress(&task.meeting).unwrap().enrol_pending);
        assert_eq!(
            pickup(root.path(), task.ended + chrono::Duration::hours(25))
                .unwrap()
                .len(),
            1
        );
        assert!(read_progress(&task.meeting).unwrap().enrol_pending);
    }
    #[test]
    fn exit_cancels_the_operation_and_waits_for_its_completion() {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.ktalk_job(Job::Check(settings()));
        let control = state.ktalk.cancel.clone();
        state.exit();
        assert!(control.token.is_cancelled());
        assert!(!state.can_exit());
        let job = commands
            .try_iter()
            .find_map(|command| {
                if let crate::worker::Command::Ktalk(job, control) = command {
                    Some((job, control))
                } else {
                    None
                }
            })
            .unwrap();
        state.ktalk_result(run_control(*job.0, &Store::default(), &job.1));
        assert!(state.can_exit());
    }
    #[test]
    fn manual_pending_enrollment_resumes_even_with_auto_names_disabled() {
        let (_root, mut task, plan) = fixture();
        apply_names(&mut task, &plan, true, &Control::default()).unwrap();
        task.progress
            .pending_enrollment
            .as_mut()
            .unwrap()
            .not_before = Utc::now() - chrono::Duration::seconds(1);
        persist(&task).unwrap();
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.settings.voice_library = true;
        let mut settings = settings();
        settings.set_auto_sync(false);
        state.ktalk.settings = Some(settings);
        state.ktalk.saved = true;
        state.ktalk.tasks.insert(task.meeting.path().into(), task);
        state.poll_ktalk_at(Moment {
            wall: Utc::now(),
            monotonic: state.ktalk.clock.started + Duration::from_secs(121),
        });
        assert!(commands.try_iter().any(|command| matches!(command, crate::worker::Command::Ktalk(job, _) if matches!(*job, Job::Resume(..)))));
    }
    #[test]
    fn ai_save_from_default_after_invalid_ai_keeps_the_valid_tolk_section() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path,"[defaults]\nlocal=123\n[ktalk]\nbase_url='https://neutral.ktalk.ru'\ncredential_id='neutral'\nunknown={nested=[1,2]}\n").unwrap();
        assert!(crate::config::LlmSettings::from_path(&path).is_err());
        let before = Settings::from_path(&path).unwrap().unwrap();
        let draft = crate::config::LlmSettings::default();
        notetaker_llm::config_write::save(
            &path,
            notetaker_llm::config_write::Section::Ai,
            toml::from_str(&draft.validate().unwrap()).unwrap(),
        )
        .unwrap();
        let after = Settings::from_path(&path).unwrap().unwrap();
        assert_eq!(before.extra, after.extra);
        assert_eq!(before.base_url, after.base_url);
        assert!(crate::config::LlmSettings::from_path(&path).is_ok());
    }
    fn fixture() -> (tempfile::TempDir, Task, Plan) {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let (transcript, plan) = crate::ktalk_fixture::plan();
        let mut meta =
            notetaker_core::MeetingMeta::new("Планирование", plan.recording.created_date);
        meta.title_source = Some(notetaker_core::TitleSource::Call);
        meta.ended_at = Some(meta.started_at + chrono::Duration::minutes(5));
        let ended = meta.ended_at.unwrap();
        let meeting = library.create_meeting(meta).unwrap();
        meeting.write_transcript(&transcript).unwrap();
        let mut registered = register_recording(&meeting).unwrap();
        registered.ended = ended;
        (
            root,
            Task {
                meeting,
                ended,
                progress: registered.progress,
            },
            plan,
        )
    }
    fn settings() -> Settings {
        Settings {
            base_url: "https://neutral.ktalk.ru".into(),
            credential_id: "neutral".into(),
            api_key_env: None,
            extra: Default::default(),
        }
    }
    #[test]
    fn schedule_points_stop_conditions_one_at_a_time_and_busy_retry() {
        let (_root, mut task, _) = fixture();
        let now = task.ended;
        let mut state = State {
            settings: Some(settings()),
            saved: true,
            ..Default::default()
        };
        state.tasks.insert(task.meeting.path().into(), task.clone());
        assert!(state.next(now, |_| false).is_some());
        assert!(
            state
                .next(now, |path| path == task.meeting.path())
                .is_none()
        );
        state.busy = true;
        assert!(state.next(now, |_| false).is_none());
        state.busy = false;
        for (at, next) in [(0, 3), (3, 10), (10, 30)] {
            task.after(&Outcome::Waiting, now + chrono::Duration::minutes(at));
            assert!(!task.progress.done);
            assert_eq!(
                task.progress.next,
                Some(now + chrono::Duration::minutes(next))
            );
        }
        task.after(
            &Outcome::Network("Нет сети".into()),
            now + chrono::Duration::minutes(30),
        );
        assert!(!task.progress.done);
        assert_eq!(
            task.progress.next,
            Some(now + chrono::Duration::minutes(31))
        );
        task.after(&Outcome::Busy, now);
        assert_eq!(
            task.progress.next,
            Some(now + chrono::Duration::seconds(15))
        );
        task.after(&Outcome::Waiting, now + chrono::Duration::minutes(30));
        assert!(task.progress.done);
        task.progress.done = false;
        task.after(&Outcome::Refused("сходство текста 0.20".into()), now);
        assert!(task.progress.done);
        assert_eq!(
            task.progress.reason.as_deref(),
            Some("сходство текста 0.20")
        );
        state.settings.as_mut().unwrap().set_auto_sync(false);
        assert!(state.next(now, |_| false).is_none());
        let ready = task.ended + chrono::Duration::minutes(5);
        task.progress = Progress {
            origin: Some(ready),
            ..Default::default()
        };
        task.after(&Outcome::Waiting, ready);
        assert_eq!(
            task.progress.next,
            Some(ready + chrono::Duration::minutes(3))
        );
    }
    #[test]
    fn startup_pickup_respects_sources_refusals_and_the_last_day() {
        let (root, mut task, plan) = fixture();
        let now = task.ended + chrono::Duration::seconds(1);
        assert_eq!(pickup(root.path(), now).unwrap().len(), 1);
        task.after(&Outcome::Refused("Неоднозначное совпадение".into()), now);
        persist(&task).unwrap();
        let loaded = pickup(root.path(), now).unwrap();
        assert!(loaded[0].progress.done);
        assert_eq!(loaded[0].progress.reason, task.progress.reason);
        assert!(
            pickup(root.path(), now + chrono::Duration::hours(25))
                .unwrap()
                .is_empty()
        );
        let library = Library::open(root.path()).unwrap();
        ktalk::apply(&library, &task.meeting, &plan).unwrap();
        assert!(pickup(root.path(), now).unwrap()[0].progress.done);
    }
    #[test]
    fn busy_meeting_lock_leaves_the_transcript_untouched_and_can_be_retried() {
        let (_root, mut task, plan) = fixture();
        let before = task.meeting.load_transcript().unwrap();
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(task.meeting.path().join(".notetaker.lock"))
            .unwrap();
        holder.try_lock().unwrap();
        let error = apply(&mut task, &plan, false, &Control::default())
            .err()
            .unwrap();
        assert!(timed_out(&error));
        assert_eq!(
            task.meeting.load_transcript().unwrap().revision,
            before.revision
        );
        let now = Utc::now();
        task.after(&Outcome::Busy, now);
        assert_eq!(
            task.progress.next,
            Some(now + chrono::Duration::seconds(15))
        );
        drop(holder);
        assert!(apply(&mut task, &plan, false, &Control::default()).is_ok());
    }
    #[test]
    fn explicit_people_merge_links_spellings_without_guessing() {
        let (root, task, _plan) = fixture();
        let target = notetaker_core::person_from_name("Анна Петрова").unwrap();
        let message = run(
            Job::Merge {
                root: root.path().into(),
                from: "Anna Petrova".into(),
                into: target.clone(),
            },
            &Store::default(),
        );
        assert!(matches!(message, Message::People(_)));
        let edits = Library::open(root.path())
            .unwrap()
            .read_people_edits()
            .unwrap();
        assert_eq!(edits.merges.get("name:anna petrova"), Some(&target.id));
        assert!(
            task.meeting
                .read_transcript()
                .unwrap()
                .ktalk_sources
                .is_empty()
        );
    }
    #[test]
    fn enable_voice_offer_uses_the_existing_setting_and_defers_enrollment_until_saved() {
        let (_root, mut task, plan) = fixture();
        let applied = apply(&mut task, &plan, false, &Control::default()).unwrap();
        let (mut state, commands, events) = crate::test_ui::state();
        state.ready = true;
        let path = task.meeting.path().to_path_buf();
        state
            .documents
            .insert(path.clone(), crate::state::Document::new(*applied.document));
        state.ktalk.tasks.insert(path.clone(), task);
        state.ktalk.inspected.insert(path.clone());
        let ctx = eframe::egui::Context::default();
        crate::theme::install(&ctx);
        crate::test_ui::click(
            &ctx,
            &mut |ui| transcript_ui(ui, &mut state, &path),
            "Включить",
        );
        assert!(state.settings_draft.voice_library);
        assert!(state.ktalk.enroll_pending.is_some());
        let (settings, llm, persist) = commands
            .try_iter()
            .find_map(|command| {
                if let crate::worker::Command::Settings {
                    settings,
                    llm,
                    persist_llm,
                    ..
                } = command
                {
                    Some((settings, llm, persist_llm))
                } else {
                    None
                }
            })
            .unwrap();
        events
            .send(crate::worker::Event::SettingsSaved(
                Ok((settings, llm)),
                persist,
            ))
            .unwrap();
        state.poll();
        assert!(commands.try_iter().any(|command|matches!(command,crate::worker::Command::Ktalk(job,_) if matches!(*job,Job::Enroll(_)))));
    }
    #[test]
    fn deleted_audio_is_skipped_only_after_the_undo_window() {
        let (_root, mut task, plan) = fixture();
        let applied = apply(&mut task, &plan, true, &Control::default()).unwrap();
        assert!(applied.segments >= 7);
        assert!(applied.voice_status.contains("2 минуты"));
        assert!(!applied.document.transcript.ktalk_sources.is_empty());
        task.progress
            .pending_enrollment
            .as_mut()
            .unwrap()
            .not_before = Utc::now() - chrono::Duration::seconds(1);
        persist(&task).unwrap();
        let reply = run(Job::Resume(task.clone(), settings()), &Store::default());
        assert!(
            matches!(reply, Message::Enrolled { status, .. } if status.contains("Аудио не хранится"))
        );
        assert!(!read_progress(&task.meeting).unwrap().enrol_pending);
        assert!(matches!(
            run(Job::Undo(applied.undo), &Store::default()),
            Message::Error(_)
        ));
    }
    #[derive(Default)]
    struct Store(std::sync::Mutex<BTreeMap<String, String>>);
    impl CredentialStore for Store {
        fn read(&self, id: &str) -> Result<String> {
            self.0.lock().unwrap().get(id).cloned().context("Нет ключа")
        }
        fn write(&self, id: &str, key: &str) -> Result<()> {
            self.0.lock().unwrap().insert(id.into(), key.into());
            Ok(())
        }
    }
    #[test]
    fn independent_settings_store_only_the_key_reference_and_load_without_ai() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let store = Store::default();
        let settings = settings();
        settings.save_key("synthetic-test-key", &store).unwrap();
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("synthetic-test-key"));
        assert!(!text.contains("providers"));
        assert!(!text.contains("defaults"));
        let loaded = Settings::from_path(&path).unwrap().unwrap();
        assert_eq!(loaded.base_url, settings.base_url);
        assert_eq!(store.read("ktalk/neutral").unwrap(), "synthetic-test-key");
        assert!(loaded.client(&store).is_ok());
        let before = std::fs::read(&path).unwrap();
        // The app's ordinary settings loader must leave a Tolk-only file untouched.
        assert!(crate::config::LlmSettings::from_path(&path).is_ok());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    #[test]
    fn settings_check_button_uses_the_saved_key_and_hides_its_value() {
        let mut state = State {
            settings: Some(settings()),
            saved: true,
            address: settings().base_url,
            ..Default::default()
        };
        let ctx = eframe::egui::Context::default();
        crate::theme::install(&ctx);
        let mut job = None;
        crate::test_ui::click(
            &ctx,
            &mut |ui| {
                if let Some(next) = settings_ui(ui, &mut state) {
                    job = Some(next);
                }
            },
            "Проверить",
        );
        assert!(matches!(job, Some(Job::Check(_))));
        state.busy = true;
        job = None;
        crate::test_ui::click(
            &ctx,
            &mut |ui| {
                if let Some(next) = settings_ui(ui, &mut state) {
                    job = Some(next);
                }
            },
            "Проверить",
        );
        assert!(job.is_none());
    }
    #[test]
    fn preview_apply_refusal_choices_source_and_the_once_only_voice_offer() {
        let (_root, task, plan) = fixture();
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        let path = task.meeting.path().to_path_buf();
        state.documents.insert(
            path.clone(),
            crate::state::Document::new(DocumentData::read(task.meeting.clone()).unwrap()),
        );
        state.ktalk.settings = Some(settings());
        state.ktalk.saved = true;
        state.ktalk.preview_open = true;
        state.ktalk.preview = Some((task.clone(), Outcome::Preview(Box::new(plan))));
        let ctx = eframe::egui::Context::default();
        crate::theme::install(&ctx);
        crate::test_ui::click(&ctx, &mut |ui| windows(ui.ctx(), &mut state), "Применить");
        let job = commands
            .try_iter()
            .find_map(|command| {
                if let crate::worker::Command::Ktalk(job, _) = command {
                    Some(job)
                } else {
                    None
                }
            })
            .unwrap();
        let message = run(*job, &Store::default());
        state.ktalk_result(message);
        assert!(
            !state.documents[&path]
                .data
                .transcript
                .ktalk_sources
                .is_empty()
        );
        let output = ctx.run_ui(Default::default(), |ui| {
            transcript_ui(ui, &mut state, &path)
        });
        let count=output.shapes.iter().filter(|shape|matches!(&shape.shape,eframe::egui::epaint::Shape::Text(text) if text.galley.text()=="Имена из Толка")).count();
        output.drop_without_applying_deltas();
        assert_eq!(count, 1);
        crate::test_ui::click(
            &ctx,
            &mut |ui| transcript_ui(ui, &mut state, &path),
            "Не сейчас",
        );
        assert!(state.ktalk.tasks[&path].progress.voice_offer_seen);
        let job = commands
            .try_iter()
            .find_map(|command| {
                if let crate::worker::Command::Ktalk(job, _) = command {
                    Some(job)
                } else {
                    None
                }
            })
            .unwrap();
        state.ktalk_result(run(*job, &Store::default()));
        assert!(read_progress(&task.meeting).unwrap().voice_offer_seen);
        state.ktalk.preview_open = true;
        state.ktalk.preview = Some((
            task.clone(),
            Outcome::Refused("сходство текста 0.20, подтверждено реплик 1 из 7".into()),
        ));
        for _ in 0..2 {
            ctx.run_ui(Default::default(), |ui| windows(ui.ctx(), &mut state))
                .drop_without_applying_deltas();
        }
        let output = ctx.run_ui(Default::default(), |ui| windows(ui.ctx(), &mut state));
        let visible=output.shapes.iter().any(|shape|matches!(&shape.shape,eframe::egui::epaint::Shape::Text(text) if text.galley.text().contains("0.20")));
        output.drop_without_applying_deltas();
        assert!(visible);
        let recording = Recording {
            id: "choice".into(),
            title: "Выбрать эту запись".into(),
            created_date: Utc::now(),
            duration: 300,
            status: "complete".into(),
        };
        state.ktalk.preview = Some((
            task,
            Outcome::Choices {
                reason: "Неоднозначное совпадение".into(),
                recordings: vec![recording],
            },
        ));
        // Choice is rendered, and an explicit ID goes through the same planner.
        for _ in 0..2 {
            ctx.run_ui(Default::default(), |ui| windows(ui.ctx(), &mut state))
                .drop_without_applying_deltas();
        }
        let output = ctx.run_ui(Default::default(), |ui| windows(ui.ctx(), &mut state));
        let visible=output.shapes.iter().any(|shape|matches!(&shape.shape,eframe::egui::epaint::Shape::Text(text) if text.galley.text().contains("Выбрать эту запись")));
        output.drop_without_applying_deltas();
        assert!(visible);
        state.ktalk_manual(&path, Some("choice".into()));
        assert!(commands.try_iter().any(|command|matches!(command,crate::worker::Command::Ktalk(job,_) if matches!(*job,Job::Sync {explicit:Some(ref id),..} if id=="choice"))));
    }
}
