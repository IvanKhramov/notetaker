use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use notetaker_core::Library;
use notetaker_llm::{Cancellation, Config, Template, create_provider, enhance_meeting};

#[derive(Parser)]
pub struct Args {
    pub meeting_dir: PathBuf,
    #[arg(long)]
    pub template: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let path = args
        .meeting_dir
        .canonicalize()
        .context("cannot open meeting directory")?;
    let root = path
        .parent()
        .context("meeting directory must be inside a library")?;
    let library = Library::open(root)?;
    let meeting = library
        .list_meetings()?
        .into_iter()
        .find(|meeting| meeting.path() == path)
        .context("meeting directory has no valid meta.yaml")?;
    let meta = meeting.read_meta()?;
    let template_id = args
        .template
        .as_deref()
        .or(meta.template.as_deref())
        .unwrap_or("general");
    let template = Template::load(library.root(), template_id)?;
    let config = Config::load()?;
    let provider = create_provider(
        config.select(meta.mode, args.provider.as_deref())?.clone(),
        meta.mode,
        Cancellation::default(),
    )?;
    let result = enhance_meeting(
        &meeting,
        template_id,
        &template,
        provider.as_ref(),
        &mut |_| {},
    )?;
    println!("{}", result.text);
    for warning in result.warnings {
        eprintln!("Warning: {warning}");
    }
    Ok(())
}
