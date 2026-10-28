//! Separate microphone/system streams, normalized to 16 kHz mono f32.
//!
//! `captured_at` is the first sample's offset from a shared monotonic session
//! origin, not the time the channel consumer happens to receive the frame.

mod file;
mod level;
#[cfg(target_os = "macos")]
mod macos;
mod normalize;
mod policy;
mod source;
mod track;
#[cfg(windows)]
mod windows;

pub use file::{FileSource, PlaybackPace};
pub use level::{LevelMeter, Levels, SILENCE_DBFS};
#[cfg(target_os = "macos")]
pub use macos::{LiveSource, inventory, live_sources, microphone_processes};
pub use normalize::Normalizer;
pub use policy::{FallbackPolicy, FallbackReason};
pub use source::{AudioSource, SourceTask};
pub use track::TrackWriter;
#[cfg(windows)]
pub use windows::{LiveSource, inventory, live_sources};

use serde::Serialize;
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME_SAMPLES: usize = 320;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Mic,
    System,
}

#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub channel: Channel,
    pub samples: Vec<f32>,
    pub captured_at: Duration,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureEvent {
    Fallback {
        at: Duration,
        reason: FallbackReason,
    },
    Error {
        channel: Channel,
        message: String,
    },
    Discontinuity {
        channel: Channel,
        at: Duration,
    },
    TimestampError {
        channel: Channel,
        at: Duration,
    },
    Stopped {
        channel: Channel,
    },
}

#[derive(Debug)]
pub enum CaptureMessage {
    Frame(AudioFrame),
    Event(CaptureEvent),
}

#[derive(Clone, Debug)]
pub struct LiveOptions {
    /// Shared origin for both live sources and the recorder deadline.
    pub origin: std::time::Instant,
    pub microphone_id: Option<String>,
    pub process_id: Option<u32>,
    pub silence_timeout: Duration,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            origin: std::time::Instant::now(),
            microphone_id: None,
            process_id: None,
            silence_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub channel: Channel,
    pub is_default: bool,
}

#[derive(Debug, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub exe: String,
}

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub device_id: String,
    pub pid: u32,
    pub exe: Option<String>,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Inventory {
    pub devices: Vec<DeviceInfo>,
    pub sessions: Vec<SessionInfo>,
    pub processes: Vec<ProcessInfo>,
    pub warnings: Vec<String>,
}

#[cfg(not(any(windows, target_os = "macos")))]
pub struct LiveSource;

#[cfg(not(any(windows, target_os = "macos")))]
impl AudioSource for LiveSource {
    fn start(&mut self, _: std::sync::mpsc::SyncSender<CaptureMessage>) -> anyhow::Result<()> {
        anyhow::bail!("live capture not supported on this OS yet")
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn live_sources(_: LiveOptions) -> anyhow::Result<[LiveSource; 2]> {
    anyhow::bail!("live capture not supported on this OS yet")
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn inventory() -> anyhow::Result<Inventory> {
    anyhow::bail!("live capture not supported on this OS yet")
}

pub fn sample_offset(samples: u64) -> Duration {
    Duration::from_secs_f64(samples as f64 / SAMPLE_RATE as f64)
}

pub fn sample_position(at: Duration) -> u64 {
    ((at.as_nanos() * u128::from(SAMPLE_RATE) + 500_000_000) / 1_000_000_000) as u64
}
