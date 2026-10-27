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

pub struct Document {
    pub data: DocumentData,
    pub dirty: Fields,
    pub saving: bool,
    pub changed_at: Instant,
    pub generation: Option<String>,
    pub template: String,
}
impl Document {
    fn new(data: DocumentData) -> Self {
        let template = data
            .meta
            .template
            .clone()
            .unwrap_or_else(|| "general".into());
        Self {
            data,
            dirty: Fields::default(),
            saving: false,
            changed_at: Instant::now(),
            generation: None,
            template,
        }
    }
    pub fn changed(&mut self) {
        self.changed_at = Instant::now();
    }
    fn save(&mut self) -> Option<Save> {
        if self.saving || !self.dirty.any() {
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
        if self.tx.send(command).is_err() {
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
            .any(|doc| doc.saving || doc.dirty.any())
    }
    pub fn start_recording(&mut self) {
        self.start_recording_with_app(None);
    }
    pub fn accept_call(&mut self) {
        match self.call_prompt.clone() {
            Some(CallPrompt::Start { app, pid }) => {
                self.process_id = Some(pid);
                self.record_model = self.settings.model.clone();
                self.record_mode = self.settings.mode;
                self.title.clear();
                self.start_recording_with_app(Some(app));
            }
            Some(CallPrompt::Stop { .. }) => {
                self.recording.request_stop();
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
    fn start_recording_with_app(&mut self, app_override: Option<String>) {
        if !self.ready
            || self.recording.busy()
            || self.downloading
            || self.settings_pending
            || self.exit_requested
        {
            return;
        }
        let app = app_override.or_else(|| {
            self.inventory
                .sessions
                .iter()
                .find(|s| Some(s.pid) == self.process_id)
                .map(|s| s.exe.clone().unwrap_or_else(|| s.name.clone()))
        });
        let mut settings = self.settings.clone();
        settings.model = self.record_model.clone();
        settings.mode = self.record_mode;
        let title = if self.title.trim().is_empty() {
            chrono::Local::now()
                .format("Встреча %d.%m.%Y %H:%M")
                .to_string()
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
            process_id: self.process_id,
            app,
        }));
    }
    pub fn toggle_recording(&mut self) {
        if self.recording.busy() {
            self.recording.request_stop();
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
        self.flush();
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        document.generation = Some(String::new());
        let command = Command::Enhance {
            meeting: document.data.meeting.clone(),
            template: document.template.clone(),
        };
        self.enhancing = Some(path);
        self.send(command);
    }
    pub fn download(&mut self, model: String) {
        if self.downloading || self.recording.busy() {
            return;
        }
        self.downloading = true;
        self.download_messages.clear();
        self.download_messages
            .push_back(format!("Загрузка {model} и Silero VAD…"));
        self.send(Command::Download(model));
    }
    pub fn save_settings(&mut self) {
        if self.recording.busy()
            || self.enhancing.is_some()
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
        });
    }
    pub fn exit(&mut self) {
        self.exit_requested = true;
        self.recording.request_stop();
        self.flush();
    }
    pub fn can_exit(&self) -> bool {
        self.exit_requested
            && !self.recording.busy()
            && self.enhancing.is_none()
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
        // Keep only active/unsaved documents; revisiting a meeting reloads files
        // and opening a large library does not accumulate transcripts in RAM.
        let selected = &self.selected;
        let recording = &self.recording;
        let enhancing = &self.enhancing;
        self.documents.retain(|path, doc| {
            selected.as_ref() == Some(path)
                || (recording.busy() && recording.path.as_ref() == Some(path))
                || enhancing.as_ref() == Some(path)
                || doc.dirty.any()
                || doc.saving
        });
    }
    fn poll_detection(&mut self) {
        self.detector.configure(
            self.ready && self.settings.detect_calls,
            self.settings.detect_browsers,
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
        if matches!(self.call_prompt, Some(CallPrompt::Stop { .. }))
            && (!matches!(self.recording.phase, Phase::Starting | Phase::Recording)
                || self.recording.stop_requested)
        {
            self.call_prompt = None;
        }
    }
    fn apply(&mut self, event: Event) {
        match event {
            Event::Boot {
                settings,
                llm,
                warnings,
                needs_model,
            } => {
                self.offer_model_download = needs_model;
                self.settings = settings.clone();
                self.settings_draft = settings.clone();
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
            Event::Started { document, stop } => {
                let path = document.meeting.path().to_owned();
                self.selected = Some(path.clone());
                self.tab = Tab::Notes;
                self.recording.path = Some(path.clone());
                self.recording.stop = Some(stop);
                self.recording.started = Some(Instant::now());
                self.recording.phase = Phase::Recording;
                if self.recording.stop_requested {
                    self.recording.request_stop();
                }
                self.documents.insert(path, Document::new(*document));
                self.refresh();
            }
            Event::Pipeline(event) => {
                self.recording.apply(&event);
                if let PipelineEvent::Error(message)
                | PipelineEvent::CaptureEvent(CaptureEvent::Error { message, .. }) = &event
                {
                    self.error = Some(message.clone());
                }
                if self.recording.phase == Phase::Finalizing {
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
                }
            }
            Event::RecordDone {
                path,
                document,
                error,
            } => {
                if let (Some(path), Some(data)) = (path, document)
                    && let Some(doc) = self.documents.get_mut(&path)
                {
                    doc.data.meta = data.meta;
                    doc.data.transcript = data.transcript;
                    doc.data.revisions.transcript = data.revisions.transcript;
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
                        }
                        Err(error) => {
                            self.error = Some(error);
                            self.exit_requested = false;
                        }
                    }
                }
                self.refresh();
            }
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
                        let changed_library = self.settings.library != settings.library;
                        if self.settings.detect_calls != settings.detect_calls
                            || self.settings.detect_browsers != settings.detect_browsers
                        {
                            self.call_prompt = None;
                        }
                        self.record_model = settings.model.clone();
                        self.record_mode = settings.mode;
                        self.settings = settings.clone();
                        self.settings_draft = settings;
                        self.llm_draft = llm;
                        if changed_library {
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
        });
        state.accept_call();
        let Command::Record(request) = commands.try_recv().unwrap() else {
            panic!("expected recording");
        };
        assert_eq!(request.process_id, Some(42));
        assert_eq!(request.app.as_deref(), Some("Teams"));
        assert_eq!(request.settings.model, "gigaam-v3");
        assert_eq!(request.settings.mode, state.settings.mode);
        assert_ne!(request.title, "old title");
        assert_eq!(state.recording.process_id, Some(42));
        assert!(state.call_prompt.is_none());
        state.call_prompt = Some(CallPrompt::Stop { pid: 42 });
        state.dismiss_call();
        assert!(!state.recording.stop_requested);
        assert!(state.call_prompt.is_none());
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
}
