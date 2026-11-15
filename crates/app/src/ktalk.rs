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
    time::Duration,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Progress {
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
        let origin = *self.progress.origin.get_or_insert(now);
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
            Outcome::Waiting | Outcome::Network(_) => {
                let next = [3, 10, 30]
                    .into_iter()
                    .map(|minutes| origin + chrono::Duration::minutes(minutes))
                    .find(|time| *time > now);
                if let Some(next) = next {
                    self.progress.next = Some(next);
                } else if matches!(outcome, Outcome::Network(_)) {
                    self.progress.next = Some(now + chrono::Duration::minutes(30));
                } else {
                    self.progress.done = true;
                }
            }
            _ => {}
        }
    }
}
#[derive(Clone)]
pub struct Undo {
    pub meeting: Meeting,
    pub before: Transcript,
    pub after: Revision,
}
pub struct Applied {
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
}
pub enum Job {
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
    Seen(Task),
    Inspect(Meeting),
    Merge {
        root: PathBuf,
        from: String,
        into: notetaker_core::Person,
    },
}
pub enum Message {
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
    Undone(Box<DocumentData>),
    Enrolled {
        path: PathBuf,
        status: String,
    },
    Error(String),
    Seen,
    Inspected(Task),
    People(Vec<notetaker_core::Person>),
}
#[derive(Default)]
pub struct State {
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
    pub load_root: Option<PathBuf>,
    pub address: String,
    key: String,
    replacing: bool,
    automatic_draft: Option<bool>,
    busy_path: Option<PathBuf>,
}
impl State {
    pub fn editing(&self, path: &Path) -> bool {
        self.busy_path.as_deref() == Some(path)
    }
    pub fn next(&self, now: DateTime<Utc>, blocked: impl Fn(&Path) -> bool) -> Option<Task> {
        if self.busy || !self.saved || !self.settings.as_ref().is_some_and(Settings::auto_sync) {
            return None;
        }
        self.tasks
            .values()
            .find(|task| {
                !task.progress.done
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
    crate::config::atomic_write(
        &journal(&task.meeting)?,
        &serde_json::to_string(&task.progress)?,
    )
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
pub fn pickup(root: &Path, now: DateTime<Utc>) -> Result<Vec<Task>> {
    let library = Library::open(root)?;
    let mut tasks = Vec::new();
    for meeting in library.list_meetings()? {
        let meta = meeting.read_meta()?;
        let Some(ended) = meta.ended_at else { continue };
        if meta.started_at < now - chrono::Duration::hours(24)
            || ended > now
            || (meta.title_source.is_none()
                && !meeting.mic_audio_path().is_file()
                && !meeting
                    .read_transcript()?
                    .segments
                    .iter()
                    .any(|segment| segment.channel == notetaker_core::Channel::Mic))
        {
            continue;
        }
        let progress = read_progress(&meeting)?;
        let mut task = Task {
            meeting,
            ended,
            progress,
        };
        if !task.meeting.read_transcript()?.ktalk_sources.is_empty() {
            task.progress.done = true;
        }
        tasks.push(task);
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
        tasks: pickup(root, Utc::now())?,
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
fn enroll(meeting: &Meeting) -> Result<String> {
    anyhow::ensure!(
        meeting.mic_audio_path().is_file() || meeting.system_audio_path().is_file(),
        "Аудио не хранится: голоса не запомнены"
    );
    let transcript = meeting.read_transcript()?;
    let config = notetaker_pipeline::AsrConfig::new("auto")?;
    let mut process = crate::worker::background_process(&config.binary);
    process.env(
        "NOTETAKER_MODELS_DIR",
        crate::config::Settings::load()?.models_path(),
    );
    let mut client =
        notetaker_asr_proto::AsrClient::spawn_command(process, Duration::from_secs(300))?;
    let count = notetaker_pipeline::remember_named_voices(meeting, &transcript, |wav, ranges| {
        Ok(client.embed(wav, ranges)?)
    })?;
    Ok(format!("Запомнено голосов: {count}"))
}
fn apply(task: &Task, plan: &Plan, voices: bool) -> Result<Applied> {
    let meeting = &task.meeting;
    let before = meeting.load_transcript()?;
    let library = Library::open(meeting.path().parent().context("Нет библиотеки")?)?;
    ktalk::apply(&library, meeting, plan)?;
    let after = meeting.load_transcript()?;
    let segments = after
        .value
        .segments
        .iter()
        .zip(&before.value.segments)
        .filter(|(a, b)| {
            a.speaker != b.speaker
                || after.value.speaker_name(&a.speaker.0) != before.value.speaker_name(&b.speaker.0)
        })
        .count();
    let voice_status = if voices {
        enroll(meeting).unwrap_or_else(|error| format!("Голоса: {error:#}"))
    } else {
        String::new()
    };
    Ok(Applied {
        document: Box::new(DocumentData::read(meeting.clone())?),
        undo: Undo {
            meeting: meeting.clone(),
            before: before.value,
            after: after.revision,
        },
        segments,
        recording: plan.recording.clone(),
        voice_status,
    })
}
fn sync(
    task: &Task,
    settings: &Settings,
    explicit: Option<&str>,
    automatic: bool,
    voices: bool,
    store: &dyn CredentialStore,
) -> Outcome {
    let client = match settings.client(store) {
        Ok(client) => client,
        Err(error) => return Outcome::Network(format!("{error:#}")),
    };
    let recording = match ktalk::select_recording(&client, &task.meeting, explicit) {
        Ok(recording) => recording,
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
                None => Outcome::Network(format!("{error:#}")),
            };
        }
    };
    let remote = match client.transcript(&recording.id) {
        Ok(remote) => remote,
        Err(error) if error.is::<ktalk::TranscriptNotReady>() => return Outcome::Waiting,
        Err(error) if error.is::<ktalk::InvalidTranscript>() => {
            return Outcome::Refused(error.to_string());
        }
        Err(error) => return Outcome::Network(format!("{error:#}")),
    };
    let owner = match client.profile() {
        Ok(owner) => owner,
        Err(error) => return Outcome::Network(format!("{error:#}")),
    };
    let result = (|| -> Result<Plan> {
        let local = task.meeting.read_transcript()?;
        ktalk::plan(
            &local,
            task.meeting.read_meta()?.started_at,
            recording,
            &remote,
            &owner,
        )
    })();
    let plan = match result {
        Ok(plan) => plan,
        Err(error) => return Outcome::Refused(format!("{error:#}")),
    };
    if !automatic {
        return Outcome::Preview(Box::new(plan));
    }
    match apply(task, &plan, voices) {
        Ok(applied) => Outcome::Applied(Box::new(applied)),
        Err(error) if timed_out(&error) => Outcome::Busy,
        Err(error) => Outcome::Refused(format!("{error:#}")),
    }
}
pub fn run(job: Job, store: &dyn CredentialStore) -> Message {
    let result = (|| -> Result<Message> {
        Ok(match job {
            Job::Load(root) => return load(&root, store),
            Job::Save { settings, key } => {
                settings.validate()?;
                if let Some(key) = key {
                    settings.save_key(&key, store)?;
                }
                settings.save_to(&notetaker_llm::Config::path()?)?;
                Message::Saved(settings)
            }
            Job::Check(settings) => Message::Checked(settings.client(store)?.profile()?.name()),
            Job::Sync {
                mut task,
                settings,
                explicit,
                automatic,
                voices,
            } => {
                let outcome = sync(
                    &task,
                    &settings,
                    explicit.as_deref(),
                    automatic,
                    voices,
                    store,
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
                let outcome = match apply(&task, &plan, voices) {
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
                undo.meeting.save_transcript(&undo.before, undo.after)?;
                Library::open(undo.meeting.path().parent().context("Нет библиотеки")?)?
                    .reindex_incremental()?;
                Message::Undone(Box::new(DocumentData::read(undo.meeting)?))
            }
            Job::Enroll(mut task) => {
                task.progress.voice_offer_seen = true;
                persist(&task)?;
                mark_offer_seen(&task)?;
                Message::Enrolled {
                    path: task.meeting.path().into(),
                    status: enroll(&task.meeting)
                        .unwrap_or_else(|error| format!("Голоса: {error:#}")),
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

pub fn settings_ui(ui: &mut eframe::egui::Ui, state: &mut State) -> Option<Job> {
    use crate::theme::{self, ButtonStyle};
    let mut job = None;
    ui.add_enabled_ui(!state.busy, |ui| {
        theme::settings_group(ui, "Пространство Толка", |ui| {
            ui.label("Адрес пространства");
            ui.add(
                theme::text_field(&mut state.address)
                    .hint_text("https://space.ktalk.ru")
                    .desired_width(ui.available_width()),
            );
            if state.saved && !state.replacing {
                ui.horizontal(|ui| {
                    ui.weak("Персональный ключ: сохранён");
                    if theme::button(ui, "Заменить", ButtonStyle::Ghost).clicked() {
                        state.replacing = true;
                    }
                });
            } else {
                ui.label("Персональный ключ");
                ui.add(
                    theme::text_field(&mut state.key)
                        .password(true)
                        .desired_width(ui.available_width()),
                );
            }
            ui.hyperlink_to(
                "Где взять ключ",
                "https://support.kontur.ru/talk/86797-personalnyj_apiklyuch_dostupa_v_tolke",
            );
            let mut settings = state.settings.clone().unwrap_or_else(|| Settings {
                base_url: String::new(),
                credential_id: "desktop".into(),
                api_key_env: None,
                extra: Default::default(),
            });
            settings.base_url = state.address.trim().into();
            let mut automatic = state
                .automatic_draft
                .unwrap_or_else(|| settings.auto_sync());
            theme::toggle(
                ui,
                &mut automatic,
                "Подписывать реплики именами участников после встречи",
            );
            settings.set_auto_sync(automatic);
            state.automatic_draft = Some(automatic);
            let valid = settings.validate();
            if let Err(error) = &valid
                && !state.address.is_empty()
            {
                ui.weak(error.to_string());
            }
            ui.horizontal_wrapped(|ui| {
                let same_origin = state
                    .settings
                    .as_ref()
                    .is_some_and(|old| old.base_url == settings.base_url);
                if ui
                    .add_enabled(
                        valid.is_ok() && (!state.key.is_empty() || state.saved && same_origin),
                        theme::styled_button(
                            theme::Palette::of(ui),
                            "Сохранить",
                            ButtonStyle::Primary,
                        ),
                    )
                    .clicked()
                {
                    let key = (!state.key.is_empty()).then(|| std::mem::take(&mut state.key));
                    if key.is_some() {
                        settings.api_key_env = None;
                    }
                    state.replacing = false;
                    job = Some(Job::Save {
                        settings: settings.clone(),
                        key,
                    });
                }
                if ui
                    .add_enabled(
                        state.saved && same_origin && valid.is_ok(),
                        theme::styled_button(
                            theme::Palette::of(ui),
                            "Проверить",
                            ButtonStyle::Secondary,
                        ),
                    )
                    .clicked()
                {
                    job = Some(Job::Check(settings));
                }
            });
        });
    });
    if state.busy {
        ui.spinner();
    }
    if !state.status.is_empty() {
        ui.label(&state.status);
    }
    theme::callout(ui, theme::icons::INFO, |ui| {
        ui.weak("Только чтение из Толка. Ничего из библиотеки не отправляется. Текст транскрипции Толка не сохраняется. Работает и для Local-встреч.");
    });
    job
}
pub fn merge_menu(ui: &mut eframe::egui::Ui, name: &str) {
    let people = ui
        .ctx()
        .data(|data| {
            data.get_temp::<Vec<notetaker_core::Person>>(eframe::egui::Id::unique("ktalk-people"))
        })
        .unwrap_or_default();
    crate::theme::select(("ktalk-person", name))
        .selected_text("Объединить с человеком…")
        .show_ui(ui, |ui| {
            for person in &people {
                if person.name == name {
                    continue;
                }
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
        });
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
                                ui.weak(format!(
                                    "Смещение: {:.1} с · Сходство текста: {:.2}",
                                    plan.offset_ms as f64 / 1000.0,
                                    plan.validation_score
                                ));
                                eframe::egui::Grid::new("ktalk-preview-table")
                                    .num_columns(4)
                                    .max_col_width((ui.available_width() - 150.0).max(120.0) / 2.0)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for label in
                                            ["Спикер у нас", "Участник", "Реплик", "Секунд"]
                                        {
                                            ui.weak(label);
                                        }
                                        ui.end_row();
                                        for row in &plan.rows {
                                            ui.label(&row.speaker);
                                            ui.vertical(|ui| {
                                                ui.label(&row.participant);
                                                ui.push_id(
                                                    (&row.speaker, &row.participant),
                                                    |ui| merge_menu(ui, &row.participant),
                                                );
                                            });
                                            ui.label(row.segments.to_string());
                                            ui.label(format!("{:.0}", row.seconds));
                                            ui.end_row();
                                        }
                                    });
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
    pub fn ktalk_job(&mut self, job: Job) {
        if self.ktalk.busy || self.settings_pending {
            return;
        }
        self.ktalk.busy = true;
        if let Job::Sync { task, .. } | Job::Apply { task, .. } = &job {
            self.ktalk
                .tasks
                .entry(task.meeting.path().into())
                .or_insert_with(|| task.clone());
        }
        self.ktalk.busy_path = match &job {
            Job::Sync { task, .. } | Job::Apply { task, .. } | Job::Enroll(task) => {
                Some(task.meeting.path().into())
            }
            Job::Undo(undo) => Some(undo.meeting.path().into()),
            _ => None,
        };
        self.send(crate::worker::Command::Ktalk(Box::new(job)));
    }
    pub fn ktalk_schedule(&mut self, path: &Path) {
        if let Some(document) = self.documents.get(path) {
            self.ktalk.tasks.entry(path.into()).or_insert_with(|| Task {
                meeting: document.data.meeting.clone(),
                ended: document.data.meta.ended_at.unwrap_or_else(Utc::now),
                progress: Progress {
                    origin: Some(Utc::now()),
                    ..Default::default()
                },
            });
        }
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
        if !self.ready
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
            && let Some(undo) = self.ktalk.undo_pending.take()
        {
            self.ktalk_job(Job::Undo(undo));
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
        let path = self.ktalk.busy_path.take();
        match message {
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
            Message::Undone(document) => {
                if let Some(doc) = self.documents.get_mut(document.meeting.path()) {
                    doc.data.transcript = document.transcript;
                    doc.data.revisions.transcript = document.revisions.transcript;
                    doc.speaker_ui.invalidate_transcript();
                }
                self.refresh();
            }
            Message::Enrolled { path, status } => {
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
        (
            root,
            Task {
                meeting,
                ended,
                progress: Default::default(),
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
            Some(now + chrono::Duration::minutes(60))
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
        let error = apply(&task, &plan, false).err().unwrap();
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
        assert!(apply(&task, &plan, false).is_ok());
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
        let (_root, task, plan) = fixture();
        let applied = apply(&task, &plan, false).unwrap();
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
        assert!(commands.try_iter().any(|command|matches!(command,crate::worker::Command::Ktalk(job) if matches!(*job,Job::Enroll(_)))));
    }
    #[test]
    fn applying_names_and_undo_restore_the_exact_transcript_and_deleted_audio_is_skipped() {
        let (_root, mut task, plan) = fixture();
        let original = task.meeting.read_transcript().unwrap();
        let applied = apply(&task, &plan, true).unwrap();
        assert!(applied.segments >= 7);
        assert!(applied.voice_status.contains("Аудио не хранится"));
        assert!(!applied.document.transcript.ktalk_sources.is_empty());
        task.after(&Outcome::Applied(Box::new(applied)), Utc::now());
        assert!(task.progress.done);
        let loaded = task.meeting.load_transcript().unwrap();
        let undo = Undo {
            meeting: task.meeting.clone(),
            before: original.clone(),
            after: loaded.revision,
        };
        assert!(matches!(
            run(Job::Undo(undo), &Store::default()),
            Message::Undone(_)
        ));
        assert_eq!(task.meeting.read_transcript().unwrap(), original);
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
                if let crate::worker::Command::Ktalk(job) = command {
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
                if let crate::worker::Command::Ktalk(job) = command {
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
        assert!(commands.try_iter().any(|command|matches!(command,crate::worker::Command::Ktalk(job) if matches!(*job,Job::Sync {explicit:Some(ref id),..} if id=="choice"))));
    }
}
