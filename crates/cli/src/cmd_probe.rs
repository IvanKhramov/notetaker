use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::Args as ClapArgs;
use notetaker_capture::{
    AudioFrame, AudioSource, CaptureEvent, CaptureMessage, Channel, Inventory, LevelMeter, Levels,
    LiveOptions, TrackWriter, inventory, live_sources, sample_position,
};
use serde::Serialize;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=86400))]
    seconds: u64,
    /// Capture this PID and its descendants (Windows build 20348+).
    #[arg(long, conflicts_with = "app", value_parser = clap::value_parser!(u32).range(1..))]
    pid: Option<u32>,
    /// Exact executable name, case insensitive; use --pid for ambiguous matches.
    #[arg(long, conflicts_with = "pid")]
    app: Option<String>,
    /// Output directory; existing audio/report files are never overwritten.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Capture device ID from the device list (default: Windows default input).
    #[arg(long)]
    mic_id: Option<String>,
    /// Process silence timeout, confirmed against audible endpoint samples.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=3600))]
    silence_seconds: u64,
}

#[derive(Serialize)]
struct TrackReport {
    file: &'static str,
    levels: Levels,
    captured_samples: u64,
    padded_samples: u64,
}

#[derive(Serialize)]
struct Report {
    version: u32,
    started_at_unix_ms: u128,
    requested_seconds: u64,
    measured_seconds: f64,
    sample_rate: u32,
    format: &'static str,
    aec: &'static str,
    requested_app: Option<String>,
    process_id: Option<u32>,
    include_process_tree: bool,
    microphone_id: Option<String>,
    silence_timeout_seconds: u64,
    fallback_used: bool,
    inventory: Inventory,
    mic: TrackReport,
    system: TrackReport,
    events: Vec<CaptureEvent>,
    errors: Vec<String>,
}

struct Track {
    writer: TrackWriter,
    meter: LevelMeter,
    captured: u64,
    padded: u64,
}

impl Track {
    fn create(path: PathBuf) -> Result<Self> {
        Ok(Self {
            writer: TrackWriter::create(path)?,
            meter: LevelMeter::default(),
            captured: 0,
            padded: 0,
        })
    }

    fn frame(&mut self, mut frame: AudioFrame, end: Duration) -> Result<()> {
        let position = sample_position(frame.captured_at);
        let end = sample_position(end);
        if position >= end {
            return Ok(());
        }
        frame
            .samples
            .truncate((end - position).min(frame.samples.len() as u64) as usize);
        for sample in &mut frame.samples {
            *sample = sample.clamp(-1.0, 1.0);
        }
        let before = self.writer.samples_written();
        self.padded += self.writer.write_frame(&frame)?;
        // Match reported levels to samples actually retained in the track. Silence
        // insertion affects RMS; overlaps after discontinuities are discarded.
        self.silence(position.saturating_sub(before));
        let overlap = before
            .saturating_sub(position)
            .min(frame.samples.len() as u64) as usize;
        self.meter.push(&frame.samples[overlap..]);
        self.captured += (frame.samples.len() - overlap) as u64;
        Ok(())
    }

    fn silence(&mut self, mut count: u64) {
        let zeros = [0.0; 320];
        while count > 0 {
            let chunk = count.min(320) as usize;
            self.meter.push(&zeros[..chunk]);
            count -= chunk as u64;
        }
    }

    fn finish(mut self, file: &'static str, duration: Duration) -> Result<TrackReport> {
        let tail = sample_position(duration).saturating_sub(self.writer.samples_written());
        self.silence(tail);
        self.padded += tail;
        self.writer.finish_at(duration)?;
        Ok(TrackReport {
            file,
            levels: self.meter.levels(),
            captured_samples: self.captured,
            padded_samples: self.padded,
        })
    }
}

pub fn run(args: Args) -> Result<()> {
    let devices = inventory()?;
    for device in &devices.devices {
        println!(
            "{:?}: {}{}\n  id: {}",
            device.channel,
            device.name,
            if device.is_default { " [default]" } else { "" },
            device.id
        );
    }
    println!("Audio sessions:");
    for session in &devices.sessions {
        println!(
            "  PID {} {} {} {:?}",
            session.pid,
            session.exe.as_deref().unwrap_or("<unknown>"),
            if session.active {
                "[active]"
            } else {
                "[inactive]"
            },
            session.name
        );
    }
    for warning in &devices.warnings {
        eprintln!("Warning: {warning}");
    }
    let pid = match &args.app {
        Some(app) => Some(select_app(&devices, app)?),
        None => args.pid,
    };
    let out = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "probe-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ))
    });
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;
    // Reserve the report before starting capture, so a failed run cannot replace
    // a previous report. A partial audio file still has valid flushed Ogg pages.
    let mut report_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("report.json"))?;
    let mut mic = Track::create(out.join("mic.opus"))?;
    let mut system = Track::create(out.join("system.opus"))?;
    let origin = Instant::now();
    let started_at_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let options = LiveOptions {
        origin,
        process_id: pid,
        microphone_id: args.mic_id.clone(),
        silence_timeout: Duration::from_secs(args.silence_seconds),
    };
    let mut sources = live_sources(options)?;
    let (sender, receiver) = mpsc::sync_channel(128);
    for source in &mut sources {
        source.start(sender.clone())?;
    }
    drop(sender);
    let duration = Duration::from_secs(args.seconds);
    let mut events = Vec::new();
    let mut errors = Vec::new();
    println!(
        "Recording {} seconds to {} (system: {})",
        args.seconds,
        out.display(),
        pid.map_or_else(
            || "endpoint".to_owned(),
            |pid| format!("PID {pid} + descendants")
        )
    );
    while origin.elapsed() < duration {
        match receiver.recv_timeout(
            duration
                .saturating_sub(origin.elapsed())
                .min(Duration::from_millis(100)),
        ) {
            Ok(message) => consume(
                message,
                &mut mic,
                &mut system,
                duration,
                &mut events,
                &mut errors,
            )?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // stop cancels backpressure before joining; drain already queued frames below.
    for source in &mut sources {
        if let Err(error) = source.stop() {
            errors.push(format!("{error:#}"));
        }
    }
    let measured_seconds = origin.elapsed().as_secs_f64();
    for message in receiver.try_iter() {
        consume(
            message,
            &mut mic,
            &mut system,
            duration,
            &mut events,
            &mut errors,
        )?;
    }
    let mic = mic.finish("mic.opus", duration)?;
    let system = system.finish("system.opus", duration)?;
    if mic.captured_samples == 0 {
        errors.push("microphone delivered no samples".into());
    }
    // Endpoint loopback may legitimately deliver no packets during digital silence.
    errors.sort();
    errors.dedup();
    let fallback_used = events
        .iter()
        .any(|event| matches!(event, CaptureEvent::Fallback { .. }));
    let report = Report {
        version: 1,
        started_at_unix_ms,
        requested_seconds: args.seconds,
        measured_seconds,
        sample_rate: 16_000,
        format: "ogg_opus",
        aec: "disabled",
        requested_app: args.app,
        process_id: pid,
        include_process_tree: pid.is_some(),
        microphone_id: args.mic_id,
        silence_timeout_seconds: args.silence_seconds,
        fallback_used,
        inventory: devices,
        mic,
        system,
        events,
        errors,
    };
    serde_json::to_writer_pretty(&mut report_file, &report)?;
    report_file.write_all(b"\n")?;
    report_file.sync_all()?;
    for (label, track) in [("mic", &report.mic), ("system", &report.system)] {
        println!(
            "{label}: average {:.1} dBFS, peak {:.1} dBFS; captured {}, padded {} samples",
            track.levels.average_dbfs,
            track.levels.peak_dbfs,
            track.captured_samples,
            track.padded_samples
        );
    }
    println!(
        "Endpoint fallback: {fallback_used}; report: {}",
        out.join("report.json").display()
    );
    for error in &report.errors {
        eprintln!("Capture error: {error}");
    }
    ensure!(
        report.errors.is_empty(),
        "probe completed with capture errors; see report.json"
    );
    Ok(())
}

fn consume(
    message: CaptureMessage,
    mic: &mut Track,
    system: &mut Track,
    duration: Duration,
    events: &mut Vec<CaptureEvent>,
    errors: &mut Vec<String>,
) -> Result<()> {
    match message {
        CaptureMessage::Frame(frame) => match frame.channel {
            Channel::Mic => mic.frame(frame, duration)?,
            Channel::System => system.frame(frame, duration)?,
        },
        CaptureMessage::Event(event) => {
            match &event {
                CaptureEvent::Error { message, .. } => errors.push(message.clone()),
                CaptureEvent::Fallback { reason, .. } => eprintln!("Endpoint fallback: {reason:?}"),
                _ => {}
            }
            events.push(event);
        }
    }
    Ok(())
}

fn select_app(devices: &Inventory, app: &str) -> Result<u32> {
    let matches: Vec<_> = devices
        .processes
        .iter()
        .filter(|p| p.exe.eq_ignore_ascii_case(app))
        .collect();
    let roots: Vec<_> = matches
        .iter()
        .filter(|p| {
            !matches
                .iter()
                .any(|parent| p.parent_pid == Some(parent.pid))
        })
        .collect();
    match roots.as_slice() {
        [process] => Ok(process.pid),
        [] => bail!(
            "no running process named {app}; use an executable name from the session list or --pid"
        ),
        _ => bail!(
            "multiple process trees named {app}: {:?}; select --pid",
            roots.iter().map(|p| p.pid).collect::<Vec<_>>()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use notetaker_capture::ProcessInfo;
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: Args,
    }

    #[test]
    fn cli_validates_duration_and_mutually_exclusive_selectors() {
        assert!(TestCli::try_parse_from(["probe"]).is_ok());
        assert!(TestCli::try_parse_from(["probe", "--seconds", "0"]).is_err());
        assert!(TestCli::try_parse_from(["probe", "--pid", "0"]).is_err());
        assert!(TestCli::try_parse_from(["probe", "--pid", "12", "--app", "teams.exe"]).is_err());
    }

    #[test]
    fn app_selects_parent_tree_and_rejects_ambiguous_roots() {
        let mut inventory = Inventory::default();
        for (pid, parent_pid) in [(10, None), (11, Some(10))] {
            inventory.processes.push(ProcessInfo {
                pid,
                parent_pid,
                exe: "ms-teams.exe".into(),
            });
        }
        assert_eq!(select_app(&inventory, "MS-TEAMS.EXE").unwrap(), 10);
        assert!(select_app(&inventory, "zoom.exe").is_err());
        inventory.processes.push(ProcessInfo {
            pid: 20,
            parent_pid: None,
            exe: "ms-teams.exe".into(),
        });
        assert!(select_app(&inventory, "ms-teams.exe").is_err());
    }
    #[test]
    fn track_report_counts_gaps_overlaps_and_tail_in_rms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.opus");
        let mut track = Track::create(path).unwrap();
        let duration = Duration::from_millis(100);
        let frame = AudioFrame {
            channel: Channel::Mic,
            captured_at: Duration::from_millis(20),
            samples: vec![0.5; 320],
        };
        track.frame(frame.clone(), duration).unwrap();
        track.frame(frame, duration).unwrap(); // duplicate must not affect levels
        let report = track.finish("mic.opus", duration).unwrap();
        assert_eq!(report.captured_samples, 320);
        assert_eq!(report.padded_samples, 1280);
        assert_eq!(report.levels.samples, 1600);
        assert!((report.levels.rms - (0.25_f64 / 5.0).sqrt()).abs() < 1e-9);
    }
}
