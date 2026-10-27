//! Blocking storage is serialized here; recording, LLM and downloads have their
//! own threads. The UI never waits for a worker or owns a RecordingSession.
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use chrono::Utc;
use notetaker_capture::{AudioSource, Inventory, LiveOptions, inventory, live_sources};
use notetaker_core::{Library, Meeting, MeetingMeta, Revision, Transcript};
use notetaker_llm::{
    Cancellation, Config, CredentialStore, OsCredentialStore, Template, create_provider,
    enhance_meeting,
};
use notetaker_pipeline::{
    Asr, AsrConfig, PipelineEvent, RecordingConfig, RecordingSession, StopHandle,
};

use crate::{
    attachments::{self, ImageInput, InsertAt},
    config::{LlmSettings, Settings, atomic_write},
};

pub struct MeetingRow {
    pub meeting: Meeting,
    pub meta: MeetingMeta,
    pub snippet: String,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct Revisions {
    pub notes: Revision,
    pub enhanced: Revision,
    pub transcript: Revision,
}
pub struct DocumentData {
    pub meeting: Meeting,
    pub meta: MeetingMeta,
    pub notes: String,
    pub enhanced: String,
    pub transcript: Transcript,
    pub revisions: Revisions,
}
impl DocumentData {
    pub fn read(meeting: Meeting) -> Result<Self> {
        let notes = meeting.load_notes()?;
        let enhanced = meeting.load_enhanced()?;
        let transcript = meeting.load_transcript()?;
        Ok(Self {
            meta: meeting.read_meta()?,
            notes: notes.value,
            enhanced: enhanced.value,
            transcript: transcript.value,
            revisions: Revisions {
                notes: notes.revision,
                enhanced: enhanced.revision,
                transcript: transcript.revision,
            },
            meeting,
        })
    }
}
#[derive(Clone, Copy, Default)]
pub struct Fields {
    pub notes: bool,
    pub enhanced: bool,
    pub transcript: bool,
}
impl Fields {
    pub fn any(self) -> bool {
        self.notes || self.enhanced || self.transcript
    }
}
pub struct Save {
    pub meeting: Meeting,
    pub notes: Option<String>,
    pub enhanced: Option<String>,
    pub transcript: Option<Transcript>,
    pub revisions: Revisions,
}
#[derive(Default)]
pub struct SaveResult {
    pub revisions: Revisions,
    pub failed: Fields,
    pub conflicts: Fields,
    pub errors: Vec<String>,
    pub reloaded: Option<Box<DocumentData>>,
}
impl Save {
    fn write(&self) -> SaveResult {
        let mut saved = SaveResult {
            revisions: self.revisions,
            ..SaveResult::default()
        };
        // Attempt every dirty field, preserving each conflicting local version.
        let results = [
            self.notes
                .as_ref()
                .map(|text| self.meeting.save_notes(text, self.revisions.notes)),
            self.enhanced
                .as_ref()
                .map(|text| self.meeting.save_enhanced(text, self.revisions.enhanced)),
            self.transcript.as_ref().map(|text| {
                self.meeting
                    .save_transcript(text, self.revisions.transcript)
            }),
        ];
        for (result, (revision, failed, conflict)) in results.into_iter().zip([
            (
                &mut saved.revisions.notes,
                &mut saved.failed.notes,
                &mut saved.conflicts.notes,
            ),
            (
                &mut saved.revisions.enhanced,
                &mut saved.failed.enhanced,
                &mut saved.conflicts.enhanced,
            ),
            (
                &mut saved.revisions.transcript,
                &mut saved.failed.transcript,
                &mut saved.conflicts.transcript,
            ),
        ]) {
            match result {
                Some(Ok(value)) => *revision = value,
                Some(Err(error)) => {
                    *failed = true;
                    *conflict = matches!(error, notetaker_core::Error::Conflict { .. });
                    saved.errors.push(error.to_string());
                }
                None => {}
            }
        }
        if saved.conflicts.any() {
            match DocumentData::read(self.meeting.clone()) {
                Ok(data) => saved.reloaded = Some(Box::new(data)),
                Err(error) => saved
                    .errors
                    .push(format!("Reload after conflict: {error:#}")),
            }
        }
        saved
    }
}
#[derive(Clone)]
pub struct RecordRequest {
    pub settings: Settings,
    pub title: String,
    pub brief: Option<Box<notetaker_llm::Brief>>,
    pub event: Option<notetaker_core::calendar::CalendarEvent>,
    pub process_id: Option<u32>,
    pub app: Option<String>,
}

pub enum Command {
    Tasks {
        root: PathBuf,
    },
    ToggleTask {
        meeting: Meeting,
        task: notetaker_core::Task,
        completed: bool,
    },
    Brief {
        root: PathBuf,
        event: notetaker_core::calendar::CalendarEvent,
        cancel: Cancellation,
    },
    Followup {
        meeting: Meeting,
        kind: notetaker_llm::FollowupKind,
        cancel: Cancellation,
    },
    Calendar {
        subscriptions: Vec<notetaker_llm::calendar::Subscription>,
        timezone: String,
        generation: u64,
    },
    Chat {
        root: PathBuf,
        scope: Option<Meeting>,
        question: String,
        history: Vec<notetaker_llm::ChatTurn>,
        cancel: Cancellation,
    },
    Ai(crate::ai::Action),
    Attach {
        meeting: Meeting,
        inputs: Vec<ImageInput>,
        anchor: InsertAt,
    },
    Reveal(PathBuf),
    Export {
        meeting: Meeting,
        text: String,
    },
    List {
        root: PathBuf,
        query: String,
        request: u64,
    },
    Load(Meeting),
    Save(Box<Save>),
    EditSpeakers {
        meeting: Meeting,
        expected: Transcript,
        edit: notetaker_core::SpeakerEdit,
    },
    ExportTranscript {
        meeting: Meeting,
        transcript: Transcript,
        format: notetaker_core::ExportFormat,
    },
    Inventory,
    Record(RecordRequest),
    Import {
        settings: Settings,
        file: PathBuf,
    },
    Enhance {
        meeting: Meeting,
        template: String,
        provider: notetaker_llm::ProviderConfig,
        cancel: Cancellation,
        automatic: bool,
    },
    Download(String),
    Voices {
        root: PathBuf,
        delete: Option<String>,
    },
    Settings {
        settings: Settings,
        llm: LlmSettings,
        calendar_secrets: Vec<(String, String)>,
    },
}
// Rare worker-to-UI messages; boxing variants buys nothing measurable.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Tasks {
        root: PathBuf,
        result: Result<Vec<notetaker_core::Task>, String>,
    },
    TaskToggled {
        path: PathBuf,
        result: Result<Box<DocumentData>, String>,
    },
    Brief(Result<notetaker_llm::Brief, String>),
    Followup {
        path: PathBuf,
        result: Result<String, String>,
    },
    Calendar {
        generation: u64,
        result: Result<Vec<notetaker_core::calendar::CalendarEvent>, String>,
    },
    ChatChunk(String),
    ChatDone(Result<notetaker_llm::ChatResult, String>),
    Ai(crate::ai::Response),
    Attached {
        path: PathBuf,
        anchor: InsertAt,
        results: Vec<Result<String, String>>,
    },
    Exported(Result<PathBuf, String>),
    Revealed(Result<(), String>),
    Boot {
        settings: Settings,
        llm: LlmSettings,
        warnings: Vec<String>,
        needs_model: bool,
        llm_configured: bool,
    },
    Listed {
        request: u64,
        result: Result<Vec<MeetingRow>, String>,
        templates: Vec<String>,
    },
    Loaded {
        path: PathBuf,
        result: Result<DocumentData, String>,
    },
    Saved {
        path: PathBuf,
        result: SaveResult,
    },
    SpeakersEdited {
        path: PathBuf,
        result: Result<notetaker_core::SpeakerEditResult, String>,
        data: Option<Box<DocumentData>>,
    },
    Inventory(Result<Inventory, String>),
    Started {
        importing: bool,
        document: Box<DocumentData>,
        stop: StopHandle,
    },
    Pipeline(PipelineEvent),
    RecordDone {
        path: Option<PathBuf>,
        document: Option<Box<DocumentData>>,
        error: Option<String>,
    },
    ReloadEnhanced {
        path: PathBuf,
        value: notetaker_core::Versioned<String>,
    },
    EnhancedChunk {
        path: PathBuf,
        text: String,
    },
    Enhanced {
        path: PathBuf,
        result: Result<notetaker_llm::EnhanceResult, String>,
    },
    Voices(Result<notetaker_core::VoiceLibrary, String>),
    DownloadProgress(String),
    Downloaded(Result<(), String>),
    SettingsSaved(Result<(Settings, LlmSettings), String>),
}

pub fn spawn() -> (Sender<Command>, Receiver<Event>) {
    let (commands, rx) = mpsc::channel();
    let (events, messages) = mpsc::channel();
    thread::spawn(move || {
        let mut warnings = Vec::new();
        let settings = Settings::load().unwrap_or_else(|error| {
            warnings.push(format!("{error:#}"));
            Settings::default()
        });
        let llm = LlmSettings::load().unwrap_or_else(|error| {
            warnings.push(format!("{error:#}"));
            LlmSettings::default()
        });
        if let Err(error) =
            Library::open(&settings.library).and_then(|library| library.sweep_audio(Utc::now()))
        {
            warnings.push(format!("Не удалось очистить старые файлы: {error:#}"));
        }
        // Reuse the worker's model manifest and path resolution; do not load
        // hundreds of MB of weights just to check first-run readiness.
        let needs_model = !crate::config::MODELS.iter().any(|model| {
            asr_binary().is_ok_and(|binary| {
                background_process(&binary)
                    .args(["check-model", model])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            })
        });
        if settings.diarize
            && !asr_binary().is_ok_and(|binary| {
                background_process(&binary)
                    .args(["check-model", "diarization"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
        {
            warnings.push("Для разделения собеседников скачайте модели в настройках. Транскрипт работает и без них.".into());
        }
        let mut voice_library_enabled = settings.voice_library;
        let _ = events.send(Event::Voices(
            Library::open(&settings.library)
                .and_then(|l| l.read_voices())
                .map_err(|e| e.to_string()),
        ));
        let llm_configured = Config::load().is_ok();
        let _ = events.send(Event::Boot {
            settings,
            llm,
            warnings,
            needs_model,
            llm_configured,
        });
        for command in rx {
            match command {
                Command::Tasks { root } => {
                    let result = (|| -> Result<_> {
                        let library = Library::open(&root)?;
                        library.reindex_incremental()?;
                        Ok(library.list_tasks(None, None, false)?)
                    })()
                    .map_err(error_text);
                    let _ = events.send(Event::Tasks { root, result });
                }
                Command::ToggleTask {
                    meeting,
                    task,
                    completed,
                } => {
                    let path = meeting.path().to_owned();
                    let result = (|| -> Result<_> {
                        meeting.toggle_task(&task, completed)?;
                        Ok(Box::new(DocumentData::read(meeting)?))
                    })()
                    .map_err(error_text);
                    let _ = events.send(Event::TaskToggled { path, result });
                }
                Command::Brief {
                    root,
                    event,
                    cancel,
                } => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let result = (|| -> Result<_> {
                            let library = Library::open(root)?;
                            let config = Config::load().ok();
                            notetaker_llm::brief(&library, &event, config.as_ref(), cancel)
                        })()
                        .map_err(error_text);
                        let _ = events.send(Event::Brief(result));
                    });
                }
                Command::Followup {
                    meeting,
                    kind,
                    cancel,
                } => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let path = meeting.path().to_owned();
                        let result = Config::load()
                            .and_then(|config| {
                                notetaker_llm::followup(&meeting, kind, &config, cancel)
                            })
                            .map_err(error_text);
                        let _ = events.send(Event::Followup { path, result });
                    });
                }

                Command::Calendar {
                    subscriptions,
                    timezone,
                    generation,
                } => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let result = notetaker_llm::calendar::refresh(
                            &subscriptions,
                            &timezone,
                            Utc::now(),
                            &OsCredentialStore,
                        )
                        .map_err(error_text);
                        let _ = events.send(Event::Calendar { generation, result });
                    });
                }
                Command::Chat {
                    root,
                    scope,
                    question,
                    history,
                    cancel,
                } => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let result = (|| -> Result<_> {
                            let library = Library::open(root)?;
                            let config = Config::load()?;
                            notetaker_llm::ask(
                                &library,
                                scope.as_ref(),
                                &question,
                                &history,
                                &config,
                                cancel,
                                &mut |text| {
                                    let _ = events.send(Event::ChatChunk(text.into()));
                                },
                            )
                        })()
                        .map_err(error_text);
                        let _ = events.send(Event::ChatDone(result));
                    });
                }
                Command::Ai(action) => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let _ = events.send(Event::Ai(crate::ai::execute(action)));
                    });
                }
                Command::Attach {
                    meeting,
                    inputs,
                    anchor,
                } => {
                    let results = inputs
                        .into_iter()
                        .map(|input| attachments::import(&meeting, input).map_err(error_text))
                        .collect();
                    let _ = events.send(Event::Attached {
                        path: meeting.path().to_owned(),
                        anchor,
                        results,
                    });
                }
                Command::Reveal(path) => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let _ =
                            events.send(Event::Revealed(reveal_file(&path).map_err(error_text)));
                    });
                }
                Command::Export { meeting, text } => {
                    let _ = events.send(Event::Exported(
                        meeting.export_summary(&text).map_err(|e| e.to_string()),
                    ));
                }
                Command::List {
                    root,
                    query,
                    request,
                } => {
                    let templates = templates(&root);
                    let result = list(&root, &query).map_err(error_text);
                    let _ = events.send(Event::Listed {
                        request,
                        result,
                        templates,
                    });
                }
                Command::Load(meeting) => {
                    let path = meeting.path().to_owned();
                    let _ = events.send(Event::Loaded {
                        path,
                        result: (|| -> Result<_> {
                            default_self_name(&meeting, &Settings::load()?.user_name)?;
                            DocumentData::read(meeting)
                        })()
                        .map_err(error_text),
                    });
                }
                Command::Voices { root, delete } => {
                    let result = (|| -> Result<_> {
                        let library = Library::open(root)?;
                        if let Some(name) = delete {
                            library.delete_voice(&name)?;
                        }
                        Ok(library.read_voices()?)
                    })()
                    .map_err(error_text);
                    let _ = events.send(Event::Voices(result));
                }
                Command::Save(save) => {
                    let mut result = save.write();
                    if save.transcript.is_some()
                        && !result.failed.transcript
                        && let Err(error) =
                            Library::open(save.meeting.path().parent().expect("meeting parent"))
                                .and_then(|library| library.reindex_incremental())
                    {
                        result.errors.push(format!(
                            "Имена сохранены; не обновлён индекс поиска: {error:#}"
                        ));
                    }
                    let _ = events.send(Event::Saved {
                        path: save.meeting.path().to_owned(),
                        result,
                    });
                }
                Command::EditSpeakers {
                    meeting,
                    expected,
                    edit,
                } => {
                    let library = Library::open(meeting.path().parent().expect("meeting parent"));
                    let result = library
                        .as_ref()
                        .map_err(|e| e.to_string())
                        .and_then(|library| {
                            library
                                .edit_speakers(&meeting, &expected, edit, voice_library_enabled)
                                .map_err(|e| e.to_string())
                        });
                    let data = DocumentData::read(meeting.clone()).ok().map(Box::new);
                    if voice_library_enabled && let Ok(library) = library {
                        let _ = events.send(Event::Voices(
                            library.read_voices().map_err(|e| e.to_string()),
                        ));
                    }
                    let _ = events.send(Event::SpeakersEdited {
                        path: meeting.path().to_owned(),
                        result,
                        data,
                    });
                }
                Command::ExportTranscript {
                    meeting,
                    transcript,
                    format,
                } => {
                    let result = meeting
                        .export_transcript(&transcript, format)
                        .map_err(|e| e.to_string());
                    let _ = events.send(Event::Exported(result));
                }
                Command::Inventory => {
                    let _ = events.send(Event::Inventory(inventory().map_err(error_text)));
                }
                Command::Record(request) => {
                    let events = events.clone();
                    thread::spawn(move || record(request, None, &events));
                }
                Command::Import { settings, file } => {
                    let events = events.clone();
                    thread::spawn(move || {
                        record(
                            RecordRequest {
                                settings,
                                title: String::new(),
                                brief: None,
                                event: None,
                                process_id: None,
                                app: None,
                            },
                            Some(file),
                            &events,
                        )
                    });
                }
                Command::Enhance {
                    meeting,
                    template,
                    provider,
                    cancel,
                    automatic,
                } => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let path = meeting.path().to_owned();
                        let result: Result<_> = (|| {
                            default_self_name(&meeting, &Settings::load()?.user_name)?;
                            let meta = meeting.read_meta()?;
                            let root = path.parent().context("Нет библиотеки")?;
                            let template_data = Template::load(root, &template)?;
                            let provider = create_provider(provider, meta.mode, cancel)?;
                            let enhance = if automatic {
                                notetaker_llm::enhance_meeting_if_unedited
                            } else {
                                enhance_meeting
                            };
                            let result = enhance(
                                &meeting,
                                &template,
                                &template_data,
                                provider.as_ref(),
                                &mut |text| {
                                    let _ = events.send(Event::EnhancedChunk {
                                        path: path.clone(),
                                        text: text.into(),
                                    });
                                },
                            )?;
                            Ok(result)
                        })();
                        if result.as_ref().err().is_some_and(|error| {
                            matches!(
                                error.downcast_ref::<notetaker_core::Error>(),
                                Some(notetaker_core::Error::Conflict { .. })
                            )
                        }) && let Ok(value) = meeting.load_enhanced()
                        {
                            let _ = events.send(Event::ReloadEnhanced {
                                path: path.clone(),
                                value,
                            });
                        }
                        let _ = events.send(Event::Enhanced {
                            path,
                            result: result.map_err(error_text),
                        });
                    });
                }
                Command::Download(model) => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let result = download(&model, &events).map_err(error_text);
                        let _ = events.send(Event::Downloaded(result));
                    });
                }
                Command::Settings {
                    settings,
                    llm,
                    calendar_secrets,
                } => {
                    let result = (|| {
                        settings.validate()?;
                        let llm_text = llm.validate()?;
                        let previous = Settings::load().unwrap_or_default();
                        for (id, url) in &calendar_secrets {
                            let subscription = settings
                                .calendars
                                .iter()
                                .find(|s| &s.id == id)
                                .context("Нет календаря для URL")?;
                            subscription.save_url(url, &OsCredentialStore)?;
                        }
                        for subscription in &settings.calendars {
                            OsCredentialStore
                                .read(&subscription.id)
                                .context("Укажите приватный URL календаря")?;
                        }
                        Library::open(&settings.library)?;
                        atomic_write(&notetaker_llm::Config::path()?, &llm_text)?;
                        atomic_write(&Settings::path()?, &toml::to_string_pretty(&settings)?)?;
                        for subscription in &previous.calendars {
                            if !settings.calendars.iter().any(|s| s.id == subscription.id) {
                                let _ = OsCredentialStore.delete(&subscription.id);
                            }
                        }
                        voice_library_enabled = settings.voice_library;
                        let _ = events.send(Event::Voices(
                            Library::open(&settings.library)
                                .and_then(|l| l.read_voices())
                                .map_err(|e| e.to_string()),
                        ));
                        Ok((settings, llm))
                    })()
                    .map_err(error_text);
                    let _ = events.send(Event::SettingsSaved(result));
                }
            }
        }
    });
    (commands, messages)
}

pub fn summary_can_overwrite(data: &DocumentData) -> bool {
    data.meta
        .summary
        .as_ref()
        .map_or(data.enhanced.is_empty(), |stamp| {
            !stamp.edited(&data.enhanced)
        })
}

fn error_text(error: anyhow::Error) -> String {
    notetaker_core::display_text(&format!("{error:#}"))
}
fn list(root: &Path, query: &str) -> Result<Vec<MeetingRow>> {
    let library = Library::open(root)?;
    let mut rows = Vec::new();
    let hits = if query.trim().is_empty() {
        None
    } else {
        library.reindex_incremental()?;
        Some(library.search(query, 500)?)
    };
    for meeting in library.list_meetings()? {
        let meta = meeting.read_meta()?;
        let snippet = if let Some(hits) = &hits {
            let Some(hit) = hits.iter().find(|hit| hit.meeting_id == meta.id) else {
                continue;
            };
            hit.snippet.clone()
        } else {
            String::new()
        };
        rows.push(MeetingRow {
            meeting,
            meta,
            snippet,
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.meta.started_at));
    Ok(rows)
}
fn templates(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = ["general", "one-on-one", "client-call", "standup"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Ok(entries) = fs::read_dir(root.join(".templates")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md")
                && let Some(name) = path.file_stem().and_then(|name| name.to_str())
                && !name.starts_with("followup-")
                && Template::load(root, name).is_ok()
                && !names.iter().any(|n| n == name)
            {
                names.push(name.into());
            }
        }
    }
    names
}

// Preload once before opening capture. Own the protocol client here so Windows
// worker processes can use CREATE_NO_WINDOW; no console flashes over a call.
// The pipeline still controls retries through Asr::start.
struct AppAsr {
    config: AsrConfig,
    client: Option<notetaker_asr_proto::AsrClient>,
    preloaded: bool,
    request: u64,
}
impl Asr for AppAsr {
    fn set_timeout(&mut self, timeout: Duration) {
        self.config.timeout = self.config.timeout.max(timeout);
        if let Some(client) = &mut self.client {
            client.set_timeout(self.config.timeout);
        }
    }
    fn cancellation_handle(&self) -> Option<notetaker_asr_proto::CancellationHandle> {
        self.client
            .as_ref()
            .map(notetaker_asr_proto::AsrClient::cancellation_handle)
    }
    fn start(&mut self) -> Result<()> {
        if self.preloaded {
            self.preloaded = false;
            return Ok(());
        }
        self.client.take();
        let mut client = notetaker_asr_proto::AsrClient::spawn_command(
            background_process(&self.config.binary),
            self.config.timeout,
        )?;
        client.load_model_with_hotwords(
            &self.config.model,
            self.config.provider,
            &self.config.vocabulary.hotwords(),
        )?;
        self.client = Some(client);
        Ok(())
    }
    fn diarize(
        &mut self,
        wav: &Path,
        speakers: Option<u32>,
        embeddings: bool,
    ) -> Result<notetaker_asr_proto::Diarization> {
        Ok(self
            .client
            .as_mut()
            .context("ASR не запущен")?
            .diarize(wav, "system", speakers, embeddings)?)
    }
    fn embed(
        &mut self,
        wav: &Path,
        ranges: Vec<notetaker_asr_proto::TimeRange>,
    ) -> Result<Vec<f32>> {
        Ok(self
            .client
            .as_mut()
            .context("ASR не запущен")?
            .embed(wav, ranges)?)
    }
    fn transcribe(
        &mut self,
        input: notetaker_asr_proto::AudioInput,
        channel: notetaker_capture::Channel,
        offset_ms: u64,
    ) -> Result<Vec<notetaker_asr_proto::Segment>> {
        self.transcribe_with_progress(input, channel, offset_ms, &mut |_| {})
    }
    fn transcribe_with_progress(
        &mut self,
        input: notetaker_asr_proto::AudioInput,
        channel: notetaker_capture::Channel,
        offset_ms: u64,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<notetaker_asr_proto::Segment>> {
        self.request += 1;
        let channel = match channel {
            notetaker_capture::Channel::Mic => "mic",
            notetaker_capture::Channel::System => "system",
        };
        let mut segments = self
            .client
            .as_mut()
            .context("ASR не запущен")?
            .transcribe_with_progress(
                &self.request.to_string(),
                input,
                channel,
                offset_ms,
                progress,
            )?;
        for segment in &mut segments {
            segment.text = self.config.vocabulary.correct(&segment.text);
        }
        Ok(segments)
    }
}
fn background_process(binary: &Path) -> Process {
    let mut command = Process::new(binary);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}
fn record(request: RecordRequest, import_file: Option<PathBuf>, events: &Sender<Event>) {
    let mut path = None;
    let mut meeting_for_reload = None;
    let result: Result<()> = (|| {
        let binary = asr_binary()?;
        let mut config = AsrConfig::new(&request.settings.model)?;
        config.binary = binary;
        config.vocabulary = notetaker_core::Vocabulary::with_library(
            &request.settings.vocabulary,
            &request.settings.library,
        )?;
        let mut asr = AppAsr {
            config,
            client: None,
            preloaded: false,
            request: 0,
        };
        asr.start()
            .context("Модель ASR недоступна. Откройте настройки и нажмите «Скачать»")?;
        asr.preloaded = true;
        let library = Library::open(&request.settings.library)?;
        let config = RecordingConfig {
            diarization: notetaker_pipeline::DiarizationConfig {
                enabled: request.settings.diarize,
                speakers: request.settings.speakers,
                voice_library: request.settings.voice_library,
            },
            retention: request
                .settings
                .retention_days
                .map(|days| Duration::from_secs(u64::from(days) * 86400)),
            auto_stop_after_silence: (request.settings.auto_stop_minutes > 0)
                .then(|| Duration::from_secs(u64::from(request.settings.auto_stop_minutes) * 60)),
            ..RecordingConfig::default()
        };
        let session = if let Some(file) = &import_file {
            RecordingSession::import_with_asr(
                library.clone(),
                file,
                request.settings.mode,
                asr,
                config,
            )?
        } else {
            let [mic, system] = live_sources(LiveOptions {
                process_id: request.process_id,
                ..LiveOptions::default()
            })?;
            let sources: [Box<dyn AudioSource + Send>; 2] = [Box::new(mic), Box::new(system)];
            let mut meta = MeetingMeta::new(request.title, Utc::now());
            meta.mode = if request
                .brief
                .as_ref()
                .is_some_and(|brief| brief.mode == notetaker_core::Mode::Local)
            {
                notetaker_core::Mode::Local
            } else {
                request.settings.mode
            };
            meta.app = request.app;
            if let Some(event) = &request.event {
                notetaker_core::calendar::link_meeting(&mut meta, event, true);
            }
            RecordingSession::start_with_asr(library.clone(), meta, sources, asr, config)?
        };
        let meeting = session.meeting().clone();
        if let Some(brief) = &request.brief {
            let notes = meeting.load_notes()?;
            let text = format!(
                "{}\n<details>\n<summary>Бриф перед встречей</summary>\n\n{}\n\n</details>\n",
                notes.value, brief.text
            );
            meeting.save_notes(&text, notes.revision)?;
        }

        path = Some(meeting.path().to_owned());
        meeting_for_reload = Some(meeting.clone());
        let document = DocumentData::read(meeting)?;
        if events
            .send(Event::Started {
                importing: import_file.is_some(),
                document: Box::new(document),
                stop: session.stop_handle(),
            })
            .is_err()
        {
            session.stop();
        }
        for event in session.events() {
            let finished = matches!(event, PipelineEvent::Finished { .. });
            let _ = events.send(Event::Pipeline(event));
            if finished {
                break;
            }
        }
        session.wait()?;
        default_self_name(
            &meeting_for_reload.clone().context("Нет встречи")?,
            &request.settings.user_name,
        )?;
        library.reindex_incremental()?;
        Ok(())
    })();
    let document = meeting_for_reload
        .and_then(|meeting| DocumentData::read(meeting).ok())
        .map(Box::new);
    let _ = events.send(Event::RecordDone {
        path,
        document,
        error: result.err().map(error_text),
    });
}

fn reveal_file(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Process::new("explorer.exe");
        command.arg(format!("/select,{}", notetaker_core::display_path(path)));
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        #[cfg(target_os = "macos")]
        let mut command = Process::new("open");
        #[cfg(not(target_os = "macos"))]
        let mut command = Process::new("xdg-open");
        command.arg(path.parent().context("Нет папки файла")?);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Не удалось открыть папку")?
        .wait()?;
    Ok(())
}

fn default_self_name(meeting: &Meeting, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Ok(());
    }
    let loaded = meeting.load_transcript()?;
    let mut transcript = loaded.value;
    if transcript.default_self_name(name) {
        meeting.save_transcript(&transcript, loaded.revision)?;
    }
    Ok(())
}

pub fn asr_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NOTETAKER_ASR_BIN") {
        let path = PathBuf::from(path);
        ensure!(path.is_file(), "NOTETAKER_ASR_BIN не указывает на файл");
        return Ok(path);
    }
    let path = notetaker_runtime::paths::current_sibling(&format!(
        "notetaker-asr{}",
        std::env::consts::EXE_SUFFIX
    ))?;
    ensure!(
        path.is_file(),
        "Не найден {}. Поместите notetaker-asr рядом с приложением (соберите notetaker-asr-worker).",
        notetaker_core::display_path(path)
    );
    Ok(path)
}
fn download(model: &str, events: &Sender<Event>) -> Result<()> {
    let mut process = background_process(&asr_binary()?)
        .args(["download", model])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = process.stderr.take().context("Нет stderr загрузчика")?;
    let read_result = (|| -> Result<()> {
        for line in BufReader::new(stderr).lines() {
            let line = line?;
            notetaker_runtime::diagnostics::worker_stderr(&line);
            let _ = events.send(Event::DownloadProgress(line));
        }
        Ok(())
    })();
    let status = process.wait()?;
    read_result?;
    ensure!(
        status.success(),
        "Загрузка завершилась с ошибкой ({status}); подробности выше"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_self_name_does_not_replace_confirmed_name() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Names", Utc::now()))
            .unwrap();
        default_self_name(&meeting, "Иван").unwrap();
        assert_eq!(
            meeting.read_transcript().unwrap().speaker_names["me"],
            "Иван"
        );
        default_self_name(&meeting, "Игорь").unwrap();
        assert_eq!(
            meeting.read_transcript().unwrap().speaker_names["me"],
            "Иван"
        );
    }
    #[test]
    fn stale_ui_save_keeps_external_files_and_reloads_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Concurrent edit", Utc::now()))
            .unwrap();
        let loaded = DocumentData::read(meeting.clone()).unwrap();
        meeting.write_notes("external").unwrap();
        let saved = Save {
            meeting: meeting.clone(),
            revisions: loaded.revisions,
            notes: Some("local".into()),
            enhanced: Some("summary".into()),
            transcript: None,
        }
        .write();
        assert!(saved.conflicts.notes);
        assert!(!saved.failed.enhanced);
        assert_eq!(saved.reloaded.unwrap().notes, "external");
        assert_eq!(meeting.read_notes().unwrap(), "external");
        assert_eq!(meeting.read_enhanced().unwrap(), "summary");
        let conflict = fs::read_dir(meeting.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains("notes.conflict-"))
            .unwrap();
        assert_eq!(fs::read_to_string(conflict).unwrap(), "local");
    }
    #[test]
    fn meeting_list_does_not_require_transcripts_or_a_working_index() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Visible", Utc::now()))
            .unwrap();
        fs::write(meeting.path().join("transcript.json"), "invalid JSON").unwrap();
        // A directory here makes opening/removing the index impossible.
        fs::create_dir_all(library.index_path()).unwrap();
        let rows = list(root.path(), "").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].meta.title, "Visible");
    }
    #[test]
    fn storage_save_search_and_reopen() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Созвон", Utc::now()))
            .unwrap();
        let revisions = DocumentData::read(meeting.clone()).unwrap().revisions;
        let saved = Save {
            revisions,
            meeting: meeting.clone(),
            notes: Some("Обсудить Kubernetes".into()),
            enhanced: None,
            transcript: None,
        }
        .write();
        assert!(saved.errors.is_empty());
        assert_eq!(list(root.path(), "Kuber").unwrap().len(), 1);
        let loaded = DocumentData::read(meeting).unwrap();
        assert_eq!(loaded.notes, "Обсудить Kubernetes");
        assert!(loaded.enhanced.is_empty());
        assert!(list(root.path(), "несуществующее").unwrap().is_empty());
    }
}
