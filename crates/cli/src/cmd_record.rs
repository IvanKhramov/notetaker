use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args as ClapArgs;
use notetaker_capture::{
    AudioSource, Channel, FileSource, LiveOptions, PlaybackPace, inventory, live_sources,
};
use notetaker_core::{Library, MeetingMeta};
use notetaker_pipeline::{AsrConfig, PipelineEvent, RecordingConfig, RecordingSession};

#[derive(ClapArgs)]
pub struct Args {
    #[arg(long)]
    library: PathBuf,
    #[arg(long)]
    title: String,
    #[arg(long, default_value = "gigaam-v3", value_parser = ["gigaam-v3", "parakeet-v3"])]
    model: String,
    #[arg(long, conflicts_with = "app", value_parser = clap::value_parser!(u32).range(1..))]
    pid: Option<u32>,
    /// Exact executable name, case insensitive (Windows).
    #[arg(long, conflicts_with = "pid")]
    app: Option<String>,
    /// Replay microphone and system WAVs at accelerated speed; stop at EOF.
    #[arg(long, num_args = 2, value_names = ["MIC.wav", "SYSTEM.wav"], conflicts_with_all = ["pid", "app"])]
    from_files: Vec<PathBuf>,
    /// Days from stop; zero deletes audio after the final pass.
    #[arg(long, default_value_t = 7)]
    retention_days: u64,
    /// Stop after both tracks stay quiet this many seconds; zero disables it.
    #[arg(long, default_value_t = 900)]
    auto_stop_silence_seconds: u64,
}

pub fn run(args: Args) -> Result<()> {
    let library = Library::open(&args.library)?;
    let mut meta = MeetingMeta::new(&args.title, Utc::now());
    meta.app = args.app.clone();
    let sources: [Box<dyn AudioSource + Send>; 2] = if args.from_files.is_empty() {
        let process_id = match &args.app {
            Some(app) => Some(crate::cmd_probe::select_app(&inventory()?, app)?),
            None => args.pid,
        };
        let [mic, system] = live_sources(LiveOptions {
            process_id,
            ..LiveOptions::default()
        })?;
        [Box::new(mic), Box::new(system)]
    } else {
        [
            Box::new(FileSource::new(
                &args.from_files[0],
                Channel::Mic,
                PlaybackPace::Accelerated,
            )),
            Box::new(FileSource::new(
                &args.from_files[1],
                Channel::System,
                PlaybackPace::Accelerated,
            )),
        ]
    };
    let retention = args
        .retention_days
        .checked_mul(86400)
        .context("retention too large")?;
    let mut asr = AsrConfig::new(args.model)?;
    asr.vocabulary = notetaker_core::Vocabulary::load(library.root())?;
    let session = RecordingSession::start(
        library,
        meta,
        sources,
        asr,
        RecordingConfig {
            retention: Some(Duration::from_secs(retention)),
            auto_stop_after_silence: (args.auto_stop_silence_seconds != 0)
                .then(|| Duration::from_secs(args.auto_stop_silence_seconds)),
            ..RecordingConfig::default()
        },
    )?;
    let stop = session.stop_handle();
    ctrlc::set_handler(move || stop.stop()).context("install Ctrl+C handler")?;
    eprintln!(
        "Recording: {} (Ctrl+C to stop)",
        session.meeting().path().display()
    );
    for event in session.events() {
        match event {
            PipelineEvent::LiveSegments(segments) => {
                for segment in segments {
                    println!(
                        "[{} - {}] {}: {}",
                        timestamp(segment.start_ms),
                        timestamp(segment.end_ms),
                        segment.speaker.0,
                        segment.text
                    );
                }
            }
            PipelineEvent::LiveDisabled => eprintln!(
                "Live transcription disabled; recording continues, full transcription follows at stop."
            ),
            PipelineEvent::AutoStopped => {
                eprintln!("Recording stopped after prolonged silence; final transcription follows.")
            }
            PipelineEvent::Error(error) => eprintln!("Error: {error}"),
            PipelineEvent::CaptureEvent(event) => eprintln!("Capture: {event:?}"),
            PipelineEvent::CaptureIssue(issue) => eprintln!("Incomplete capture: {issue:?}"),
            PipelineEvent::FinalPassProgress { completed, total } => {
                eprintln!("Final pass: {completed}/{total}")
            }
            PipelineEvent::Finished { .. } => break,
            PipelineEvent::Lag { .. } => {}
        }
    }
    let meeting = session.wait()?;
    let transcript = meeting.read_transcript()?;
    println!(
        "Finished: {} ({} segments)",
        meeting.path().display(),
        transcript.segments.len()
    );
    for segment in transcript.segments {
        println!(
            "[{} - {}] {}: {}",
            timestamp(segment.start_ms),
            timestamp(segment.end_ms),
            segment.speaker.0,
            segment.text
        );
    }
    Ok(())
}

fn timestamp(ms: u64) -> String {
    format!("{:02}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    #[test]
    fn validates_record_arguments() {
        let base = ["record", "--library", "library", "--title", "Test"];
        assert!(Cli::try_parse_from(base).is_ok());
        assert!(
            Cli::try_parse_from(base.into_iter().chain([
                "--from-files",
                "mic.wav",
                "system.wav",
                "--retention-days",
                "0"
            ]))
            .is_ok()
        );
        for extra in [
            vec!["--pid", "1", "--app", "zoom.exe"],
            vec!["--from-files", "one.wav"],
            vec!["--from-files", "a.wav", "b.wav", "--pid", "1"],
            vec!["--model", "unknown"],
            vec!["--retention-days", "-1"],
        ] {
            assert!(Cli::try_parse_from(base.into_iter().chain(extra)).is_err());
        }
    }
}
