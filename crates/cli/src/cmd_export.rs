use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use notetaker_core::{ExportFormat, Library};

#[derive(Parser)]
pub struct Args {
    /// Meeting directory.
    pub meeting: PathBuf,
    #[arg(long)]
    pub format: ExportFormat,
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
    let text = meeting.read_transcript()?.export(args.format)?;
    std::io::stdout().lock().write_all(text.as_bytes())?;
    Ok(())
}
