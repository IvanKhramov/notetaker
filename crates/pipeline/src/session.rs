use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use chrono::{TimeDelta, Utc};
use notetaker_asr_proto::AudioInput;
use notetaker_capture::{
    AudioFrame, AudioSource, CaptureEvent, CaptureMessage, Channel, SAMPLE_RATE, sample_position,
};
use notetaker_core::{Library, Meeting, MeetingMeta, Segment, Source, SpeakerId};
use uuid::Uuid;

use crate::{
    Asr, AsrConfig, ChunkConfig, WorkerAsr,
    audio::{Chunk, Track},
};

#[derive(Clone, Debug)]
pub struct RecordingConfig {
    pub chunks: ChunkConfig,
    pub diarization: crate::DiarizationConfig,
    /// Total queued/in-flight audio seconds across both channels, not wall time.
    pub max_live_lag: Duration,
    /// None keeps audio forever; zero removes Opus only after a successful final.
    pub retention: Option<Duration>,
    /// Stop after this wall-clock interval without RMS above chunks.silence_level
    /// on either track. Missing packets count as silence. None disables it.
    pub auto_stop_after_silence: Option<Duration>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            chunks: ChunkConfig::default(),
            diarization: crate::DiarizationConfig::default(),
            max_live_lag: Duration::from_secs(60),
            retention: Some(Duration::from_secs(7 * 86400)),
            auto_stop_after_silence: Some(Duration::from_secs(15 * 60)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PipelineEvent {
    LiveSegments(Vec<Segment>),
    Lag {
        seconds: f64,
    },
    LiveDisabled,
    /// Capture stopped for silence; final transcription and Finished follow.
    AutoStopped,
    CaptureEvent(CaptureEvent),
    CaptureIssue(notetaker_core::CaptureIssue),
    Error(String),
    Warning(String),
    FinalPassProgress {
        completed: usize,
        total: usize,
    },
    Finished {
        meeting: Meeting,
    },
}

#[derive(Clone)]
pub struct StopHandle(Arc<AtomicBool>);
impl StopHandle {
    /// Non-blocking; wait for Finished (or call RecordingSession::wait) to join.
    pub(crate) fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
}

pub struct RecordingSession {
    meeting: Meeting,
    stop: StopHandle,
    events: Receiver<PipelineEvent>,
    handle: Option<JoinHandle<Result<()>>>,
}

impl RecordingSession {
    /// Import into the existing system/them channel, preserving the file mtime.
    pub fn import(
        library: Library,
        file: &Path,
        mode: notetaker_core::Mode,
        asr: AsrConfig,
        config: RecordingConfig,
    ) -> Result<Self> {
        Self::import_with_asr(library, file, mode, WorkerAsr::new(asr), config)
    }

    pub fn import_with_asr(
        library: Library,
        file: &Path,
        mode: notetaker_core::Mode,
        asr: impl Asr,
        config: RecordingConfig,
    ) -> Result<Self> {
        let file = file.canonicalize().context("Не удалось открыть запись")?;
        let metadata = fs::metadata(&file)?;
        anyhow::ensure!(metadata.is_file(), "Ожидается файл записи");
        let mut meta = MeetingMeta::new(
            file.file_stem()
                .context("Нет имени файла")?
                .to_string_lossy(),
            metadata
                .modified()
                .context("Не удалось прочитать дату файла")?
                .into(),
        );
        meta.mode = mode;
        let meeting = library.create_meeting(meta)?;
        let (events_tx, events) = mpsc::channel();
        let stop = StopHandle(Arc::new(AtomicBool::new(false)));
        let worker_stop = stop.clone();
        let worker_meeting = meeting.clone();
        let handle = thread::Builder::new()
            .name("import".into())
            .spawn(move || {
                let result = crate::import::run(
                    &worker_meeting,
                    &file,
                    asr,
                    config,
                    worker_stop,
                    &events_tx,
                );
                if let Err(error) = &result {
                    let _ = events_tx.send(PipelineEvent::Error(format!("{error:#}")));
                }
                result
            })?;
        Ok(Self {
            meeting,
            stop,
            events,
            handle: Some(handle),
        })
    }

    pub fn start(
        library: Library,
        meta: MeetingMeta,
        sources: [Box<dyn AudioSource + Send>; 2],
        asr: AsrConfig,
        config: RecordingConfig,
    ) -> Result<Self> {
        Self::start_with_asr(library, meta, sources, WorkerAsr::new(asr), config)
    }

    pub fn start_with_asr(
        library: Library,
        meta: MeetingMeta,
        sources: [Box<dyn AudioSource + Send>; 2],
        asr: impl Asr,
        config: RecordingConfig,
    ) -> Result<Self> {
        config.chunks.validate()?;
        if let Some(retention) = config.retention {
            TimeDelta::from_std(retention).context("retention too large")?;
            Utc::now()
                .checked_add_signed(TimeDelta::from_std(retention)?)
                .context("retention timestamp out of range")?;
        }
        let mut meta = meta;
        meta.ended_at = None;
        meta.audio_expires_at = None; // Retention starts only once recording stops.
        let meeting = library.create_meeting(meta)?;
        let (events_tx, events) = mpsc::channel();
        if let Err(error) = library.sweep_audio(Utc::now()) {
            let _ = events_tx.send(PipelineEvent::Error(format!("audio retention: {error:#}")));
        }
        let stop = StopHandle(Arc::new(AtomicBool::new(false)));
        let worker_stop = stop.clone();
        let worker_meeting = meeting.clone();
        let handle = thread::Builder::new()
            .name("recording".into())
            .spawn(move || {
                let result = record(
                    &worker_meeting,
                    sources,
                    asr,
                    config,
                    worker_stop,
                    &events_tx,
                );
                if let Err(error) = &result {
                    let _ = events_tx.send(PipelineEvent::Error(format!("{error:#}")));
                }
                result
            })?;
        Ok(Self {
            meeting,
            stop,
            events,
            handle: Some(handle),
        })
    }

    pub fn meeting(&self) -> &Meeting {
        &self.meeting
    }
    pub fn events(&self) -> &Receiver<PipelineEvent> {
        &self.events
    }
    pub fn stop_handle(&self) -> StopHandle {
        self.stop.clone()
    }
    pub fn stop(&self) {
        self.stop.stop();
    }

    pub fn wait(mut self) -> Result<Meeting> {
        self.handle
            .take()
            .expect("session handle")
            .join()
            .map_err(|_| anyhow!("recording thread panicked"))??;
        Ok(self.meeting.clone())
    }
}

impl Drop for RecordingSession {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct Sources([Box<dyn AudioSource + Send>; 2]);
impl Sources {
    fn stop(&mut self, events: &Sender<PipelineEvent>) {
        for source in &mut self.0 {
            if let Err(error) = source.stop() {
                let _ = events.send(PipelineEvent::Error(format!("capture stop: {error:#}")));
            }
        }
    }
}
impl Drop for Sources {
    fn drop(&mut self) {
        for source in &mut self.0 {
            let _ = source.stop();
        }
    }
}

struct Queue {
    enabled: AtomicBool,
    pending: AtomicU64,
    max: Duration,
    events: Sender<PipelineEvent>,
}

impl Queue {
    fn disable(&self) {
        if self.enabled.swap(false, Ordering::AcqRel) {
            let _ = self.events.send(PipelineEvent::LiveDisabled);
        }
    }

    fn enqueue(&self, chunk: Chunk, jobs: &Sender<Job>) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let count = chunk.samples.len() as u64;
        let pending = self.pending.fetch_add(count, Ordering::AcqRel) + count;
        let seconds = pending as f64 / f64::from(SAMPLE_RATE);
        let _ = self.events.send(PipelineEvent::Lag { seconds });
        if seconds > self.max.as_secs_f64() || jobs.send(Job::Live(chunk)).is_err() {
            self.pending.fetch_sub(count, Ordering::AcqRel);
            self.disable();
        }
    }
}

enum Job {
    Live(Chunk),
    Final([PathBuf; 2]),
}

fn record(
    meeting: &Meeting,
    sources: [Box<dyn AudioSource + Send>; 2],
    asr: impl Asr,
    config: RecordingConfig,
    stop: StopHandle,
    events: &Sender<PipelineEvent>,
) -> Result<()> {
    let work = meeting.path().join("audio/.work");
    fs::create_dir(&work)?;
    let wavs = [work.join("mic.wav"), work.join("system.wav")];
    let mut tracks = [
        Track::create(
            &meeting.mic_audio_path(),
            &wavs[0],
            Channel::Mic,
            config.chunks.clone(),
        )?,
        Track::create(
            &meeting.system_audio_path(),
            &wavs[1],
            Channel::System,
            config.chunks.clone(),
        )?,
    ];
    let queue = Arc::new(Queue {
        enabled: AtomicBool::new(true),
        pending: AtomicU64::new(0),
        max: config.max_live_lag,
        events: events.clone(),
    });
    let (jobs_tx, jobs_rx) = mpsc::channel();
    let asr_queue = Arc::clone(&queue);
    let asr_meeting = meeting.clone();
    let diarization = config.diarization.clone();
    let asr_handle = thread::Builder::new()
        .name("session-asr".into())
        .spawn(move || run_asr(asr, &asr_meeting, jobs_rx, &asr_queue, &diarization))?;
    // Always join ASR, including disk/capture errors. Dropping jobs_tx cancels the
    // consumer rather than leaving a detached worker holding the model in RAM.
    let captured = (|| -> Result<()> {
        let mut sources = Sources(sources);
        let (tx, rx) = mpsc::sync_channel(256);
        let origin = Instant::now();
        let mut stopped = [false; 2];
        let mut silence =
            SilenceTimer::new(config.auto_stop_after_silence, config.chunks.silence_level);
        for (i, source) in sources.0.iter_mut().enumerate() {
            if let Err(error) = source.start(tx.clone()) {
                stopped[i] = true;
                capture_issue(
                    meeting,
                    if i == 0 {
                        Channel::Mic
                    } else {
                        Channel::System
                    },
                    0,
                    format!("capture start: {error:#}"),
                    events,
                )?;
            }
        }
        drop(tx);
        let mut explicit_end = None;
        loop {
            if stop.0.load(Ordering::Acquire) {
                explicit_end = Some(sample_position(origin.elapsed()));
                sources.stop(events);
                for message in rx.try_iter() {
                    capture(
                        message,
                        meeting,
                        &mut tracks,
                        &queue,
                        &jobs_tx,
                        &mut stopped,
                    )?;
                }
                break;
            }
            if stopped.iter().all(|s| *s) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(message) => {
                    if let CaptureMessage::Frame(frame) = &message {
                        silence.observe(origin.elapsed(), frame);
                    }
                    capture(
                        message,
                        meeting,
                        &mut tracks,
                        &queue,
                        &jobs_tx,
                        &mut stopped,
                    )?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if silence.expired(origin.elapsed())
                && stop
                    .0
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                let _ = events.send(PipelineEvent::AutoStopped);
            }
        }
        sources.stop(events);
        let ended = Utc::now();
        let end = tracks
            .iter()
            .map(Track::position)
            .max()
            .unwrap_or(0)
            .max(explicit_end.unwrap_or(0));
        for track in &mut tracks {
            track.pad_to(end, queue.enabled.load(Ordering::Acquire), |c| {
                queue.enqueue(c, &jobs_tx)
            })?;
            track
                .chunker
                .flush(track.channel, |c| queue.enqueue(c, &jobs_tx));
        }
        for track in tracks {
            track.finish()?;
        }
        let mut meta = meeting.read_meta()?;
        meta.ended_at = Some(ended);
        meta.audio_expires_at = config
            .retention
            .map(|r| -> Result<_> {
                ended
                    .checked_add_signed(TimeDelta::from_std(r)?)
                    .context("retention timestamp out of range")
            })
            .transpose()?;
        meeting.write_meta(&meta)?;
        jobs_tx
            .send(Job::Final(wavs.clone()))
            .context("ASR thread stopped")?;
        Ok(())
    })();
    drop(jobs_tx);
    let recognized = asr_handle
        .join()
        .map_err(|_| anyhow!("ASR thread panicked"))?;
    captured?;
    recognized?;
    for wav in wavs {
        fs::remove_file(wav)?;
    }
    fs::remove_dir(work)?;
    if config.retention == Some(Duration::ZERO) {
        fs::remove_file(meeting.mic_audio_path())?;
        fs::remove_file(meeting.system_audio_path())?;
    }
    let _ = events.send(PipelineEvent::Finished {
        meeting: meeting.clone(),
    });
    Ok(())
}

/// A single timer since sound on either channel is equivalent to requiring both
/// channels to be quiet. Wall time also covers WASAPI's no-packet silence.
struct SilenceTimer {
    timeout: Option<Duration>,
    level: f32,
    last_sound: Duration,
}
impl SilenceTimer {
    fn new(timeout: Option<Duration>, level: f32) -> Self {
        Self {
            timeout,
            level,
            last_sound: Duration::ZERO,
        }
    }

    fn observe(&mut self, now: Duration, frame: &AudioFrame) {
        if frame.samples.is_empty() {
            return;
        }
        let rms =
            (frame.samples.iter().map(|s| s * s).sum::<f32>() / frame.samples.len() as f32).sqrt();
        // Invalid input must not be mistaken for evidence of silence.
        if !rms.is_finite() || rms > self.level {
            self.last_sound = now;
        }
    }

    fn expired(&self, now: Duration) -> bool {
        self.timeout
            .is_some_and(|timeout| now.saturating_sub(self.last_sound) >= timeout)
    }
}

fn capture(
    message: CaptureMessage,
    meeting: &Meeting,
    tracks: &mut [Track; 2],
    queue: &Queue,
    jobs: &Sender<Job>,
    stopped: &mut [bool; 2],
) -> Result<()> {
    match message {
        CaptureMessage::Frame(frame) => {
            let index = channel_index(frame.channel);
            tracks[index].frame(frame, queue.enabled.load(Ordering::Acquire), |c| {
                queue.enqueue(c, jobs)
            })?;
        }
        CaptureMessage::Event(event) => {
            if let CaptureEvent::Error { channel, message } = &event {
                capture_issue(
                    meeting,
                    *channel,
                    tracks[channel_index(*channel)].position() * 1000 / u64::from(SAMPLE_RATE),
                    message.clone(),
                    &queue.events,
                )?;
            }
            if let CaptureEvent::Stopped { channel } = &event {
                stopped[channel_index(*channel)] = true;
            }
            let _ = queue.events.send(PipelineEvent::CaptureEvent(event));
        }
    }
    Ok(())
}

fn capture_issue(
    meeting: &Meeting,
    channel: Channel,
    from_ms: u64,
    reason: String,
    events: &Sender<PipelineEvent>,
) -> Result<()> {
    let issue = notetaker_core::CaptureIssue {
        channel: match channel {
            Channel::Mic => notetaker_core::Channel::Mic,
            Channel::System => notetaker_core::Channel::System,
        },
        from_ms,
        reason,
    };
    let mut meta = meeting.read_meta()?;
    meta.capture_issues.push(issue.clone());
    meeting.write_meta(&meta)?;
    let _ = events.send(PipelineEvent::CaptureIssue(issue));
    Ok(())
}

fn channel_index(channel: Channel) -> usize {
    match channel {
        Channel::Mic => 0,
        Channel::System => 1,
    }
}

struct Recognizer<A> {
    asr: A,
    ready: bool,
    restarted: bool,
    failed: Option<String>,
}
impl<A: Asr> Recognizer<A> {
    fn transcribe(
        &mut self,
        input: AudioInput,
        channel: Channel,
        offset: u64,
        queue: &Queue,
    ) -> Result<Vec<notetaker_asr_proto::Segment>> {
        if let Some(error) = &self.failed {
            return Err(anyhow!(error.clone()));
        }
        loop {
            let result = (|| {
                if !self.ready {
                    self.asr.start()?;
                    if let Some(notice) = self.asr.take_notice() {
                        let _ = queue.events.send(PipelineEvent::Warning(notice));
                    }
                    self.ready = true;
                }
                if let AudioInput::WavPath(path) = &input {
                    let wav = hound::WavReader::open(path)?;
                    let duration = Duration::from_secs_f64(
                        f64::from(wav.duration()) / f64::from(wav.spec().sample_rate),
                    );
                    self.asr.set_timeout(
                        Duration::from_secs(300).saturating_add(duration.saturating_mul(4)),
                    );
                }
                self.asr.transcribe(input.clone(), channel, offset)
            })();
            match result {
                Ok(segments) => return Ok(segments),
                Err(error) => {
                    let message = format!("ASR: {error:#}");
                    let _ = queue.events.send(PipelineEvent::Error(message.clone()));
                    if self.restarted {
                        self.failed = Some(message);
                        return Err(error);
                    }
                    self.restarted = true;
                    self.ready = false;
                }
            }
        }
    }
}

fn run_asr(
    asr: impl Asr,
    meeting: &Meeting,
    jobs: Receiver<Job>,
    queue: &Queue,
    diarization: &crate::DiarizationConfig,
) -> Result<()> {
    let mut asr = Recognizer {
        asr,
        ready: false,
        restarted: false,
        failed: None,
    };
    for job in jobs {
        match job {
            Job::Live(chunk) => {
                let count = chunk.samples.len() as u64;
                if queue.enabled.load(Ordering::Acquire) {
                    match asr.transcribe(
                        AudioInput::pcm(&chunk.samples),
                        chunk.channel,
                        chunk.start * 1000 / u64::from(SAMPLE_RATE),
                        queue,
                    ) {
                        Ok(segments) if queue.enabled.load(Ordering::Acquire) => {
                            let mut segments = convert(segments, chunk.channel, Source::Live);
                            meeting.update_transcript(|transcript| {
                                transcript.segments.extend(segments.clone());
                                sort(&mut transcript.segments);
                                crate::echo::suppress_echo(&mut transcript.segments);
                                segments.retain(|segment| {
                                    transcript
                                        .segments
                                        .iter()
                                        .any(|retained| retained.id == segment.id)
                                });
                            })?;
                            let _ = queue.events.send(PipelineEvent::LiveSegments(segments));
                        }
                        Ok(_) => {}
                        Err(_) => queue.disable(),
                    }
                }
                let pending = queue.pending.fetch_sub(count, Ordering::AcqRel) - count;
                let _ = queue.events.send(PipelineEvent::Lag {
                    seconds: pending as f64 / f64::from(SAMPLE_RATE),
                });
            }
            Job::Final(wavs) => {
                let mut final_segments = Vec::new();
                let _ = queue.events.send(PipelineEvent::FinalPassProgress {
                    completed: 0,
                    total: 2,
                });
                for (i, (wav, channel)) in
                    wavs.iter().zip([Channel::Mic, Channel::System]).enumerate()
                {
                    final_segments.extend(convert(
                        asr.transcribe(AudioInput::WavPath(wav.clone()), channel, 0, queue)?,
                        channel,
                        Source::Final,
                    ));
                    let _ = queue.events.send(PipelineEvent::FinalPassProgress {
                        completed: i + 1,
                        total: 2,
                    });
                }
                // Reload immediately before replacement to respect UI edits made
                // during capture or while the full-file requests were running.
                meeting.update_transcript(|transcript| {
                    transcript.segments =
                        replace_final(std::mem::take(&mut transcript.segments), final_segments);
                    crate::echo::suppress_echo(&mut transcript.segments);
                })?;
                crate::diarization::final_pass(
                    meeting,
                    &mut asr.asr,
                    &wavs[1],
                    Some(&wavs[0]),
                    diarization,
                    &queue.events,
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn convert(
    segments: Vec<notetaker_asr_proto::Segment>,
    channel: Channel,
    source: Source,
) -> Vec<Segment> {
    let (channel, speaker) = match channel {
        Channel::Mic => (notetaker_core::Channel::Mic, "me"),
        Channel::System => (notetaker_core::Channel::System, "them"),
    };
    let mut segments: Vec<_> = segments
        .into_iter()
        .map(|s| Segment {
            id: Uuid::now_v7(),
            speaker: SpeakerId(speaker.into()),
            channel,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            text: s.text,
            source,
            edited: false,
            lang: s.lang,
        })
        .collect();
    sort(&mut segments);
    segments
}

fn sort(segments: &mut [Segment]) {
    segments.sort_by_key(|s| {
        (
            s.start_ms,
            s.end_ms,
            match s.channel {
                notetaker_core::Channel::Mic => 0,
                notetaker_core::Channel::System => 1,
            },
        )
    });
}

pub(crate) fn replace_final(existing: Vec<Segment>, final_segments: Vec<Segment>) -> Vec<Segment> {
    let mut edited: Vec<_> = existing.into_iter().filter(|s| s.edited).collect();
    let retained: Vec<_> = final_segments
        .into_iter()
        .filter(|s| {
            let mut ranges: Vec<_> = edited
                .iter()
                .filter(|e| {
                    e.channel == s.channel && s.start_ms < e.end_ms && e.start_ms < s.end_ms
                })
                .map(|e| (e.start_ms.max(s.start_ms), e.end_ms.min(s.end_ms)))
                .collect();
            ranges.sort_unstable();
            let mut covered = 0;
            let mut end = s.start_ms;
            for (start, next_end) in ranges {
                covered += next_end.saturating_sub(start.max(end));
                end = end.max(next_end);
            }
            // Boundaries move between live and final ASR. Keep final evidence
            // unless edits cover a strict majority; a small boundary overlap
            // must not discard an entire phrase. Count the union, not duplicates.
            covered <= s.end_ms.saturating_sub(s.start_ms) / 2
        })
        .collect();
    edited.extend(retained);
    sort(&mut edited);
    edited
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_timer_resets_for_either_channel_and_can_be_disabled() {
        let at = Duration::from_secs;
        let mut timer = SilenceTimer::new(Some(at(10)), 0.01);
        assert!(!timer.expired(at(9)));
        for (channel, now) in [(Channel::Mic, 9), (Channel::System, 18)] {
            timer.observe(
                at(now),
                &AudioFrame {
                    channel,
                    samples: vec![0.1; 320],
                    captured_at: at(now),
                },
            );
            assert!(!timer.expired(at(now + 9)));
        }
        timer.observe(
            at(27),
            &AudioFrame {
                channel: Channel::Mic,
                samples: vec![0.001; 320],
                captured_at: at(27),
            },
        );
        assert!(timer.expired(at(28)));
        assert!(!SilenceTimer::new(None, 0.01).expired(at(10000)));
    }

    #[test]
    fn final_replacement_preserves_other_channels_and_minor_overlaps() {
        let segment = |start, end, source, edited| {
            let mut s = convert(
                vec![notetaker_asr_proto::Segment {
                    start_ms: start,
                    end_ms: end,
                    text: "text".into(),
                    lang: None,
                }],
                Channel::Mic,
                source,
            )
            .remove(0);
            s.edited = edited;
            s
        };
        let edit = segment(100, 200, Source::Live, true);
        let final_edit = segment(500, 600, Source::Final, true);
        let mut other_channel = segment(150, 170, Source::Final, false);
        other_channel.channel = notetaker_core::Channel::System;
        let result = replace_final(
            vec![
                segment(0, 100, Source::Live, false),
                edit.clone(),
                final_edit.clone(),
            ],
            vec![
                segment(0, 100, Source::Final, false),
                segment(90, 110, Source::Final, false),
                other_channel.clone(),
                segment(110, 190, Source::Final, false),
                segment(200, 300, Source::Final, false),
                segment(490, 510, Source::Final, false),
            ],
        );
        assert_eq!(result.len(), 7);
        assert!(result.contains(&edit));
        assert!(result.contains(&final_edit));
        assert!(result.contains(&other_channel));
        assert!(result.iter().any(|s| s.start_ms == 90)); // Exactly half survives.
        assert!(result.iter().any(|s| s.start_ms == 490));
        assert!(!result.iter().any(|s| s.start_ms == 110)); // Majority edited.
        assert!(result.iter().any(|s| s.start_ms == 200)); // Touching endpoints survive.
    }
    #[test]
    fn final_overlap_counts_union_and_only_strict_majority() {
        let base = convert(
            vec![notetaker_asr_proto::Segment {
                start_ms: 0,
                end_ms: 100,
                text: "final".into(),
                lang: None,
            }],
            Channel::Mic,
            Source::Final,
        )
        .remove(0);
        let edit = |start, end| Segment {
            id: Uuid::now_v7(),
            start_ms: start,
            end_ms: end,
            edited: true,
            ..base.clone()
        };
        for (edits, retained) in [
            (vec![edit(0, 30), edit(20, 40)], true), // Overlap is not counted twice.
            (vec![edit(0, 25), edit(75, 100)], true),
            (vec![edit(0, 26), edit(75, 100)], false),
        ] {
            assert_eq!(
                replace_final(edits, vec![base.clone()]).contains(&base),
                retained
            );
        }
    }
}
