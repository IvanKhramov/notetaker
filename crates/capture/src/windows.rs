//! WASAPI 0.26.0: explicit 48 kHz float stereo shared streams, normalized in Rust.
//! Process loopback never queries the unsupported mix format/period/buffer size.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::SyncSender,
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use sysinfo::{ProcessRefreshKind, RefreshKind, System};
use wasapi::{
    AudioClient, Device, DeviceEnumerator, Direction, SampleType, SessionState, StreamMode,
    WaveFormat,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, IAudioCaptureClient,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::source::{recover_default, send};
use crate::{
    AudioFrame, AudioSource, CaptureEvent, CaptureMessage, Channel, DeviceInfo, FallbackPolicy,
    FallbackReason, Inventory, LevelMeter, LiveOptions, Normalizer, ProcessInfo, SILENCE_DBFS,
    SessionInfo, SourceTask, sample_offset,
};

const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const BLOCK_ALIGN: usize = CHANNELS * 4;
const SILENT_THRESHOLD: f64 = -60.0;

struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> {
        wasapi::initialize_mta().ok()?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

pub fn inventory() -> Result<Inventory> {
    let _apartment = Apartment::enter()?;
    let enumerator = DeviceEnumerator::new()?;
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    let mut inventory = Inventory {
        processes: system
            .processes()
            .values()
            .map(|p| ProcessInfo {
                pid: p.pid().as_u32(),
                parent_pid: p.parent().map(|id| id.as_u32()),
                exe: p.name().to_string_lossy().into_owned(),
            })
            .collect(),
        ..Inventory::default()
    };
    inventory.processes.sort_by_key(|p| p.pid);
    for (direction, channel) in [
        (Direction::Capture, Channel::Mic),
        (Direction::Render, Channel::System),
    ] {
        let default_id = enumerator
            .get_default_device(&direction)
            .and_then(|d| d.get_id())
            .ok();
        for device in &enumerator.get_device_collection(&direction)? {
            let device = device?;
            let id = device.get_id()?;
            inventory.devices.push(DeviceInfo {
                name: device.get_friendlyname()?,
                channel,
                is_default: default_id.as_ref() == Some(&id),
                id: id.clone(),
            });
            let sessions = (|| -> Result<()> {
                let manager = device.get_iaudiosessionmanager()?;
                let sessions = manager.get_audiosessionenumerator()?;
                for i in 0..sessions.get_count()? {
                    let session = sessions.get_session(i)?;
                    let pid = session.get_process_id()?;
                    inventory.sessions.push(SessionInfo {
                        device_id: id.clone(),
                        pid,
                        exe: inventory
                            .processes
                            .iter()
                            .find(|p| p.pid == pid)
                            .map(|p| p.exe.clone()),
                        name: session.get_display_name().unwrap_or_default(),
                        active: session.get_state()? == SessionState::Active,
                    });
                }
                Ok(())
            })();
            if let Err(error) = sessions {
                inventory
                    .warnings
                    .push(format!("sessions on {id}: {error:#}"));
            }
        }
    }
    Ok(inventory)
}

pub struct LiveSource {
    channel: Channel,
    options: LiveOptions,
    task: SourceTask,
}

pub fn live_sources(options: LiveOptions) -> Result<[LiveSource; 2]> {
    ensure!(options.process_id != Some(0), "PID 0 cannot be captured");
    Ok([Channel::Mic, Channel::System].map(|channel| LiveSource {
        channel,
        options: options.clone(),
        task: SourceTask::default(),
    }))
}

impl AudioSource for LiveSource {
    fn start(&mut self, sender: SyncSender<CaptureMessage>) -> Result<()> {
        let channel = self.channel;
        let options = self.options.clone();
        self.task.start(channel, sender, move |stop, sender| {
            let _apartment = Apartment::enter()?;
            let clock = Clock::new(options.origin)?;
            let enumerator = DeviceEnumerator::new()?;
            let defaults = LiveOptions {
                microphone_id: None,
                process_id: None,
                ..options.clone()
            };
            recover_default(
                |use_default, healthy| {
                    let options = if use_default { &defaults } else { &options };
                    let result = match channel {
                        Channel::Mic => {
                            capture_mic(&enumerator, options, &clock, stop, sender, healthy)
                        }
                        Channel::System => {
                            capture_system(&enumerator, options, &clock, stop, sender, healthy)
                        }
                    };
                    if stop.load(Ordering::Relaxed) {
                        Ok(())
                    } else {
                        result
                    }
                },
                |error| {
                    send(
                        sender,
                        stop,
                        CaptureMessage::Event(CaptureEvent::Error {
                            channel,
                            message: format!(
                                "capture interrupted; reopening default device: {error:#}"
                            ),
                        }),
                    );
                },
            )
        })
    }
    fn stop(&mut self) -> Result<()> {
        self.task.stop()
    }
}

struct Clock {
    origin: Instant,
    qpc_origin_hns: u64,
}
impl Clock {
    #[allow(unsafe_code)]
    fn new(origin: Instant) -> Result<Self> {
        let mut ticks = 0;
        let mut frequency = 0;
        // SAFETY: both Win32 calls receive valid writable i64 pointers.
        unsafe {
            QueryPerformanceFrequency(&mut frequency)?;
            QueryPerformanceCounter(&mut ticks)?;
        }
        ensure!(frequency > 0 && ticks >= 0, "invalid performance counter");
        let now_hns = (ticks as u128 * 10_000_000 / frequency as u128) as u64;
        Ok(Self {
            origin,
            qpc_origin_hns: now_hns.saturating_sub((origin.elapsed().as_nanos() / 100) as u64),
        })
    }
    fn timestamp(&self, packet: &Packet) -> Duration {
        if packet.timestamp_error {
            self.origin
                .elapsed()
                .saturating_sub(Duration::from_secs_f64(
                    packet.samples.len() as f64 / CHANNELS as f64 / RATE as f64,
                ))
        } else {
            Duration::from_nanos(
                packet
                    .timestamp
                    .saturating_sub(self.qpc_origin_hns)
                    .saturating_mul(100),
            )
        }
    }
}

struct Packet {
    samples: Vec<f32>,
    index: u64,
    timestamp: u64,
    discontinuity: bool,
    timestamp_error: bool,
}

struct Stream {
    client: AudioClient,
    capture: IAudioCaptureClient,
    normalizer: Normalizer,
    anchor: Option<Duration>,
    position: u64,
    expected_index: Option<u64>,
}

impl Stream {
    #[allow(unsafe_code)]
    fn open(mut client: AudioClient) -> Result<Self> {
        let format = WaveFormat::new(32, 32, &SampleType::Float, RATE as usize, CHANNELS, None);
        client.initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::PollingShared {
                autoconvert: true,
                buffer_duration_hns: 1_000_000,
            },
        )?;
        // SAFETY: GetService is called on an initialized capture client; the COM
        // reference owns its lifetime. Use the raw capture interface because the
        // wasapi 0.26.0 byte reader forms a slice before checking SILENT, for which
        // Windows may supply a null buffer (IAudioCaptureClient::GetBuffer docs).
        let capture = unsafe {
            client
                .as_iaudioclient()
                .GetService::<IAudioCaptureClient>()?
        };
        client.start_stream()?;
        Ok(Self {
            client,
            capture,
            normalizer: Normalizer::new(RATE, CHANNELS)?,
            anchor: None,
            position: 0,
            expected_index: None,
        })
    }

    fn endpoint(device: &Device) -> Result<Self> {
        Self::open(device.get_iaudioclient()?)
    }

    #[allow(unsafe_code)]
    fn packet(&self) -> Result<Option<Packet>> {
        // SAFETY: COM apartment and capture client live on this worker; no buffer
        // is outstanding. GetNextPacketSize does not hand out any borrowed memory.
        if unsafe { self.capture.GetNextPacketSize()? } == 0 {
            return Ok(None);
        }
        let mut data = std::ptr::null_mut();
        let (mut frames, mut flags, mut index, mut timestamp) = (0, 0, 0, 0);
        // SAFETY: pointers refer to valid local out parameters. Every successful
        // nonempty GetBuffer is paired below with ReleaseBuffer, including invalid data.
        unsafe {
            self.capture.GetBuffer(
                &mut data,
                &mut frames,
                &mut flags,
                Some(&mut index),
                Some(&mut timestamp),
            )?;
        }
        // AUDCLNT_S_BUFFER_EMPTY leaves data/index/timestamp unspecified. There
        // is no acquired packet to release and no timeline anchor to establish.
        if frames == 0 {
            return Ok(None);
        }
        let samples = (|| -> Result<Vec<f32>> {
            ensure!(
                frames <= RATE * 10,
                "unreasonable capture packet size: {frames}"
            );
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                return Ok(vec![0.0; frames as usize * CHANNELS]);
            }
            ensure!(!data.is_null(), "null non-silent capture buffer");
            // SAFETY: a non-silent GetBuffer guarantees frames * block-align
            // readable bytes until ReleaseBuffer; byte conversion needs no alignment.
            let bytes = unsafe { std::slice::from_raw_parts(data, frames as usize * BLOCK_ALIGN) };
            Ok(bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect())
        })();
        // SAFETY: releases exactly the frame count returned by this GetBuffer.
        unsafe {
            self.capture.ReleaseBuffer(frames)?;
        }
        Ok(Some(Packet {
            samples: samples?,
            index,
            timestamp,
            discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
            timestamp_error: timestamp == 0
                || flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0,
        }))
    }

    fn drain(
        &mut self,
        channel: Channel,
        clock: &Clock,
        sender: &SyncSender<CaptureMessage>,
        stop: &AtomicBool,
    ) -> Result<Vec<AudioFrame>> {
        let mut frames = Vec::new();
        // Bound a drain so a producer or slow consumer cannot starve cancellation.
        for _ in 0..100 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Some(packet) = self.packet()? else {
                break;
            };
            let at = clock.timestamp(&packet);
            if packet.discontinuity || self.expected_index.is_some_and(|i| i != packet.index) {
                self.normalizer = Normalizer::new(RATE, CHANNELS)?;
                self.anchor = None;
                self.position = 0;
                send(
                    sender,
                    stop,
                    CaptureMessage::Event(CaptureEvent::Discontinuity { channel, at }),
                );
            }
            if packet.timestamp_error {
                send(
                    sender,
                    stop,
                    CaptureMessage::Event(CaptureEvent::TimestampError { channel, at }),
                );
            }
            self.expected_index = Some(
                packet
                    .index
                    .saturating_add((packet.samples.len() / CHANNELS) as u64),
            );
            let anchor = *self.anchor.get_or_insert(at);
            let samples = self.normalizer.push(&packet.samples)?;
            if !samples.is_empty() {
                let captured_at = anchor + sample_offset(self.position);
                self.position += samples.len() as u64;
                frames.push(AudioFrame {
                    channel,
                    captured_at,
                    samples,
                });
            }
        }
        Ok(frames)
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
}

fn capture_mic(
    enumerator: &DeviceEnumerator,
    options: &LiveOptions,
    clock: &Clock,
    stop: &AtomicBool,
    sender: &SyncSender<CaptureMessage>,
    healthy: &mut bool,
) -> Result<()> {
    let device = match &options.microphone_id {
        Some(id) => enumerator.get_device(id)?,
        None => enumerator.get_default_device(&Direction::Capture)?,
    };
    ensure!(
        device.get_direction() == Direction::Capture,
        "selected microphone is not a capture device"
    );
    let id = device.get_id()?;
    let mut stream = Stream::endpoint(&device).context("open microphone")?;
    let mut checked = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let frames = stream.drain(Channel::Mic, clock, sender, stop)?;
        *healthy = true;
        for frame in frames {
            if !send(sender, stop, CaptureMessage::Frame(frame)) {
                return Ok(());
            }
        }
        if checked.elapsed() >= Duration::from_secs(1) {
            if options.microphone_id.is_none() {
                check_default(enumerator, Direction::Capture, &id)?;
            }
            checked = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn capture_system(
    enumerator: &DeviceEnumerator,
    options: &LiveOptions,
    clock: &Clock,
    stop: &AtomicBool,
    sender: &SyncSender<CaptureMessage>,
    healthy: &mut bool,
) -> Result<()> {
    // Monitor the real endpoint concurrently, never infer its activity from the
    // process stream or a historical session peak (the Teams/WebView2 failure).
    let device = enumerator.get_default_device(&Direction::Render)?;
    let id = device.get_id()?;
    let mut endpoint = Stream::endpoint(&device).context("open render endpoint loopback")?;
    let mut policy = FallbackPolicy::new(options.silence_timeout, SILENT_THRESHOLD);
    let mut process = match options.process_id {
        Some(pid) => match AudioClient::new_application_loopback_client(pid, true)
            .map_err(anyhow::Error::from)
            .and_then(Stream::open)
        {
            Ok(stream) => Some(stream),
            Err(error) => {
                if let Some(reason) = policy.failed(FallbackReason::ActivationFailed {
                    message: format!("{error:#}"),
                }) {
                    fallback(sender, stop, clock, reason);
                }
                None
            }
        },
        None => None,
    };
    let mut checked = Instant::now();
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let endpoint_frames = endpoint.drain(Channel::System, clock, sender, stop)?;
        *healthy = true;
        let mut process_frames = Vec::new();
        if let Some(stream) = &mut process {
            match stream.drain(Channel::System, clock, sender, stop) {
                Ok(frames) => process_frames = frames,
                Err(error) => {
                    if let Some(reason) = policy.failed(FallbackReason::ProcessCaptureFailed {
                        message: format!("{error:#}"),
                    }) {
                        fallback(sender, stop, clock, reason);
                    }
                    process = None;
                }
            }
        }
        if process.is_some()
            && let Some(reason) = policy.observe(
                started.elapsed(),
                level(&process_frames),
                level(&endpoint_frames),
            )
        {
            fallback(sender, stop, clock, reason);
            process = None;
        }
        let frames = if process.is_some() {
            process_frames
        } else {
            endpoint_frames
        };
        for frame in frames {
            if !send(sender, stop, CaptureMessage::Frame(frame)) {
                return Ok(());
            }
        }
        if checked.elapsed() >= Duration::from_secs(1) {
            check_default(enumerator, Direction::Render, &id)?;
            checked = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn fallback(
    sender: &SyncSender<CaptureMessage>,
    stop: &AtomicBool,
    clock: &Clock,
    reason: FallbackReason,
) {
    send(
        sender,
        stop,
        CaptureMessage::Event(CaptureEvent::Fallback {
            at: clock.origin.elapsed(),
            reason,
        }),
    );
}
fn level(frames: &[AudioFrame]) -> f64 {
    if frames.is_empty() {
        return SILENCE_DBFS;
    }
    let mut meter = LevelMeter::default();
    for frame in frames {
        meter.push(&frame.samples);
    }
    meter.levels().average_dbfs
}
fn check_default(
    enumerator: &DeviceEnumerator,
    direction: Direction,
    expected: &str,
) -> Result<()> {
    if enumerator.get_default_device(&direction)?.get_id()? != expected {
        bail!("default {direction} device changed; restart capture with the new device");
    }
    Ok(())
}
