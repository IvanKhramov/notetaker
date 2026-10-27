use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use anyhow::{Result, bail};
use chrono::Utc;
use notetaker_asr_proto::{AudioInput, Segment as AsrSegment};
use notetaker_capture::{AudioSource, Channel, FileSource, PlaybackPace};
use notetaker_core::{Library, MeetingMeta, Source};
use notetaker_pipeline::{Asr, ChunkConfig, PipelineEvent, RecordingConfig, RecordingSession};

#[derive(Debug)]
struct Call {
    channel: Channel,
    offset: u64,
    len: usize,
    final_pass: bool,
}
#[derive(Default)]
struct Stats {
    starts: usize,
    attempts: usize,
    calls: Vec<Call>,
}
struct Fake {
    stats: Arc<Mutex<Stats>>,
    crashes: Vec<usize>,
    gate: Option<mpsc::Receiver<()>>,
}
impl Asr for Fake {
    fn start(&mut self) -> Result<()> {
        self.stats.lock().unwrap().starts += 1;
        if let Some(gate) = self.gate.take() {
            gate.recv_timeout(Duration::from_secs(10))?;
        }
        Ok(())
    }
    fn transcribe(
        &mut self,
        input: AudioInput,
        channel: Channel,
        offset: u64,
    ) -> Result<Vec<AsrSegment>> {
        let mut stats = self.stats.lock().unwrap();
        stats.attempts += 1;
        if self.crashes.contains(&stats.attempts) {
            bail!("worker exited");
        }
        let (len, final_pass) = match input {
            AudioInput::Pcm {
                samples_f32_16k_mono,
            } => (
                AudioInput::decode_pcm(&samples_f32_16k_mono).unwrap().len(),
                false,
            ),
            AudioInput::WavPath(path) => {
                let wav = hound::WavReader::open(path)?;
                assert_eq!(wav.spec().sample_rate, 16000);
                assert!(matches!(wav.spec().bits_per_sample, 16 | 32));
                (wav.duration() as usize, true)
            }
        };
        stats.calls.push(Call {
            channel,
            offset,
            len,
            final_pass,
        });
        let duration = len as u64 / 16;
        // Deliberately unsorted worker output.
        Ok(vec![
            AsrSegment {
                start_ms: offset + duration / 2,
                end_ms: offset + duration,
                text: format!("{channel:?} second"),
            },
            AsrSegment {
                start_ms: offset,
                end_ms: offset + duration / 2,
                text: format!("{channel:?} first"),
            },
        ])
    }
}

fn wav(path: &Path, count: usize) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for i in 0..count {
        writer
            .write_sample(if i % 1600 < 960 { 10000_i16 } else { 0 })
            .unwrap();
    }
    writer.finalize().unwrap();
}
fn config() -> RecordingConfig {
    RecordingConfig {
        chunks: ChunkConfig {
            min: Duration::from_millis(40),
            max: Duration::from_millis(100),
            silence: Duration::from_millis(20),
            silence_level: 0.01,
        },
        max_live_lag: Duration::from_secs(60),
        ..RecordingConfig::default()
    }
}
fn start(
    dir: &Path,
    config: RecordingConfig,
    crashes: Vec<usize>,
    gate: Option<mpsc::Receiver<()>>,
) -> (RecordingSession, Arc<Mutex<Stats>>) {
    wav(&dir.join("mic.wav"), 6400);
    wav(&dir.join("system.wav"), 3200);
    let sources: [Box<dyn AudioSource + Send>; 2] = [
        Box::new(FileSource::new(
            dir.join("mic.wav"),
            Channel::Mic,
            PlaybackPace::Accelerated,
        )),
        Box::new(FileSource::new(
            dir.join("system.wav"),
            Channel::System,
            PlaybackPace::Accelerated,
        )),
    ];
    let stats = Arc::new(Mutex::new(Stats::default()));
    let session = RecordingSession::start_with_asr(
        Library::open(dir.join("library")).unwrap(),
        MeetingMeta::new("Test", Utc::now()),
        sources,
        Fake {
            stats: Arc::clone(&stats),
            crashes,
            gate,
        },
        config,
    )
    .unwrap();
    (session, stats)
}
fn drain(session: &RecordingSession) -> Vec<PipelineEvent> {
    let mut events = Vec::new();
    while let Ok(event) = session.events().recv_timeout(Duration::from_secs(15)) {
        let finished = matches!(event, PipelineEvent::Finished { .. });
        events.push(event);
        if finished {
            return events;
        }
    }
    events
}

#[test]
fn two_tracks_are_padded_chunks_are_contiguous_and_live_and_final_are_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let (session, stats) = start(dir.path(), config(), vec![], None);
    let events = drain(&session);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::LiveSegments(_)))
    );
    assert!(!events.iter().any(|e| matches!(e, PipelineEvent::Error(_))));
    let meeting = session.wait().unwrap();
    let transcript = meeting.read_transcript().unwrap();
    assert!(
        transcript
            .segments
            .windows(2)
            .all(|s| s[0].start_ms <= s[1].start_ms)
    );
    assert!(
        transcript
            .segments
            .iter()
            .all(|s| s.source == Source::Final)
    );
    assert!(transcript.segments.iter().any(|s| s.speaker.0 == "me"));
    assert!(transcript.segments.iter().any(|s| s.speaker.0 == "them"));
    assert!(!meeting.path().join("audio/.work").exists());
    assert!(meeting.mic_audio_path().is_file() && meeting.system_audio_path().is_file());
    let stats = stats.lock().unwrap();
    assert_eq!(stats.starts, 1);
    for channel in [Channel::Mic, Channel::System] {
        let mut end = 0;
        for call in stats
            .calls
            .iter()
            .filter(|c| c.channel == channel && !c.final_pass)
        {
            assert_eq!(call.offset, end / 16);
            end += call.len as u64;
        }
        assert_eq!(
            end, 6400,
            "no omitted/duplicated samples, including shorter track's silence"
        );
        let final_call = stats
            .calls
            .iter()
            .find(|c| c.channel == channel && c.final_pass)
            .unwrap();
        assert_eq!(final_call.offset, 0);
        assert_eq!(final_call.len, 6400);
    }
    let first_final = stats.calls.iter().position(|c| c.final_pass).unwrap();
    assert!(stats.calls[first_final..].iter().all(|c| c.final_pass));
    let mut live = events
        .into_iter()
        .flat_map(|e| match e {
            PipelineEvent::LiveSegments(s) => s,
            _ => vec![],
        })
        .collect::<Vec<_>>();
    live.sort_by_key(|s| s.start_ms);
    for channel in [
        notetaker_core::Channel::Mic,
        notetaker_core::Channel::System,
    ] {
        let channel: Vec<_> = live.iter().filter(|s| s.channel == channel).collect();
        assert!(channel.windows(2).all(|s| s[0].end_ms <= s[1].start_ms));
    }
}

#[test]
fn lag_disables_live_but_keeps_recording_and_finishes_both_tracks() {
    let dir = tempfile::tempdir().unwrap();
    let (release, gate) = mpsc::channel();
    let mut config = config();
    config.max_live_lag = Duration::from_millis(100);
    let (session, stats) = start(dir.path(), config, vec![], Some(gate));
    let mut disabled = 0;
    let mut high_lag = false;
    loop {
        match session
            .events()
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            PipelineEvent::Lag { seconds } if seconds > 0.1 => high_lag = true,
            PipelineEvent::LiveDisabled => {
                disabled += 1;
                break;
            }
            _ => {}
        }
    }
    release.send(()).unwrap();
    let rest = drain(&session);
    disabled += rest
        .iter()
        .filter(|e| matches!(e, PipelineEvent::LiveDisabled))
        .count();
    assert!(high_lag);
    assert_eq!(disabled, 1);
    let meeting = session.wait().unwrap();
    assert!(
        meeting
            .read_transcript()
            .unwrap()
            .segments
            .iter()
            .all(|s| s.source == Source::Final)
    );
    assert_eq!(
        stats
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.final_pass && c.len == 6400)
            .count(),
        2
    );
}

#[test]
fn retention_is_measured_from_stop_and_zero_deletes_only_after_final() {
    for retention in [
        None,
        Some(Duration::ZERO),
        Some(Duration::from_secs(7 * 86400)),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.retention = retention;
        let (session, _) = start(dir.path(), config, vec![], None);
        drain(&session);
        let meeting = session.wait().unwrap();
        let meta = meeting.read_meta().unwrap();
        let ended = meta.ended_at.unwrap();
        assert!(ended >= meta.started_at);
        assert_eq!(
            meta.audio_expires_at,
            retention.map(|r| ended + chrono::TimeDelta::from_std(r).unwrap())
        );
        assert_eq!(
            meeting.mic_audio_path().exists(),
            retention != Some(Duration::ZERO)
        );
        assert_eq!(
            meeting.system_audio_path().exists(),
            retention != Some(Duration::ZERO)
        );
        assert!(!meeting.read_transcript().unwrap().segments.is_empty());
    }
}

#[test]
fn crashed_worker_restarts_once_and_recording_survives() {
    let dir = tempfile::tempdir().unwrap();
    let (session, stats) = start(dir.path(), config(), vec![1], None);
    let events = drain(&session);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, PipelineEvent::Error(_)))
            .count(),
        1
    );
    session.wait().unwrap();
    assert_eq!(stats.lock().unwrap().starts, 2);
}

#[test]
fn second_crash_does_not_restart_and_preserves_audio_and_work_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let (session, stats) = start(dir.path(), config(), vec![1, 2], None);
    let meeting = session.meeting().clone();
    let events = drain(&session);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PipelineEvent::Finished { .. }))
    );
    assert!(session.wait().is_err());
    assert_eq!(stats.lock().unwrap().starts, 2);
    assert!(meeting.mic_audio_path().is_file());
    assert!(meeting.path().join("audio/.work/mic.wav").is_file());
    assert!(meeting.read_meta().unwrap().ended_at.is_some());
}

#[test]
fn edits_saved_during_recording_survive_the_real_replacement_path() {
    let dir = tempfile::tempdir().unwrap();
    let (release, gate) = mpsc::channel();
    let (session, _) = start(dir.path(), config(), vec![], Some(gate));
    let meeting = session.meeting().clone();
    let edited = notetaker_core::Segment {
        id: meeting.read_meta().unwrap().id,
        speaker: notetaker_core::SpeakerId("me".into()),
        channel: notetaker_core::Channel::Mic,
        start_ms: 50,
        end_ms: 150,
        text: "User correction".into(),
        source: Source::Live,
        edited: true,
    };
    let mut transcript = meeting.read_transcript().unwrap();
    transcript.segments.push(edited.clone());
    meeting.write_transcript(&transcript).unwrap();
    release.send(()).unwrap();
    drain(&session);
    session.wait().unwrap();
    let transcript = meeting.read_transcript().unwrap();
    assert!(transcript.segments.contains(&edited));
    assert!(
        transcript
            .segments
            .iter()
            .filter(|s| !s.edited)
            .all(|s| s.source == Source::Final)
    );
    // The edit covers exactly half of the first 200 ms mic phrase, and must
    // never suppress the simultaneous system phrase.
    for channel in [
        notetaker_core::Channel::Mic,
        notetaker_core::Channel::System,
    ] {
        assert!(
            transcript
                .segments
                .iter()
                .any(|s| !s.edited && s.channel == channel && s.start_ms == 0)
        );
    }
    assert!(
        transcript
            .segments
            .iter()
            .any(|s| !s.edited && s.end_ms == 400)
    );
}

#[test]
fn explicit_stop_finishes_a_realtime_source_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    wav(&dir.path().join("input.wav"), 160000);
    let sources: [Box<dyn AudioSource + Send>; 2] = [Channel::Mic, Channel::System].map(|c| {
        Box::new(FileSource::new(
            dir.path().join("input.wav"),
            c,
            PlaybackPace::Realtime,
        )) as Box<dyn AudioSource + Send>
    });
    let stats = Arc::new(Mutex::new(Stats::default()));
    let session = RecordingSession::start_with_asr(
        Library::open(dir.path().join("library")).unwrap(),
        MeetingMeta::new("Stop", Utc::now()),
        sources,
        Fake {
            stats: stats.clone(),
            crashes: vec![],
            gate: None,
        },
        config(),
    )
    .unwrap();
    // Wait until capture has actually produced enough audio for one live chunk.
    loop {
        if matches!(
            session
                .events()
                .recv_timeout(Duration::from_secs(10))
                .unwrap(),
            PipelineEvent::LiveSegments(_)
        ) {
            break;
        }
    }
    session.stop();
    session.stop_handle().stop();
    drain(&session);
    let meeting = session.wait().unwrap();
    assert!(meeting.read_meta().unwrap().ended_at.is_some());
    assert!(
        stats
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.final_pass)
            .all(|c| c.len < 160000)
    );
}

#[test]
fn loopback_fallback_is_forwarded_to_the_ui_event_stream() {
    struct Events(Channel);
    impl AudioSource for Events {
        fn start(&mut self, tx: mpsc::SyncSender<notetaker_capture::CaptureMessage>) -> Result<()> {
            use notetaker_capture::{CaptureEvent, CaptureMessage, FallbackReason};
            if self.0 == Channel::System {
                tx.send(CaptureMessage::Event(CaptureEvent::Fallback {
                    at: Duration::from_secs(10),
                    reason: FallbackReason::ProcessSilent,
                }))?;
            }
            tx.send(CaptureMessage::Event(CaptureEvent::Stopped {
                channel: self.0,
            }))?;
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let sources: [Box<dyn AudioSource + Send>; 2] = [
        Box::new(Events(Channel::Mic)),
        Box::new(Events(Channel::System)),
    ];
    let session = RecordingSession::start_with_asr(
        Library::open(dir.path()).unwrap(),
        MeetingMeta::new("Fallback", Utc::now()),
        sources,
        Fake {
            stats: Arc::new(Mutex::new(Stats::default())),
            crashes: vec![],
            gate: None,
        },
        config(),
    )
    .unwrap();
    let events = drain(&session);
    assert!(events.iter().any(|e| matches!(
        e,
        PipelineEvent::CaptureEvent(notetaker_capture::CaptureEvent::Fallback {
            reason: notetaker_capture::FallbackReason::ProcessSilent,
            ..
        })
    )));
    session.wait().unwrap();
}

/// Live-like fake: it keeps the sender open until stop, optionally emitting
/// quiet frames. This also tests WASAPI's no-packet representation of silence.
struct QuietSource {
    channel: Channel,
    send_frames: bool,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl QuietSource {
    fn new(channel: Channel, send_frames: bool) -> Self {
        Self {
            channel,
            send_frames,
            stop: Arc::default(),
            worker: None,
        }
    }
}
impl AudioSource for QuietSource {
    fn start(&mut self, sender: mpsc::SyncSender<notetaker_capture::CaptureMessage>) -> Result<()> {
        use std::sync::atomic::Ordering;
        let stop = Arc::clone(&self.stop);
        let channel = self.channel;
        let send_frames = self.send_frames;
        self.worker = Some(std::thread::spawn(move || {
            let origin = std::time::Instant::now();
            while !stop.load(Ordering::Acquire) {
                if send_frames {
                    let _ = sender.try_send(notetaker_capture::CaptureMessage::Frame(
                        notetaker_capture::AudioFrame {
                            channel,
                            captured_at: origin.elapsed(),
                            samples: vec![0.0; 320],
                        },
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }));
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
        Ok(())
    }
}

#[test]
fn silent_live_sources_auto_stop_once_and_finalize_audio_and_asr() {
    for send_frames in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.auto_stop_after_silence = Some(Duration::from_millis(100));
        let session = RecordingSession::start_with_asr(
            Library::open(dir.path().join("library")).unwrap(),
            MeetingMeta::new("Silence", Utc::now()),
            [
                Box::new(QuietSource::new(Channel::Mic, send_frames)),
                Box::new(QuietSource::new(Channel::System, send_frames)),
            ],
            Fake {
                stats: Arc::default(),
                crashes: vec![],
                gate: None,
            },
            config,
        )
        .unwrap();
        let events = drain(&session);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, PipelineEvent::AutoStopped))
                .count(),
            1
        );
        assert!(matches!(
            events.last(),
            Some(PipelineEvent::Finished { .. })
        ));
        assert!(!events.iter().any(|e| matches!(e, PipelineEvent::Error(_))));
        let auto = events
            .iter()
            .position(|e| matches!(e, PipelineEvent::AutoStopped))
            .unwrap();
        let final_pass = events
            .iter()
            .position(|e| matches!(e, PipelineEvent::FinalPassProgress { .. }))
            .unwrap();
        assert!(auto < final_pass);
        let meeting = session.wait().unwrap();
        assert!(meeting.read_meta().unwrap().ended_at.is_some());
        assert!(meeting.mic_audio_path().is_file() && meeting.system_audio_path().is_file());
        assert!(!meeting.path().join("audio/.work").exists());
    }
}

#[test]
fn silence_auto_stop_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config();
    config.auto_stop_after_silence = None;
    let session = RecordingSession::start_with_asr(
        Library::open(dir.path().join("library")).unwrap(),
        MeetingMeta::new("Silence disabled", Utc::now()),
        [
            Box::new(QuietSource::new(Channel::Mic, true)),
            Box::new(QuietSource::new(Channel::System, true)),
        ],
        Fake {
            stats: Arc::default(),
            crashes: vec![],
            gate: None,
        },
        config,
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_millis(150);
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match session.events().recv_timeout(remaining) {
            Ok(event) => assert!(!matches!(
                event,
                PipelineEvent::AutoStopped | PipelineEvent::Finished { .. }
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(error) => panic!("session ended unexpectedly: {error}"),
        }
    }
    session.stop();
    let events = drain(&session);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PipelineEvent::AutoStopped))
    );
    assert!(matches!(
        events.last(),
        Some(PipelineEvent::Finished { .. })
    ));
    session.wait().unwrap();
}

#[test]
fn failed_channel_is_persisted_as_incomplete_capture() {
    struct Failed;
    impl AudioSource for Failed {
        fn start(&mut self, _: mpsc::SyncSender<notetaker_capture::CaptureMessage>) -> Result<()> {
            bail!("microphone disconnected");
        }
        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    wav(&dir.path().join("system.wav"), 3200);
    let session = RecordingSession::start_with_asr(
        Library::open(dir.path().join("library")).unwrap(),
        MeetingMeta::new("Failure", Utc::now()),
        [
            Box::new(Failed),
            Box::new(FileSource::new(
                dir.path().join("system.wav"),
                Channel::System,
                PlaybackPace::Accelerated,
            )),
        ],
        Fake {
            stats: Arc::new(Mutex::new(Stats::default())),
            crashes: vec![],
            gate: None,
        },
        config(),
    )
    .unwrap();
    let meeting = session.meeting().clone();
    let mut issues = Vec::new();
    while let Ok(event) = session.events().recv_timeout(Duration::from_secs(5)) {
        match event {
            PipelineEvent::CaptureIssue(issue) => issues.push(issue),
            PipelineEvent::Finished { .. } => break,
            _ => {}
        }
    }
    session.wait().unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].channel, notetaker_core::Channel::Mic);
    let meta = std::fs::read_to_string(meeting.path().join("meta.yaml")).unwrap();
    assert!(meta.contains("capture_issues:"), "{meta}");
    assert!(meta.contains("microphone disconnected"), "{meta}");
    assert!(meta.contains("from_ms: 0"), "{meta}");
}

#[test]
fn recording_start_sweeps_expired_audio_for_every_frontend() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::open(dir.path().join("library")).unwrap();
    let mut meta = MeetingMeta::new("Expired", Utc::now());
    meta.ended_at = Some(Utc::now());
    meta.audio_expires_at = Some(Utc::now() - chrono::TimeDelta::days(1));
    let expired = library.create_meeting(meta).unwrap();
    std::fs::write(expired.mic_audio_path(), b"expired").unwrap();
    let (session, _) = start(dir.path(), config(), vec![], None);
    session.wait().unwrap();
    assert!(!expired.mic_audio_path().exists());
}

#[test]
fn import_uses_file_metadata_final_system_channel_and_retention() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("Обсуждение релиза.wav");
    wav(&input, 32000);
    let mtime: chrono::DateTime<Utc> = std::fs::metadata(&input)
        .unwrap()
        .modified()
        .unwrap()
        .into();
    let library = Library::open(directory.path().join("library")).unwrap();
    let stats = Arc::new(Mutex::new(Stats::default()));
    let session = RecordingSession::import_with_asr(
        library,
        &input,
        notetaker_core::Mode::Local,
        Fake {
            stats: Arc::clone(&stats),
            crashes: vec![],
            gate: None,
        },
        RecordingConfig {
            retention: Some(Duration::ZERO),
            ..RecordingConfig::default()
        },
    )
    .unwrap();
    let meeting = session.meeting().clone();
    let mut progress = Vec::new();
    for event in session.events() {
        match event {
            PipelineEvent::FinalPassProgress { completed, total } => {
                progress.push((completed, total))
            }
            PipelineEvent::Finished { .. } => break,
            PipelineEvent::Error(error) => panic!("{error}"),
            _ => {}
        }
    }
    session.wait().unwrap();
    let meta = meeting.read_meta().unwrap();
    assert_eq!(meta.title, "Обсуждение релиза");
    assert_eq!(meta.started_at, mtime);
    assert_eq!((meta.ended_at.unwrap() - mtime).num_milliseconds(), 2000);
    assert!(!meeting.system_audio_path().exists());
    assert!(!meeting.mic_audio_path().exists());
    assert!(
        std::fs::read_dir(meeting.path().join("audio"))
            .unwrap()
            .next()
            .is_none()
    );
    let transcript = meeting.read_transcript().unwrap();
    assert!(!transcript.segments.is_empty());
    assert!(transcript.segments.iter().all(|s| s.source == Source::Final
        && s.channel == notetaker_core::Channel::System
        && s.speaker.0 == "them"));
    assert_eq!(progress.first(), Some(&(0, 200)));
    assert_eq!(progress.last(), Some(&(200, 200)));
    assert_eq!(stats.lock().unwrap().starts, 1);
}

#[test]
fn failed_import_does_not_publish_a_final_transcript_and_keeps_audio() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.wav");
    wav(&input, 1600);
    let library = Library::open(directory.path().join("library")).unwrap();
    let session = RecordingSession::import_with_asr(
        library,
        &input,
        notetaker_core::Mode::Local,
        Fake {
            stats: Arc::new(Mutex::new(Stats::default())),
            crashes: vec![1],
            gate: None,
        },
        RecordingConfig {
            retention: Some(Duration::ZERO),
            ..RecordingConfig::default()
        },
    )
    .unwrap();
    let meeting = session.meeting().clone();
    assert!(session.wait().is_err());
    assert!(meeting.read_transcript().unwrap().segments.is_empty());
    assert!(meeting.system_audio_path().is_file());
}
