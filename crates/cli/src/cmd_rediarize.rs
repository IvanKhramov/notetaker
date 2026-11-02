use std::{fs::OpenOptions, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use notetaker_asr_proto::AsrClient;
use notetaker_core::Library;

#[derive(Parser)]
pub struct Args {
    /// Meeting directory. Speaker names and merges on the system track are reset.
    pub meeting: PathBuf,
    /// Requested number of system-track voices (1..32); microphone is separate.
    /// Sherpa may return fewer active voices after segmentation.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=32))]
    pub speakers: Option<u32>,
}

pub fn run(args: Args) -> Result<()> {
    let path = args
        .meeting
        .canonicalize()
        .context("cannot open meeting directory")?;
    let library = Library::open(path.parent().context("meeting must be inside a library")?)?;
    let meeting = library
        .list_meetings()?
        .into_iter()
        .find(|m| m.path() == path)
        .context("meeting directory has no valid meta.yaml")?;
    let mut source = std::fs::File::open(path.join("transcript.json"))?;
    let backup = path.join(format!(
        "transcript.before-diarization-{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ")
    ));
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)?;
    std::io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    eprintln!("Резервная копия: {}", backup.display());
    let binary = super::cmd_transcribe::worker_binary()?;
    let mut client =
        AsrClient::spawn(&binary).with_context(|| format!("launch {}", binary.display()))?;
    notetaker_pipeline::rediarize(&meeting, &mut client, args.speakers)?;
    client.shutdown()?;
    library.reindex_incremental()?;
    println!("{}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_remote_speaker_count() {
        assert!(Args::try_parse_from(["rediarize", "meeting"]).is_ok());
        assert!(Args::try_parse_from(["rediarize", "meeting", "--speakers", "2"]).is_ok());
        for count in ["0", "33", "-1"] {
            assert!(Args::try_parse_from(["rediarize", "meeting", "--speakers", count]).is_err());
        }
    }
}
