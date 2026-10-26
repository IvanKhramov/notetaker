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
use notetaker_core::{Library, Meeting, MeetingMeta, Transcript};
use notetaker_llm::{Config, HttpProvider, Template, enhance_meeting};
use notetaker_pipeline::{
    Asr, AsrConfig, PipelineEvent, RecordingConfig, RecordingSession, StopHandle,
};

use crate::config::{LlmSettings, Settings, atomic_write};

pub struct MeetingRow {
    pub meeting: Meeting,
    pub meta: MeetingMeta,
    pub snippet: String,
}
pub struct DocumentData {
    pub meeting: Meeting,
    pub meta: MeetingMeta,
    pub notes: String,
    pub enhanced: String,
    pub transcript: Transcript,
}
impl DocumentData {
    pub fn read(meeting: Meeting) -> Result<Self> {
        Ok(Self {
            meta: meeting.read_meta()?,
            notes: meeting.read_notes()?,
            enhanced: meeting.read_enhanced()?,
            transcript: meeting.read_transcript()?,
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
}
impl Save {
    fn write(&self) -> Result<()> {
        if let Some(text) = &self.notes {
            self.meeting.write_notes(text)?;
        }
        if let Some(text) = &self.enhanced {
            self.meeting.write_enhanced(text)?;
        }
        if let Some(transcript) = &self.transcript {
            self.meeting.write_transcript(transcript)?;
        }
        Ok(())
    }
    fn fields(&self) -> Fields {
        Fields {
            notes: self.notes.is_some(),
            enhanced: self.enhanced.is_some(),
            transcript: self.transcript.is_some(),
        }
    }
}
#[derive(Clone)]
pub struct RecordRequest {
    pub settings: Settings,
    pub title: String,
    pub process_id: Option<u32>,
    pub app: Option<String>,
}

pub enum Command {
    List {
        root: PathBuf,
        query: String,
        request: u64,
    },
    Load(Meeting),
    Save(Box<Save>),
    Inventory,
    Record(RecordRequest),
    Enhance {
        meeting: Meeting,
        template: String,
    },
    Download(String),
    Settings {
        settings: Settings,
        llm: LlmSettings,
    },
}
pub enum Event {
    Boot {
        settings: Settings,
        llm: LlmSettings,
        warnings: Vec<String>,
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
        fields: Fields,
        result: Result<(), String>,
    },
    Inventory(Result<Inventory, String>),
    Started {
        document: Box<DocumentData>,
        stop: StopHandle,
    },
    Pipeline(PipelineEvent),
    RecordDone {
        path: Option<PathBuf>,
        document: Option<Box<DocumentData>>,
        error: Option<String>,
    },
    EnhancedChunk {
        path: PathBuf,
        text: String,
    },
    Enhanced {
        path: PathBuf,
        result: Result<String, String>,
    },
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
        let _ = events.send(Event::Boot {
            settings,
            llm,
            warnings,
        });
        for command in rx {
            match command {
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
                        result: DocumentData::read(meeting).map_err(error_text),
                    });
                }
                Command::Save(save) => {
                    let _ = events.send(Event::Saved {
                        path: save.meeting.path().to_owned(),
                        fields: save.fields(),
                        result: save.write().map_err(error_text),
                    });
                }
                Command::Inventory => {
                    let _ = events.send(Event::Inventory(inventory().map_err(error_text)));
                }
                Command::Record(request) => {
                    let events = events.clone();
                    thread::spawn(move || record(request, &events));
                }
                Command::Enhance { meeting, template } => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let path = meeting.path().to_owned();
                        let result = (|| {
                            let meta = meeting.read_meta()?;
                            let root = path.parent().context("Нет библиотеки")?;
                            let template_data = Template::load(root, &template)?;
                            let config = Config::load()?;
                            let provider = HttpProvider::new(
                                config.select(meta.mode, None)?.clone(),
                                meta.mode,
                            )?;
                            let result = enhance_meeting(
                                &meeting,
                                &template_data,
                                &provider,
                                &mut |text| {
                                    let _ = events.send(Event::EnhancedChunk {
                                        path: path.clone(),
                                        text: text.into(),
                                    });
                                },
                            )?;
                            let mut meta = meeting.read_meta()?;
                            meta.template = Some(template);
                            meeting.write_meta(&meta)?;
                            Library::open(root)?.reindex_incremental()?;
                            Ok(result)
                        })()
                        .map_err(error_text);
                        let _ = events.send(Event::Enhanced { path, result });
                    });
                }
                Command::Download(model) => {
                    let events = events.clone();
                    thread::spawn(move || {
                        let result = download(&model, &events).map_err(error_text);
                        let _ = events.send(Event::Downloaded(result));
                    });
                }
                Command::Settings { settings, llm } => {
                    let result = (|| {
                        settings.validate()?;
                        let llm_text = llm.validate()?;
                        Library::open(&settings.library)?;
                        atomic_write(&notetaker_llm::Config::path()?, &llm_text)?;
                        atomic_write(&Settings::path()?, &toml::to_string_pretty(&settings)?)?;
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

fn error_text(error: anyhow::Error) -> String {
    format!("{error:#}")
}
fn list(root: &Path, query: &str) -> Result<Vec<MeetingRow>> {
    let library = Library::open(root)?;
    library.reindex_incremental()?;
    let mut rows = Vec::new();
    let hits = if query.trim().is_empty() {
        None
    } else {
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
        client.load_model(&self.config.model, self.config.provider)?;
        self.client = Some(client);
        Ok(())
    }
    fn transcribe(
        &mut self,
        input: notetaker_asr_proto::AudioInput,
        channel: notetaker_capture::Channel,
        offset_ms: u64,
    ) -> Result<Vec<notetaker_asr_proto::Segment>> {
        self.request += 1;
        let channel = match channel {
            notetaker_capture::Channel::Mic => "mic",
            notetaker_capture::Channel::System => "system",
        };
        Ok(self.client.as_mut().context("ASR не запущен")?.transcribe(
            &self.request.to_string(),
            input,
            channel,
            offset_ms,
        )?)
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
fn record(request: RecordRequest, events: &Sender<Event>) {
    let mut path = None;
    let mut meeting_for_reload = None;
    let result: Result<()> = (|| {
        let binary = asr_binary()?;
        let mut config = AsrConfig::new(&request.settings.model)?;
        config.binary = binary;
        let mut asr = AppAsr {
            config,
            client: None,
            preloaded: false,
            request: 0,
        };
        asr.start()
            .context("Модель ASR недоступна. Откройте настройки и нажмите «Скачать»")?;
        asr.preloaded = true;
        let [mic, system] = live_sources(LiveOptions {
            process_id: request.process_id,
            ..LiveOptions::default()
        })?;
        let sources: [Box<dyn AudioSource + Send>; 2] = [Box::new(mic), Box::new(system)];
        let library = Library::open(&request.settings.library)?;
        let mut meta = MeetingMeta::new(request.title, Utc::now());
        meta.mode = request.settings.mode;
        meta.app = request.app;
        let session = RecordingSession::start_with_asr(
            library.clone(),
            meta,
            sources,
            asr,
            RecordingConfig {
                retention: request
                    .settings
                    .retention_days
                    .map(|days| Duration::from_secs(u64::from(days) * 86400)),
                ..RecordingConfig::default()
            },
        )?;
        let meeting = session.meeting().clone();
        path = Some(meeting.path().to_owned());
        meeting_for_reload = Some(meeting.clone());
        let document = DocumentData::read(meeting)?;
        if events
            .send(Event::Started {
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
        library.reindex_incremental()?;
        library.sweep_audio(Utc::now())?;
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

pub fn asr_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NOTETAKER_ASR_BIN") {
        let path = PathBuf::from(path);
        ensure!(path.is_file(), "NOTETAKER_ASR_BIN не указывает на файл");
        return Ok(path);
    }
    let path = std::env::current_exe()?
        .with_file_name(format!("notetaker-asr{}", std::env::consts::EXE_SUFFIX));
    ensure!(
        path.is_file(),
        "Не найден {}. Поместите notetaker-asr рядом с приложением (соберите notetaker-asr-worker).",
        path.display()
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
            let _ = events.send(Event::DownloadProgress(line?));
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
    fn storage_save_search_and_reopen() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Созвон", Utc::now()))
            .unwrap();
        Save {
            meeting: meeting.clone(),
            notes: Some("Обсудить Kubernetes".into()),
            enhanced: None,
            transcript: None,
        }
        .write()
        .unwrap();
        assert_eq!(list(root.path(), "Kuber").unwrap().len(), 1);
        let loaded = DocumentData::read(meeting).unwrap();
        assert_eq!(loaded.notes, "Обсудить Kubernetes");
        assert!(loaded.enhanced.is_empty());
        assert!(list(root.path(), "несуществующее").unwrap().is_empty());
    }
}
