//! Window-independent state and transitions; rendering lives in ui.rs.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    time::{Duration, Instant},
};

use notetaker_capture::{CaptureEvent, Inventory};
use notetaker_core::{Channel, Meeting, Transcript};
use notetaker_pipeline::{PipelineEvent, StopHandle};
use uuid::Uuid;

use crate::{
    config::{LlmSettings, Settings},
    detection::{CallPrompt, Monitor, proposal},
    worker::{self, Command, DocumentData, Event, Fields, MeetingRow, RecordRequest, Save},
};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Notes,
    #[default]
    Enhanced,
    Transcript,
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    Finalizing,
}
#[derive(Default)]
pub struct Recording {
    pub phase: Phase,
    pub importing: bool,
    pub process_id: Option<u32>,
    pub path: Option<PathBuf>,
    pub started: Option<Instant>,
    pub stopped: Option<Duration>,
    pub stop: Option<StopHandle>,
    pub stop_requested: bool,
    pub lag: f64,
    pub live_disabled: bool,
    pub fallback: bool,
    pub capture_incomplete: bool,
    pub progress: Option<(usize, usize)>,
    pub messages: VecDeque<String>,
    audio_warnings: AudioWarnings,
}
impl Recording {
    pub fn busy(&self) -> bool {
        self.phase != Phase::Idle
    }
    pub fn elapsed(&self) -> Duration {
        self.stopped
            .unwrap_or_else(|| self.started.map_or(Duration::ZERO, |start| start.elapsed()))
    }
    pub fn message(&mut self, text: String) {
        if self.messages.len() == 8 {
            self.messages.pop_front();
        }
        self.messages.push_back(text);
    }
    pub fn apply(&mut self, event: &PipelineEvent) {
        match event {
            PipelineEvent::Lag { seconds } => self.lag = *seconds,
            PipelineEvent::LiveDisabled => self.live_disabled = true,
            PipelineEvent::Warning(message) => self.message(message.clone()),
            PipelineEvent::CaptureIssue(issue) => {
                self.capture_incomplete = true;
                self.message(format!(
                    "Неполный захват {} с {}: {}",
                    speaker(issue.channel),
                    timestamp(issue.from_ms),
                    issue.reason
                ));
            }
            PipelineEvent::FinalPassProgress { completed, total } => {
                if self.stopped.is_none() {
                    self.stopped = Some(self.elapsed());
                }
                self.phase = Phase::Finalizing;
                self.progress = Some((*completed, *total));
            }
            PipelineEvent::CaptureEvent(CaptureEvent::Fallback { .. }) => {
                self.fallback = true;
                self.message("Переключение на весь системный звук".into());
            }
            PipelineEvent::CaptureEvent(CaptureEvent::Error { .. }) | PipelineEvent::Error(_) => {}
            PipelineEvent::CaptureEvent(
                CaptureEvent::Discontinuity { .. } | CaptureEvent::TimestampError { .. },
            ) => self.audio_warnings.pending += 1,
            PipelineEvent::AutoStopped => {
                if self.stopped.is_none() {
                    self.stopped = Some(self.elapsed());
                }
                self.phase = Phase::Finalizing;
                self.message("Запись остановлена: долгая тишина".into());
            }
            // Finished means the pipeline is done; RecordDone follows wait() and
            // the final reload. Keep editing locked until that reload arrives.
            PipelineEvent::Finished { .. }
            | PipelineEvent::LiveSegments(_)
            | PipelineEvent::CaptureEvent(CaptureEvent::Stopped { .. }) => {}
        }
    }
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
        if let Some(stop) = &self.stop {
            stop.stop();
            self.stopped = Some(self.elapsed());
            self.phase = Phase::Finalizing;
        }
    }
}

#[derive(Default)]
struct AudioWarnings {
    last: Option<Instant>,
    pending: u64,
}
impl AudioWarnings {
    fn take(&mut self, now: Instant) -> Option<u64> {
        if self.pending == 0
            || self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(30))
        {
            return None;
        }
        self.last = Some(now);
        Some(std::mem::take(&mut self.pending))
    }
}

pub struct SummaryStatus {
    changed_at: Instant,
    template: String,
    input: String,
    stale: bool,
}

pub struct Document {
    pub attachments_pending: usize,
    pub notes_cursor: Option<std::ops::Range<usize>>,
    pub inserted_cursor: Option<usize>,
    pub data: DocumentData,
    pub dirty: Fields,
    pub saving: bool,
    pub changed_at: Instant,
    pub generation: Option<String>,
    pub template: String,
    pub summary_status: Option<SummaryStatus>,
}
impl Document {
    fn new(data: DocumentData) -> Self {
        let template = data
            .meta
            .template
            .clone()
            .unwrap_or_else(|| "general".into());
        Self {
            attachments_pending: 0,
            notes_cursor: None,
            inserted_cursor: None,
            data,
            dirty: Fields::default(),
            saving: false,
            changed_at: Instant::now(),
            generation: None,
            template,
            summary_status: None,
        }
    }
    pub fn changed(&mut self) {
        self.changed_at = Instant::now();
    }
    pub fn summary_stale(&mut self) -> bool {
        let Some(stamp) = &self.data.meta.summary else {
            return false;
        };
        if let Some(status) = &self.summary_status
            && status.changed_at == self.changed_at
            && status.template == self.template
            && status.input == stamp.input
        {
            return status.stale;
        }
        let stale = stamp.stale(&self.data.notes, &self.data.transcript, &self.template);
        self.summary_status = Some(SummaryStatus {
            changed_at: self.changed_at,
            template: self.template.clone(),
            input: stamp.input.clone(),
            stale,
        });
        stale
    }
    fn save(&mut self) -> Option<Save> {
        if self.saving || self.attachments_pending > 0 || !self.dirty.any() {
            return None;
        }
        let save = Save {
            meeting: self.data.meeting.clone(),
            revisions: self.data.revisions,
            notes: self.dirty.notes.then(|| self.data.notes.clone()),
            enhanced: self.dirty.enhanced.then(|| self.data.enhanced.clone()),
            transcript: self.dirty.transcript.then(|| self.data.transcript.clone()),
        };
        self.dirty = Fields::default();
        self.saving = true;
        Some(save)
    }
}

pub struct AppState {
    pub settings: Settings,
    pub settings_draft: Settings,
    pub llm_draft: LlmSettings,
    llm: LlmSettings,
    llm_configured: bool,
    pub connections: crate::ai::Connections,
    pub chat: crate::chat::Panel,
    pub calendar_events: Vec<notetaker_core::calendar::CalendarEvent>,
    pub calendar_secrets: HashMap<String, String>,
    pub calendar_busy: bool,
    pub calendar_status: String,
    calendar_checked: Option<Instant>,
    calendar_generation: u64,
    prefilled_title: Option<String>,
    selected_event: Option<notetaker_core::calendar::CalendarEvent>,
    pub generation_cancel: notetaker_llm::Cancellation,
    pending_summary: Option<PathBuf>,
    pub ready: bool,
    pub settings_pending: bool,
    pub rows: Vec<MeetingRow>,
    pub query: String,
    pub query_changed: Option<Instant>,
    pub listing: bool,
    request: u64,
    pub documents: HashMap<PathBuf, Document>,
    loading: HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    pub tab: Tab,
    pub recording: Recording,
    pub call_prompt: Option<CallPrompt>,
    detector: Monitor,
    pub inventory: Inventory,
    pub inventory_pending: bool,
    pub inventory_error: Option<String>,
    pub title: String,
    pub record_model: String,
    pub record_mode: notetaker_core::Mode,
    pub process_id: Option<u32>,
    pub templates: Vec<String>,
    pub enhancing: Option<PathBuf>,
    pub voices: notetaker_core::VoiceLibrary,
    pub downloading: bool,
    pub offer_model_download: bool,
    pub download_messages: VecDeque<String>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub status: String,
    pub exit_requested: bool,
    tx: Sender<Command>,
    rx: Receiver<Event>,
}
impl AppState {
    pub fn new() -> Self {
        let (tx, rx) = worker::spawn();
        let mut state = Self::with_channels(tx, rx);
        state.detector.start();
        state
    }
    fn with_channels(tx: Sender<Command>, rx: Receiver<Event>) -> Self {
        let settings = Settings::default();
        Self {
            settings: settings.clone(),
            settings_draft: settings.clone(),
            llm_draft: LlmSettings::default(),
            llm: LlmSettings::default(),
            llm_configured: false,
            connections: crate::ai::Connections::default(),
            chat: Default::default(),
            calendar_events: Vec::new(),
            calendar_secrets: HashMap::new(),
            calendar_busy: false,
            calendar_status: String::new(),
            calendar_checked: None,
            calendar_generation: 0,
            prefilled_title: None,
            selected_event: None,
            generation_cancel: notetaker_llm::Cancellation::default(),
            pending_summary: None,
            ready: false,
            settings_pending: false,
            rows: Vec::new(),
            query: String::new(),
            query_changed: None,
            listing: false,
            request: 0,
            documents: HashMap::new(),
            loading: HashSet::new(),
            selected: None,
            tab: Tab::Enhanced,
            recording: Recording::default(),
            call_prompt: None,
            detector: Monitor::default(),
            inventory: Inventory::default(),
            inventory_pending: false,
            inventory_error: None,
            title: String::new(),
            record_model: settings.model,
            record_mode: settings.mode,
            process_id: None,
            templates: vec!["general".into()],
            enhancing: None,
            voices: Default::default(),
            downloading: false,
            offer_model_download: false,
            download_messages: VecDeque::new(),
            error: None,
            warnings: Vec::new(),
            status: "Открываем библиотеку…".into(),
            exit_requested: false,
            tx,
            rx,
        }
    }
    pub fn send(&mut self, command: Command) {
        let selecting_ai = matches!(&command, Command::Ai(action) if action.operation == crate::ai::Operation::Default);
        if selecting_ai {
            self.settings_pending = true;
        }
        if self.tx.send(command).is_err() {
            if selecting_ai {
                self.settings_pending = false;
            }
            self.error = Some("Фоновый поток завершился; перезапустите приложение".into());
        }
    }
    pub fn attach(&mut self, inputs: Vec<crate::attachments::ImageInput>) {
        if inputs.is_empty() || self.settings_pending || self.exit_requested {
            return;
        }
        let Some(document) = self
            .selected
            .as_ref()
            .and_then(|path| self.documents.get_mut(path))
        else {
            return;
        };
        let command = Command::Attach {
            meeting: document.data.meeting.clone(),
            inputs,
            anchor: crate::attachments::InsertAt {
                notes: document.data.notes.clone(),
                cursor: document.notes_cursor.clone(),
            },
        };
        document.attachments_pending += 1;
        if self.tx.send(command).is_err() {
            document.attachments_pending -= 1;
            self.error = Some("Фоновый поток завершился; перезапустите приложение".into());
        }
    }
    pub fn refresh(&mut self) {
        self.request += 1;
        self.listing = true;
        self.query_changed = None;
        self.send(Command::List {
            root: self.settings.library.clone(),
            query: self.query.clone(),
            request: self.request,
        });
    }
    pub fn select(&mut self, meeting: Meeting) {
        self.flush();
        let path = meeting.path().to_owned();
        self.selected = Some(path.clone());
        if !self.documents.contains_key(&path) && self.loading.insert(path) {
            self.send(Command::Load(meeting));
        }
    }
    pub fn flush(&mut self) {
        let saves: Vec<_> = self
            .documents
            .values_mut()
            .filter_map(Document::save)
            .collect();
        for save in saves {
            self.send(Command::Save(Box::new(save)));
        }
    }
    pub fn unsaved(&self) -> bool {
        self.documents
            .values()
            .any(|doc| doc.saving || doc.dirty.any() || doc.attachments_pending > 0)
    }
    pub fn prefill_call_title(&mut self) {
        if self.title.trim().is_empty()
            && let Some(title) = self
                .detector
                .current_call(self.process_id)
                .and_then(|call| call.meeting_title.as_ref())
        {
            self.title = title.clone();
            self.prefilled_title = Some(title.clone());
        }
    }
    pub fn start_recording(&mut self) {
        self.start_recording_with_app(None);
    }
    pub fn accept_call(&mut self) {
        match self.call_prompt.clone() {
            Some(CallPrompt::Start {
                app,
                pid,
                meeting_title,
            }) => {
                self.process_id = Some(pid);
                self.record_model = self.settings.model.clone();
                self.record_mode = self.settings.mode;
                self.start_recording_with_app(Some(notetaker_detect::DetectedCall {
                    app,
                    pid,
                    meeting_title,
                }));
            }
            Some(CallPrompt::Stop { .. }) => {
                self.recording.request_stop();
                if self.selected == self.recording.path {
                    self.tab = Tab::Enhanced;
                }
                self.call_prompt = None;
            }
            None => {}
        }
    }
    pub fn dismiss_call(&mut self) {
        if let Some(CallPrompt::Start { pid, .. }) = self.call_prompt.take() {
            self.detector.dismiss(pid);
        }
    }
    fn start_recording_with_app(&mut self, call_override: Option<notetaker_detect::DetectedCall>) {
        if !self.ready
            || self.recording.busy()
            || self.downloading
            || self.settings_pending
            || self.exit_requested
        {
            return;
        }
        let call = call_override.or_else(|| self.detector.current_call(self.process_id).cloned());
        let app = call.as_ref().map(|call| call.app.clone()).or_else(|| {
            self.inventory
                .sessions
                .iter()
                .find(|s| Some(s.pid) == self.process_id)
                .map(|s| s.exe.clone().unwrap_or_else(|| s.name.clone()))
        });
        let mut settings = self.settings.clone();
        settings.model = self.record_model.clone();
        settings.mode = self.record_mode;
        let event = self.selected_event.take().or_else(|| {
            notetaker_core::calendar::overlapping(&self.calendar_events, chrono::Utc::now())
                .cloned()
        });
        let typed = !self.title.trim().is_empty()
            && self.prefilled_title.as_deref() != Some(self.title.as_str());
        let title = if let Some(event) = event.as_ref().filter(|_| !typed) {
            event.link.title.clone()
        } else if self.title.trim().is_empty() {
            call.and_then(|call| call.meeting_title).unwrap_or_else(|| {
                chrono::Local::now()
                    .format("Встреча %d.%m.%Y %H:%M")
                    .to_string()
            })
        } else {
            self.title.trim().to_owned()
        };
        self.call_prompt = None;
        self.recording = Recording {
            phase: Phase::Starting,
            process_id: self.process_id,
            ..Recording::default()
        };
        self.send(Command::Record(RecordRequest {
            settings,
            title,
            event,
            process_id: self.process_id,
            app,
        }));
    }
    pub fn start_event(&mut self, event: notetaker_core::calendar::CalendarEvent) {
        if !self.ready
            || self.recording.busy()
            || self.downloading
            || self.settings_pending
            || self.exit_requested
        {
            return;
        }
        self.title.clear();
        self.prefilled_title = None;
        self.selected_event = Some(event);
        self.record_model = self.settings.model.clone();
        self.record_mode = self.settings.mode;
        self.start_recording();
    }
    pub fn refresh_calendar(&mut self) {
        if self.calendar_busy {
            return;
        }
        self.calendar_busy = true;
        self.calendar_checked = Some(Instant::now());
        self.calendar_generation += 1;
        self.send(Command::Calendar {
            subscriptions: self.settings.calendars.clone(),
            timezone: self.settings.calendar_timezone.clone(),
            generation: self.calendar_generation,
        });
    }
    pub fn ask(&mut self) {
        if self.chat.busy || self.unsaved() || self.settings_pending || self.recording.busy() {
            return;
        }
        self.chat.pending = std::mem::take(&mut self.chat.question);
        self.chat.streaming.clear();
        self.chat.error = None;
        self.chat.busy = true;
        self.chat.cancel = Default::default();
        self.send(Command::Chat {
            root: self.settings.library.clone(),
            scope: self.chat.scope.clone(),
            question: self.chat.pending.clone(),
            history: self.chat.messages.iter().map(|m| m.turn.clone()).collect(),
            cancel: self.chat.cancel.clone(),
        });
    }
    pub fn import_recording(&mut self, file: PathBuf) {
        if !self.ready
            || self.recording.busy()
            || self.downloading
            || self.settings_pending
            || self.exit_requested
        {
            return;
        }
        self.flush();
        let mut settings = self.settings.clone();
        settings.model = self.record_model.clone();
        settings.mode = self.record_mode;
        self.recording = Recording {
            phase: Phase::Starting,
            importing: true,
            ..Recording::default()
        };
        self.send(Command::Import { settings, file });
    }
    pub fn toggle_recording(&mut self) {
        if self.recording.busy() {
            self.recording.request_stop();
            if self.selected == self.recording.path {
                self.tab = Tab::Enhanced;
            }
        } else {
            self.start_recording();
        }
    }
    pub fn enhance(&mut self) {
        if self.enhancing.is_some()
            || self.recording.busy()
            || self.settings_pending
            || self.unsaved()
        {
            return;
        }
        let Some(path) = self.selected.clone() else {
            return;
        };
        self.start_summary(path, false);
    }
    fn start_summary(&mut self, path: PathBuf, automatic: bool) {
        self.flush();
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        self.generation_cancel = notetaker_llm::Cancellation::default();
        document.generation = Some(String::new());
        let command = Command::Enhance {
            meeting: document.data.meeting.clone(),
            template: document.template.clone(),
            cancel: self.generation_cancel.clone(),
            automatic,
        };
        self.enhancing = Some(path);
        self.send(command);
    }
    pub fn generation_cancellable(&self) -> bool {
        self.enhancing
            .as_ref()
            .and_then(|path| self.documents.get(path))
            .is_some_and(|doc| {
                let id = match doc.data.meta.mode {
                    notetaker_core::Mode::Local => &self.llm.defaults.local,
                    notetaker_core::Mode::Hybrid => &self.llm.defaults.hybrid,
                };
                self.llm.providers.iter().any(|provider| {
                    &provider.id == id
                        && matches!(provider.kind.as_str(), "claude-cli" | "codex-cli")
                })
            })
    }
    pub fn download(&mut self, model: String) {
        if self.downloading || self.recording.busy() {
            return;
        }
        self.downloading = true;
        self.download_messages.clear();
        self.download_messages
            .push_back(format!("Загрузка {model}…"));
        self.send(Command::Download(model));
    }
    pub fn save_settings(&mut self) {
        if self.recording.busy()
            || self.enhancing.is_some()
            || self.chat.busy
            || self.unsaved()
            || self.settings_pending
        {
            return;
        }
        if let Err(error) = self
            .settings_draft
            .validate()
            .and_then(|_| self.llm_draft.validate().map(|_| ()))
        {
            self.error = Some(format!("{error:#}"));
            return;
        }
        self.settings_pending = true;
        self.send(Command::Settings {
            settings: self.settings_draft.clone(),
            llm: self.llm_draft.clone(),
            calendar_secrets: self
                .calendar_secrets
                .iter()
                .filter(|(_, secret)| !secret.trim().is_empty())
                .map(|(id, secret)| (id.clone(), secret.clone()))
                .collect(),
        });
    }
    pub fn exit(&mut self) {
        self.exit_requested = true;
        self.chat.cancel.cancel();
        self.recording.request_stop();
        self.flush();
    }
    pub fn can_exit(&self) -> bool {
        self.exit_requested
            && !self.recording.busy()
            && self.enhancing.is_none()
            && !self.chat.busy
            && !self.downloading
            && !self.unsaved()
            && !self.settings_pending
    }
    pub fn poll(&mut self) {
        // Bound one UI update even if ASR/LLM produced a burst.
        for _ in 0..256 {
            let Ok(event) = self.rx.try_recv() else {
                break;
            };
            self.apply(event);
        }
        if let Some(count) = self.recording.audio_warnings.take(Instant::now()) {
            self.recording.message(format!(
                "Разрыв звука / ошибка временной метки (событий: {count})"
            ));
        }
        self.poll_detection();
        if self.ready
            && !self.settings.calendars.is_empty()
            && !self.exit_requested
            && !self.calendar_busy
            && self
                .calendar_checked
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(600))
        {
            self.refresh_calendar();
        }
        if self
            .query_changed
            .is_some_and(|time| time.elapsed() > Duration::from_millis(300))
        {
            self.refresh();
        }
        let saves: Vec<_> = self
            .documents
            .values_mut()
            .filter(|doc| {
                doc.changed_at.elapsed() > Duration::from_millis(750) || self.exit_requested
            })
            .filter_map(Document::save)
            .collect();
        for save in saves {
            self.send(Command::Save(Box::new(save)));
        }
        if self.pending_summary.is_some()
            && !self.unsaved()
            && !self.recording.busy()
            && self.enhancing.is_none()
            && !self.settings_pending
        {
            let path = self.pending_summary.take().expect("pending summary");
            if !self.exit_requested
                && let Some(document) = self.documents.get(&path)
                && self.llm_configured
                && auto_summary_ready(self.settings.auto_summary, &document.data, &self.llm)
            {
                self.start_summary(path, true);
            }
        }
        // Keep only active/unsaved documents; revisiting a meeting reloads files
        // and opening a large library does not accumulate transcripts in RAM.
        let selected = &self.selected;
        let recording = &self.recording;
        let enhancing = &self.enhancing;
        self.documents.retain(|path, doc| {
            selected.as_ref() == Some(path)
                || (recording.busy() && recording.path.as_ref() == Some(path))
                || enhancing.as_ref() == Some(path)
                || self.pending_summary.as_ref() == Some(path)
                || doc.dirty.any()
                || doc.saving
                || doc.attachments_pending > 0
        });
    }
    fn poll_detection(&mut self) {
        self.detector.configure(
            self.ready && self.settings.detect_calls,
            self.settings.detect_browsers,
            self.settings.detect_any_microphone,
            &self.settings.call_apps,
        );
        if !self.settings.detect_calls || self.exit_requested {
            self.call_prompt = None;
        }
        for event in self.detector.poll() {
            if !self.settings.detect_calls || self.exit_requested {
                continue;
            }
            if let notetaker_detect::DetectEvent::CallEnded { pid, .. } = &event
                && matches!(&self.call_prompt, Some(CallPrompt::Start { pid: offered, .. }) if offered == pid)
            {
                self.call_prompt = None;
            }
            if let Some(prompt) = proposal(
                &event,
                self.recording.phase,
                self.recording.process_id,
                self.recording.stop_requested,
            ) {
                self.call_prompt = Some(prompt);
            }
        }
        if let Some(CallPrompt::Start {
            app,
            pid,
            meeting_title,
        }) = &mut self.call_prompt
            && let Some(call) = self.detector.current_call(Some(*pid))
        {
            *app = call.app.clone();
            *meeting_title = call.meeting_title.clone();
        }
        if matches!(self.call_prompt, Some(CallPrompt::Stop { .. }))
            && (!matches!(self.recording.phase, Phase::Starting | Phase::Recording)
                || self.recording.stop_requested)
        {
            self.call_prompt = None;
        }
    }
    fn apply(&mut self, event: Event) {
        match event {
            Event::Calendar { generation, result } => {
                if generation == self.calendar_generation {
                    self.calendar_busy = false;
                    match result {
                        Ok(events) => {
                            self.calendar_status = format!("Событий: {}", events.len());
                            self.calendar_events = events;
                        }
                        Err(error) => self.calendar_status = error,
                    }
                }
            }
            Event::ChatChunk(text) => self.chat.streaming.push_str(&text),
            Event::ChatDone(result) => {
                self.chat.busy = false;
                self.chat.streaming.clear();
                match result {
                    Ok(result) => {
                        self.chat.messages.push(result);
                        if self.chat.messages.len() > 20 {
                            self.chat.messages.remove(0);
                        }
                    }
                    Err(error) => {
                        self.chat.error = Some(error);
                        self.chat.question = self.chat.pending.clone();
                    }
                }
            }
            Event::Ai(response) => {
                if response.operation == crate::ai::Operation::Default {
                    self.settings_pending = false;
                }
                self.connections.apply(&response);
                if response.operation == crate::ai::Operation::Default && response.result.is_ok() {
                    let provider = response.provider;
                    let cloud = provider
                        .config()
                        .is_ok_and(|config| config.validate(notetaker_core::Mode::Local).is_err());
                    self.llm_draft.defaults.hybrid = provider.id.clone();
                    if !cloud {
                        self.llm_draft.defaults.local = provider.id.clone();
                    }
                    if let Some(existing) = self
                        .llm_draft
                        .providers
                        .iter_mut()
                        .find(|p| p.id == provider.id)
                    {
                        *existing = provider;
                    } else {
                        self.llm_draft.providers.push(provider);
                    }
                    if cloud && self.settings_draft.mode == notetaker_core::Mode::Local {
                        self.settings_draft.mode = notetaker_core::Mode::Hybrid;
                        self.warnings
                            .push("Для новых встреч включён Hybrid: выбран облачный AI".into());
                    }
                    self.save_settings();
                }
            }
            Event::Attached {
                path,
                anchor,
                results,
            } => {
                let mut images = Vec::new();
                for result in results {
                    match result {
                        Ok(image) => images.push(image),
                        Err(error) => self.warnings.push(error),
                    }
                }
                if let Some(document) = self.documents.get_mut(&path) {
                    document.attachments_pending = document.attachments_pending.saturating_sub(1);
                    if !images.is_empty() {
                        let cursor =
                            crate::attachments::insert(&mut document.data.notes, anchor, &images);
                        document.inserted_cursor = Some(cursor);
                        document.notes_cursor = Some(cursor..cursor);
                        document.dirty.notes = true;
                        document.changed();
                    }
                }
            }
            Event::Boot {
                settings,
                llm,
                warnings,
                needs_model,
                llm_configured,
            } => {
                self.llm_configured = llm_configured;
                self.offer_model_download = needs_model;
                self.settings = settings.clone();
                self.settings_draft = settings.clone();
                self.connections.load(&llm);
                self.llm = llm.clone();
                self.llm_draft = llm;
                self.record_model = settings.model;
                self.record_mode = settings.mode;
                self.ready = true;
                if !warnings.is_empty() {
                    self.error = Some(warnings.join("\n"));
                }
                self.refresh();
            }
            Event::Listed {
                request,
                result,
                templates,
            } if request == self.request => {
                self.listing = false;
                self.templates = templates;
                match result {
                    Ok(rows) => {
                        self.rows = rows;
                        self.status = format!("Встреч: {}", self.rows.len());
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Event::Listed { .. } => {}
            Event::Loaded { path, result } => {
                self.loading.remove(&path);
                match result {
                    Ok(data) => {
                        self.documents
                            .entry(path)
                            .or_insert_with(|| Document::new(data));
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Event::Saved { path, result } => {
                if let Some(doc) = self.documents.get_mut(&path) {
                    doc.saving = false;
                    doc.data.revisions = result.revisions;
                    let reloaded = result.reloaded.as_deref();
                    // Keep edits made after enqueueing the save. If they also
                    // conflict, their next save preserves them before reloading.
                    if result.conflicts.notes
                        && !doc.dirty.notes
                        && let Some(data) = reloaded
                    {
                        doc.data.notes = data.notes.clone();
                        doc.data.revisions.notes = data.revisions.notes;
                    } else {
                        doc.dirty.notes |= result.failed.notes;
                    }
                    if result.conflicts.enhanced
                        && !doc.dirty.enhanced
                        && let Some(data) = reloaded
                    {
                        doc.data.enhanced = data.enhanced.clone();
                        doc.data.revisions.enhanced = data.revisions.enhanced;
                    } else {
                        doc.dirty.enhanced |= result.failed.enhanced;
                    }
                    if result.conflicts.transcript
                        && !doc.dirty.transcript
                        && let Some(data) = reloaded
                    {
                        doc.data.transcript = data.transcript.clone();
                        doc.data.revisions.transcript = data.revisions.transcript;
                    } else {
                        doc.dirty.transcript |= result.failed.transcript;
                    }
                    if !result.errors.is_empty() {
                        doc.changed_at = Instant::now();
                        self.error = Some(result.errors.join("\n"));
                        self.exit_requested = false;
                    }
                }
            }
            Event::Exported(result) => match result {
                Ok(path) => self
                    .warnings
                    .push(format!("Конспект сохранён: {}", path.display())),
                Err(error) => self.error = Some(error),
            },
            Event::Inventory(result) => {
                self.inventory_pending = false;
                match result {
                    Ok(inventory) => {
                        self.inventory = inventory;
                        self.inventory_error = None;
                        if !self
                            .inventory
                            .sessions
                            .iter()
                            .any(|s| Some(s.pid) == self.process_id)
                        {
                            self.process_id = None;
                        }
                    }
                    Err(error) => self.inventory_error = Some(error),
                }
            }
            Event::Started {
                document,
                stop,
                importing,
            } => {
                let path = document.meeting.path().to_owned();
                self.selected = Some(path.clone());
                self.tab = Tab::Notes;
                self.recording.path = Some(path.clone());
                self.recording.stop = Some(stop);
                self.recording.started = Some(Instant::now());
                self.recording.importing = importing;
                self.recording.phase = if importing {
                    Phase::Finalizing
                } else {
                    Phase::Recording
                };
                if self.recording.stop_requested {
                    self.recording.request_stop();
                    self.tab = Tab::Enhanced;
                }
                self.documents.insert(path, Document::new(*document));
                self.refresh();
            }
            Event::Pipeline(event) => {
                let previous = self.recording.phase;
                self.recording.apply(&event);
                if let PipelineEvent::Error(message)
                | PipelineEvent::CaptureEvent(CaptureEvent::Error { message, .. }) = &event
                {
                    self.error = Some(message.clone());
                }
                if previous != Phase::Finalizing
                    && self.recording.phase == Phase::Finalizing
                    && self.selected == self.recording.path
                {
                    self.tab = Tab::Enhanced;
                }
                if let PipelineEvent::CaptureIssue(issue) = &event
                    && let Some(doc) = self
                        .recording
                        .path
                        .as_ref()
                        .and_then(|p| self.documents.get_mut(p))
                {
                    doc.data.meta.capture_issues.push(issue.clone());
                }
                if let PipelineEvent::LiveSegments(segments) = event
                    && let Some(doc) = self
                        .recording
                        .path
                        .as_ref()
                        .and_then(|path| self.documents.get_mut(path))
                {
                    for segment in segments {
                        if !doc
                            .data
                            .transcript
                            .segments
                            .iter()
                            .any(|s| s.id == segment.id)
                        {
                            doc.data.transcript.segments.push(segment);
                        }
                    }
                    doc.data.transcript.segments.sort_by_key(|s| s.start_ms);
                    notetaker_pipeline::suppress_echo(&mut doc.data.transcript.segments);
                }
            }
            Event::RecordDone {
                path,
                document,
                error,
            } => {
                if let (Some(path), Some(data)) = (path, document) {
                    if let Some(doc) = self.documents.get_mut(&path) {
                        doc.data.meta = data.meta;
                        doc.data.transcript = data.transcript;
                        doc.data.revisions.transcript = data.revisions.transcript;
                        doc.changed();
                    } else {
                        self.documents.insert(path.clone(), Document::new(*data));
                    }
                    if error.is_none() && self.settings.auto_summary {
                        self.pending_summary = Some(path);
                    }
                }
                self.detector.recording_stopped();
                self.recording.phase = Phase::Idle;
                self.recording.stop = None;
                if let Some(error) = error {
                    self.error = Some(error);
                    self.exit_requested = false;
                }
                self.refresh();
            }
            Event::ReloadEnhanced { path, value } => {
                if let Some(doc) = self.documents.get_mut(&path) {
                    doc.data.enhanced = value.value;
                    doc.data.revisions.enhanced = value.revision;
                }
            }
            Event::EnhancedChunk { path, text } => {
                if let Some(text_buffer) = self
                    .documents
                    .get_mut(&path)
                    .and_then(|doc| doc.generation.as_mut())
                {
                    text_buffer.push_str(&text);
                }
            }
            Event::Enhanced { path, result } => {
                self.enhancing = None;
                if let Some(doc) = self.documents.get_mut(&path) {
                    doc.generation = None;
                    match result {
                        Ok(text) => {
                            doc.data.enhanced = text.text;
                            self.warnings = text.warnings;
                            doc.data.revisions.enhanced = text.revision;
                            doc.data.meta.template = Some(doc.template.clone());
                            doc.data.meta.summary = Some(text.stamp);
                        }
                        Err(error) => {
                            self.error = Some(error);
                            self.exit_requested = false;
                        }
                    }
                }
                self.refresh();
            }
            Event::Voices(result) => match result {
                Ok(voices) => self.voices = voices,
                Err(error) => self.error = Some(error),
            },
            Event::DownloadProgress(message) => {
                if self.download_messages.len() == 8 {
                    self.download_messages.pop_front();
                }
                self.download_messages.push_back(message);
            }
            Event::Downloaded(result) => {
                self.downloading = false;
                match result {
                    Ok(()) => {
                        self.offer_model_download = false;
                        self.download_messages.push_back("Модель готова".into());
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.exit_requested = false;
                    }
                }
            }
            Event::SettingsSaved(result) => {
                self.settings_pending = false;
                match result {
                    Ok((settings, llm)) => {
                        self.llm_configured = true;
                        let changed_library = self.settings.library != settings.library;
                        self.calendar_secrets.clear();
                        self.calendar_events.clear();
                        self.calendar_busy = false;
                        self.calendar_generation += 1;
                        self.calendar_checked = None;
                        if self.settings.detect_calls != settings.detect_calls
                            || self.settings.detect_browsers != settings.detect_browsers
                            || self.settings.detect_any_microphone != settings.detect_any_microphone
                            || self.settings.call_apps != settings.call_apps
                        {
                            self.call_prompt = None;
                        }
                        self.record_model = settings.model.clone();
                        self.record_mode = settings.mode;
                        self.settings = settings.clone();
                        self.settings_draft = settings;
                        self.connections.load(&llm);
                        self.llm = llm.clone();
                        self.llm_draft = llm;
                        if changed_library {
                            self.chat = Default::default();
                            self.selected = None;
                            self.documents.clear();
                            self.rows.clear();
                            self.loading.clear();
                        }
                        self.status = "Настройки сохранены".into();
                        self.refresh();
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
    }
}

fn auto_summary_ready(enabled: bool, data: &DocumentData, llm: &LlmSettings) -> bool {
    enabled
        && worker::summary_can_overwrite(data)
        && llm
            .validate()
            .ok()
            .and_then(|text| notetaker_llm::Config::parse(&text).ok())
            .is_some_and(|config| {
                config
                    .select_scenario(data.meta.mode, notetaker_llm::Scenario::Summary, None)
                    .is_ok()
            })
}

pub fn timestamp(ms: u64) -> String {
    format!("{:02}:{:02}", ms / 60_000, ms / 1000 % 60)
}
pub fn speaker(channel: Channel) -> &'static str {
    match channel {
        Channel::Mic => "Я",
        Channel::System => "Собеседники",
    }
}
pub fn edit_segment(transcript: &mut Transcript, id: Uuid, text: String) -> bool {
    if let Some(segment) = transcript
        .segments
        .iter_mut()
        .find(|segment| segment.id == id)
        && segment.text != text
    {
        segment.text = text;
        segment.edited = true;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{Segment, Source, SpeakerId};
    #[test]
    fn import_locks_recording_and_reuses_final_summary_flow() {
        let (mut state, commands, _root, path) = fixture();
        state.ready = true;
        state.import_recording("recording.mp4".into());
        assert!(state.recording.busy());
        assert!(state.recording.importing);
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Import { .. }
        ));
        state.import_recording("other.wav".into());
        assert!(commands.try_recv().is_err());
        state.apply(Event::RecordDone {
            path: Some(path.clone()),
            document: Some(Box::new(
                DocumentData::read(state.documents[&path].data.meeting.clone()).unwrap(),
            )),
            error: None,
        });
        assert!(!state.recording.busy());
        assert_eq!(state.pending_summary.as_ref(), Some(&path));
    }

    #[test]
    fn audio_warnings_are_counted_and_flushed_at_most_every_thirty_seconds() {
        let start = Instant::now();
        let mut warnings = AudioWarnings::default();
        assert_eq!(warnings.take(start), None);
        warnings.pending += 2;
        assert_eq!(warnings.take(start), Some(2));
        warnings.pending += 8;
        assert_eq!(warnings.take(start + Duration::from_secs(29)), None);
        assert_eq!(warnings.take(start + Duration::from_secs(30)), Some(8));
        assert_eq!(warnings.take(start + Duration::from_secs(60)), None);
    }
    #[test]
    fn time_and_speakers() {
        assert_eq!(timestamp(0), "00:00");
        assert_eq!(timestamp(65_999), "01:05");
        assert_eq!(timestamp(3_600_000), "60:00");
        assert_eq!(speaker(Channel::Mic), "Я");
        assert_eq!(speaker(Channel::System), "Собеседники");
    }
    #[test]
    fn edit_marks_only_changed_segment_and_persists() {
        let id = Uuid::now_v7();
        let mut transcript = Transcript {
            segments: vec![Segment {
                id,
                speaker: SpeakerId("me".into()),
                channel: Channel::Mic,
                start_ms: 10,
                end_ms: 20,
                text: "ошпбка".into(),
                source: Source::Final,
                edited: false,
            }],
            ..Transcript::default()
        };
        assert!(!edit_segment(&mut transcript, id, "ошпбка".into()));
        assert!(!edit_segment(&mut transcript, Uuid::now_v7(), "нет".into()));
        assert!(edit_segment(&mut transcript, id, "ошибка".into()));
        let root = tempfile::tempdir().unwrap();
        let library = notetaker_core::Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new("Edit", chrono::Utc::now()))
            .unwrap();
        meeting.write_transcript(&transcript).unwrap();
        let saved = meeting.read_transcript().unwrap();
        assert!(saved.segments[0].edited);
        assert_eq!(saved.segments[0].start_ms, 10);
        assert_eq!(saved.segments[0].text, "ошибка");
    }
    #[test]
    fn pipeline_status_survives_errors_until_join() {
        let mut recording = Recording {
            phase: Phase::Recording,
            ..Recording::default()
        };
        recording.apply(&PipelineEvent::Lag { seconds: 12.5 });
        assert_eq!(recording.lag, 12.5);
        recording.apply(&PipelineEvent::LiveDisabled);
        assert!(recording.live_disabled);
        recording.apply(&PipelineEvent::CaptureEvent(CaptureEvent::Fallback {
            at: Duration::ZERO,
            reason: notetaker_capture::FallbackReason::ProcessSilent,
        }));
        assert!(recording.fallback);
        recording.apply(&PipelineEvent::CaptureIssue(notetaker_core::CaptureIssue {
            channel: Channel::Mic,
            from_ms: 1000,
            reason: "device removed".into(),
        }));
        assert!(recording.capture_incomplete);
        recording.apply(&PipelineEvent::Error("worker failed".into()));
        assert!(recording.busy());
        recording.apply(&PipelineEvent::FinalPassProgress {
            completed: 1,
            total: 2,
        });
        assert_eq!(recording.phase, Phase::Finalizing);
        assert_eq!(recording.progress, Some((1, 2)));
    }
    #[test]
    fn edits_during_save_remain_dirty() {
        let root = tempfile::tempdir().unwrap();
        let library = notetaker_core::Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new("Save", chrono::Utc::now()))
            .unwrap();
        let mut doc = Document::new(DocumentData::read(meeting).unwrap());
        doc.data.notes = "first".into();
        doc.dirty.notes = true;
        let save = doc.save().unwrap();
        assert_eq!(save.notes.as_deref(), Some("first"));
        doc.data.notes = "second".into();
        doc.dirty.notes = true;
        assert!(doc.save().is_none());
        doc.saving = false;
        assert_eq!(doc.save().unwrap().notes.as_deref(), Some("second"));
    }
    fn fixture() -> (
        AppState,
        std::sync::mpsc::Receiver<Command>,
        tempfile::TempDir,
        PathBuf,
    ) {
        let (tx, commands) = std::sync::mpsc::channel();
        let (_events, rx) = std::sync::mpsc::channel();
        let mut state = AppState::with_channels(tx, rx);
        let root = tempfile::tempdir().unwrap();
        let library = notetaker_core::Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new(
                "State",
                chrono::Utc::now(),
            ))
            .unwrap();
        let path = meeting.path().to_owned();
        state.documents.insert(
            path.clone(),
            Document::new(DocumentData::read(meeting).unwrap()),
        );
        state.selected = Some(path.clone());
        (state, commands, root, path)
    }
    #[test]
    fn attachment_import_blocks_autosave_exit_and_eviction_until_reference_is_inserted() {
        let (mut state, commands, _root, path) = fixture();
        let document = state.documents.get_mut(&path).unwrap();
        document.data.notes = "До".into();
        document.notes_cursor = Some(2..2);
        document.dirty.notes = true;
        state.attach(vec![crate::attachments::ImageInput::File(
            path.join("source.png"),
        )]);
        let Command::Attach { anchor, .. } = commands.try_recv().unwrap() else {
            panic!("expected attachment import")
        };
        state.flush();
        assert!(commands.try_recv().is_err());
        assert!(state.unsaved());
        assert!(state.documents[&path].attachments_pending > 0);
        state.selected = None;
        state.poll();
        assert!(state.documents.contains_key(&path));
        state.apply(Event::Attached {
            path: path.clone(),
            anchor,
            results: vec![Ok("attachments/a.png".into())],
        });
        let document = &state.documents[&path];
        assert_eq!(document.attachments_pending, 0);
        assert_eq!(document.data.notes, "До\n![](attachments/a.png)\n");
        assert!(document.dirty.notes);
        state.flush();
        let Command::Save(save) = commands.try_recv().unwrap() else {
            panic!("expected save")
        };
        assert!(save.notes.unwrap().contains("attachments/a.png"));
    }

    #[test]
    fn failed_attachment_import_unblocks_document_and_produces_toast() {
        let (mut state, commands, _root, path) = fixture();
        state.attach(vec![crate::attachments::ImageInput::File(
            path.join("source.png"),
        )]);
        let Command::Attach { anchor, .. } = commands.try_recv().unwrap() else {
            panic!("expected attachment import")
        };
        state.apply(Event::Attached {
            path: path.clone(),
            anchor,
            results: vec![Err("Изображение больше 20 МБ".into())],
        });
        assert_eq!(state.documents[&path].attachments_pending, 0);
        assert!(!state.unsaved());
        assert_eq!(state.warnings, ["Изображение больше 20 МБ"]);
    }

    #[test]
    fn final_progress_does_not_override_navigation_after_initial_transition() {
        let (mut state, _commands, _root, path) = fixture();
        state.recording.path = Some(path);
        state.recording.phase = Phase::Recording;
        state.tab = Tab::Notes;
        state.apply(Event::Pipeline(PipelineEvent::FinalPassProgress {
            completed: 0,
            total: 2,
        }));
        assert!(state.tab == Tab::Enhanced);
        state.tab = Tab::Transcript;
        state.apply(Event::Pipeline(PipelineEvent::FinalPassProgress {
            completed: 1,
            total: 2,
        }));
        assert!(state.tab == Tab::Transcript);
    }
    #[test]
    fn call_acceptance_uses_pid_and_defaults_without_audio_inventory() {
        let (mut state, commands, _root, _path) = fixture();
        state.ready = true;
        state.settings.model = "gigaam-v3".into();
        state.record_model = "parakeet-v3".into();
        state.record_mode = notetaker_core::Mode::Hybrid;
        state.title = "old title".into();
        state.call_prompt = Some(CallPrompt::Start {
            app: "Teams".into(),
            pid: 42,
            meeting_title: Some("Planning".into()),
        });
        state.accept_call();
        let Command::Record(request) = commands.try_recv().unwrap() else {
            panic!("expected recording");
        };
        assert_eq!(request.process_id, Some(42));
        assert_eq!(request.app.as_deref(), Some("Teams"));
        assert_eq!(request.settings.model, "gigaam-v3");
        assert_eq!(request.settings.mode, state.settings.mode);
        assert_eq!(request.title, "old title");
        assert_eq!(state.recording.process_id, Some(42));
        assert!(state.call_prompt.is_none());
        state.call_prompt = Some(CallPrompt::Stop { pid: 42 });
        state.dismiss_call();
        assert!(!state.recording.stop_requested);
        assert!(state.call_prompt.is_none());
    }
    #[test]
    fn manual_start_uses_detected_title_even_without_a_prompt() {
        for typed in ["", "My title"] {
            let (mut state, commands, _root, _path) = fixture();
            state.ready = true;
            state.title = typed.into();
            state.detector = Monitor::with_calls(vec![notetaker_detect::DetectedCall {
                app: "Контур.Толк (Chrome)".into(),
                pid: 42,
                meeting_title: Some("AI planning".into()),
            }]);
            state.start_recording();
            let Command::Record(request) = commands.try_recv().unwrap() else {
                panic!("expected recording");
            };
            assert_eq!(
                request.title,
                if typed.is_empty() {
                    "AI planning"
                } else {
                    typed
                }
            );
            assert_eq!(request.app.as_deref(), Some("Контур.Толк (Chrome)"));
        }
    }

    #[test]
    fn accepting_call_prefills_title_without_inventory() {
        let (mut state, commands, _root, _path) = fixture();
        state.ready = true;
        state.call_prompt = Some(CallPrompt::Start {
            app: "Телемост".into(),
            pid: 42,
            meeting_title: Some("Planning".into()),
        });
        state.accept_call();
        let Command::Record(request) = commands.try_recv().unwrap() else {
            panic!("expected recording");
        };
        assert_eq!(request.title, "Planning");
        assert_eq!(request.process_id, Some(42));
    }

    #[test]
    fn dialog_prefill_preserves_typed_titles_and_respects_selected_pid() {
        let (mut state, _commands, _root, _path) = fixture();
        state.detector = Monitor::with_calls(vec![notetaker_detect::DetectedCall {
            app: "Телемост".into(),
            pid: 42,
            meeting_title: Some("Planning".into()),
        }]);
        state.process_id = Some(7);
        state.prefill_call_title();
        assert!(state.title.is_empty());
        state.process_id = None;
        state.prefill_call_title();
        assert_eq!(state.title, "Planning");
        state.title = "Typed".into();
        state.prefill_call_title();
        assert_eq!(state.title, "Typed");
    }

    #[test]
    fn save_failure_prevents_exit_and_keeps_latest_notes() {
        let (mut state, commands, _root, path) = fixture();
        let doc = state.documents.get_mut(&path).unwrap();
        doc.data.notes = "latest notes".into();
        doc.dirty.notes = true;
        state.exit();
        assert!(!state.can_exit());
        assert!(matches!(commands.try_recv().unwrap(), Command::Save(_)));
        state.apply(Event::Saved {
            path: path.clone(),
            result: worker::SaveResult {
                failed: Fields {
                    notes: true,
                    ..Fields::default()
                },
                errors: vec!["disk full".into()],
                ..worker::SaveResult::default()
            },
        });
        assert!(!state.exit_requested);
        assert!(state.documents[&path].dirty.notes);
        assert_eq!(state.documents[&path].data.notes, "latest notes");
        state.exit();
        state.apply(Event::Saved {
            path,
            result: worker::SaveResult::default(),
        });
        assert!(state.can_exit());
    }
    #[test]
    fn final_reload_preserves_notes_and_failed_summary_preserves_previous_text() {
        let (mut state, _commands, _root, path) = fixture();
        let doc = state.documents.get_mut(&path).unwrap();
        doc.data.notes = "unsaved during recording".into();
        doc.dirty.notes = true;
        doc.data.enhanced = "previous summary".into();
        doc.generation = Some("partial".into());
        let mut reload = DocumentData::read(doc.data.meeting.clone()).unwrap();
        reload.transcript.segments.push(Segment {
            id: Uuid::now_v7(),
            speaker: SpeakerId("them".into()),
            channel: Channel::System,
            start_ms: 0,
            end_ms: 1000,
            text: "final".into(),
            source: Source::Final,
            edited: false,
        });
        state.recording.phase = Phase::Finalizing;
        state.apply(Event::RecordDone {
            path: Some(path.clone()),
            document: Some(Box::new(reload)),
            error: None,
        });
        assert_eq!(
            state.documents[&path].data.notes,
            "unsaved during recording"
        );
        assert!(state.documents[&path].dirty.notes);
        assert_eq!(
            state.documents[&path].data.transcript.segments[0].text,
            "final"
        );
        state.apply(Event::Enhanced {
            path: path.clone(),
            result: Err("offline".into()),
        });
        assert_eq!(state.documents[&path].data.enhanced, "previous summary");
        assert!(state.documents[&path].generation.is_none());
    }
    #[test]
    fn committed_enhancement_replaces_old_text_even_with_warnings() {
        let (mut state, _commands, _root, path) = fixture();
        let doc = state.documents.get_mut(&path).unwrap();
        doc.data.enhanced = "old".into();
        let revision = doc
            .data
            .meeting
            .save_enhanced("committed", doc.data.revisions.enhanced)
            .unwrap();
        state.apply(Event::Enhanced {
            path: path.clone(),
            result: Ok(notetaker_llm::EnhanceResult {
                text: "committed".into(),
                revision,
                warnings: vec!["Index unavailable".into()],
                stamp: notetaker_core::SummaryStamp::new(
                    "",
                    &Transcript::default(),
                    "general",
                    "committed",
                ),
            }),
        });
        assert_eq!(state.documents[&path].data.enhanced, "committed");
        assert_eq!(state.documents[&path].data.revisions.enhanced, revision);
        assert_eq!(state.warnings, ["Index unavailable"]);
        assert!(state.error.is_none());
    }
    #[test]
    fn conflicting_enhancement_reloads_external_summary_before_reporting_error() {
        let (mut state, _commands, _root, path) = fixture();
        let meeting = state.documents[&path].data.meeting.clone();
        meeting.write_enhanced("external summary").unwrap();
        state.apply(Event::ReloadEnhanced {
            path: path.clone(),
            value: meeting.load_enhanced().unwrap(),
        });
        state.apply(Event::Enhanced {
            path: path.clone(),
            result: Err("Conflict".into()),
        });
        assert_eq!(state.documents[&path].data.enhanced, "external summary");
        assert!(state.error.as_ref().unwrap().contains("Conflict"));
    }
    #[test]
    fn unsaved_notes_block_llm_and_stale_search_is_ignored() {
        let (mut state, commands, _root, path) = fixture();
        state.documents.get_mut(&path).unwrap().dirty.notes = true;
        state.enhance();
        assert!(commands.try_recv().is_err());
        state.request = 2;
        state.apply(Event::Listed {
            request: 1,
            result: Err("stale failure".into()),
            templates: vec![],
        });
        assert!(state.error.is_none());
    }
    #[test]
    fn auto_summary_respects_mode_setting_and_user_edits() {
        let (state, _commands, _root, path) = fixture();
        let mut data = DocumentData::read(state.documents[&path].data.meeting.clone()).unwrap();
        let mut llm = LlmSettings::default();
        assert!(auto_summary_ready(true, &data, &llm));
        assert!(!auto_summary_ready(false, &data, &llm));
        llm.providers[0].kind = "claude-cli".into();
        assert!(!auto_summary_ready(true, &data, &llm));
        data.meta.mode = notetaker_core::Mode::Hybrid;
        assert!(auto_summary_ready(true, &data, &llm));
        data.enhanced = "old summary without stamp".into();
        assert!(!auto_summary_ready(true, &data, &llm));
        data.meta.summary = Some(notetaker_core::SummaryStamp::new(
            "",
            &data.transcript,
            "general",
            &data.enhanced,
        ));
        assert!(auto_summary_ready(true, &data, &llm));
        data.enhanced.push_str("edited");
        assert!(!auto_summary_ready(true, &data, &llm));
        data.enhanced.clear();
        assert!(!auto_summary_ready(true, &data, &llm));
        llm.defaults.hybrid = "missing".into();
        assert!(!auto_summary_ready(true, &data, &llm));
    }
    #[test]
    fn final_pass_queues_auto_summary_but_failure_does_not() {
        let (mut state, _commands, _root, path) = fixture();
        let meeting = state.documents[&path].data.meeting.clone();
        state.apply(Event::RecordDone {
            path: Some(path.clone()),
            document: Some(Box::new(DocumentData::read(meeting.clone()).unwrap())),
            error: None,
        });
        assert_eq!(state.pending_summary, Some(path.clone()));
        state.pending_summary = None;
        state.apply(Event::RecordDone {
            path: Some(path),
            document: Some(Box::new(DocumentData::read(meeting).unwrap())),
            error: Some("final pass failed".into()),
        });
        assert!(state.pending_summary.is_none());
    }
    #[test]
    fn choosing_cloud_ai_switches_only_the_new_meeting_default() {
        let (mut state, commands, _root, path) = fixture();
        state.settings_draft.library = path.parent().unwrap().to_owned();
        let provider = crate::config::ProviderSettings {
            id: "claude-subscription".into(),
            kind: "claude-cli".into(),
            model: "sonnet".into(),
            ..crate::config::ProviderSettings::default()
        };
        state.apply(Event::Ai(crate::ai::Response {
            index: 0,
            operation: crate::ai::Operation::Default,
            provider,
            result: Ok(Vec::new()),
            latency: Duration::ZERO,
        }));
        assert_eq!(state.settings_draft.mode, notetaker_core::Mode::Hybrid);
        assert_eq!(
            state.documents[&path].data.meta.mode,
            notetaker_core::Mode::Local
        );
        assert_eq!(state.llm_draft.defaults.hybrid, "claude-subscription");
        assert!(!state.warnings.is_empty());
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Settings { .. }
        ));
    }
    #[test]
    fn stale_chip_tracks_notes_and_template_changes_with_cached_status() {
        let (mut state, _commands, _root, path) = fixture();
        let doc = state.documents.get_mut(&path).unwrap();
        doc.data.meta.summary = Some(notetaker_core::SummaryStamp::new(
            &doc.data.notes,
            &doc.data.transcript,
            "general",
            "summary",
        ));
        assert!(!doc.summary_stale());
        doc.data.notes.push_str("edited");
        doc.changed();
        assert!(doc.summary_stale());
        doc.data.notes.clear();
        doc.changed();
        assert!(!doc.summary_stale());
        doc.template = "standup".into();
        assert!(doc.summary_stale());
        doc.template = "general".into();
        assert!(!doc.summary_stale());
    }
    #[test]
    fn auto_summary_waits_for_latest_notes_to_be_saved() {
        let (mut state, commands, _root, path) = fixture();
        state.llm_configured = true;
        state.pending_summary = Some(path.clone());
        let doc = state.documents.get_mut(&path).unwrap();
        doc.data.notes = "latest notes".into();
        doc.dirty.notes = true;
        doc.changed();
        state.poll();
        assert!(state.enhancing.is_none());
        assert_eq!(state.pending_summary, Some(path.clone()));
        state.flush();
        let Command::Save(save) = commands.try_recv().unwrap() else {
            panic!("expected notes save");
        };
        save.meeting
            .write_notes(save.notes.as_deref().unwrap())
            .unwrap();
        let mut revisions = save.revisions;
        revisions.notes = save.meeting.load_notes().unwrap().revision;
        let result = worker::SaveResult {
            revisions,
            ..Default::default()
        };
        state.apply(Event::Saved {
            path: path.clone(),
            result,
        });
        state.poll();
        assert_eq!(state.enhancing, Some(path));
        let Command::Enhance {
            automatic,
            template,
            ..
        } = commands.try_recv().unwrap()
        else {
            panic!("expected automatic generation");
        };
        assert!(automatic);
        assert_eq!(template, "general");
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::*;
    use notetaker_core::calendar::{CalendarEvent, EventLink};
    fn fixture() -> (AppState, Receiver<Command>) {
        let (tx, commands) = std::sync::mpsc::channel();
        let (_events, rx) = std::sync::mpsc::channel();
        let mut state = AppState::with_channels(tx, rx);
        state.ready = true;
        let now = chrono::Utc::now();
        state.calendar_events.push(CalendarEvent {
            link: EventLink {
                uid: "event".into(),
                title: "Календарный план".into(),
                attendees: Vec::new(),
                call_url: None,
            },
            start: now - chrono::Duration::minutes(5),
            end: now + chrono::Duration::minutes(55),
            all_day: false,
            organizer: None,
        });
        (state, commands)
    }
    #[test]
    fn manual_call_prefill_and_explicit_event_preserve_title_rules() {
        for (title, prefilled, expected) in [
            ("", None, "Календарный план"),
            ("Из браузера", Some("Из браузера"), "Календарный план"),
            ("Моё название", None, "Моё название"),
        ] {
            let (mut state, commands) = fixture();
            state.title = title.into();
            state.prefilled_title = prefilled.map(str::to_owned);
            state.start_recording();
            let Command::Record(request) = commands.try_recv().unwrap() else {
                panic!("record expected")
            };
            assert_eq!(request.title, expected);
            assert_eq!(request.event.unwrap().link.uid, "event");
        }
        let (mut state, commands) = fixture();
        let mut event = state.calendar_events[0].clone();
        event.start += chrono::Duration::hours(5);
        event.end += chrono::Duration::hours(5);
        state.start_event(event);
        let Command::Record(request) = commands.try_recv().unwrap() else {
            panic!("record expected")
        };
        assert_eq!(request.title, "Календарный план");
        assert!(request.event.is_some());
    }
    #[test]
    fn calendar_refresh_discards_stale_settings_results_and_runs_every_ten_minutes() {
        let (mut state, commands) = fixture();
        state
            .settings
            .calendars
            .push(notetaker_llm::calendar::Subscription {
                id: "calendar-test".into(),
                name: "Test".into(),
            });
        state.poll();
        let Command::Calendar { generation, .. } = commands.try_recv().unwrap() else {
            panic!("calendar expected")
        };
        state.apply(Event::Calendar {
            generation: generation - 1,
            result: Ok(Vec::new()),
        });
        assert_eq!(state.calendar_events.len(), 1);
        state.apply(Event::Calendar {
            generation,
            result: Ok(Vec::new()),
        });
        assert!(state.calendar_events.is_empty());
        state.poll();
        assert!(commands.try_recv().is_err());
        state.calendar_checked = Some(Instant::now() - Duration::from_secs(601));
        state.poll();
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Calendar { .. }
        ));
    }
}
