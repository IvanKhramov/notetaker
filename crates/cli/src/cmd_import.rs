use std::{fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use notetaker_core::{Library, Mode, Vocabulary};
use notetaker_llm::{Cancellation, Config, Template, create_provider, enhance_meeting_if_unedited};
use notetaker_pipeline::{AsrConfig, PipelineEvent, RecordingConfig, RecordingSession};
use serde::Deserialize;

#[derive(ClapArgs)]
pub struct Args {
    pub file: PathBuf,
    #[arg(long)]
    pub library: Option<PathBuf>,
    #[arg(long, value_parser = ["gigaam-v3", "parakeet-v3"])]
    pub model: Option<String>,
    #[arg(long, value_parser = ["local", "hybrid"])]
    pub mode: Option<String>,
    #[arg(long)]
    pub no_summary: bool,
}

#[derive(Deserialize)]
#[serde(default)]
struct Preferences {
    library: PathBuf,
    model: String,
    mode: Mode,
    retention_days: Option<u32>,
    auto_summary: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        let home = directories::UserDirs::new();
        let documents = home
            .as_ref()
            .map(|dirs| dirs.document_dir().unwrap_or(dirs.home_dir()).to_owned())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            library: documents.join("notetaker"),
            model: "gigaam-v3".into(),
            mode: Mode::Local,
            retention_days: Some(7),
            auto_summary: true,
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    let path = directories::ProjectDirs::from("", "", "notetaker")
        .context("Нет каталога настроек")?
        .config_dir()
        .join("app.toml");
    let settings: Preferences = match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).context("Некорректный app.toml")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
        Err(error) => return Err(error.into()),
    };
    let library = Library::open(args.library.unwrap_or(settings.library))?;
    let mut asr = AsrConfig::new(args.model.unwrap_or(settings.model))?;
    asr.vocabulary = Vocabulary::load(library.root())?;
    let mode = match args.mode.as_deref() {
        Some("hybrid") => Mode::Hybrid,
        Some("local") => Mode::Local,
        _ => settings.mode,
    };
    let session = RecordingSession::import(
        library.clone(),
        &args.file,
        mode,
        asr,
        RecordingConfig {
            retention: settings
                .retention_days
                .map(|days| Duration::from_secs(u64::from(days) * 86400)),
            ..RecordingConfig::default()
        },
    )?;
    let stop = session.stop_handle();
    ctrlc::set_handler(move || stop.stop()).context("install Ctrl+C handler")?;
    eprintln!("Импорт: {}", session.meeting().path().display());
    for event in session.events() {
        match event {
            PipelineEvent::FinalPassProgress { completed, total } => {
                eprintln!("Импорт: {}%", completed * 100 / total.max(1))
            }
            PipelineEvent::Error(error) => eprintln!("Ошибка: {error}"),
            PipelineEvent::Finished { .. } => break,
            _ => {}
        }
    }
    let meeting = session.wait()?;
    library.reindex_incremental()?;
    println!("{}", meeting.path().display());
    if settings.auto_summary && !args.no_summary {
        let summary = (|| -> Result<()> {
            let template = Template::load(library.root(), "general")?;
            let config = Config::load()?;
            let provider = create_provider(
                config.select(mode, None)?.clone(),
                mode,
                Cancellation::default(),
            )?;
            let result = enhance_meeting_if_unedited(
                &meeting,
                "general",
                &template,
                provider.as_ref(),
                &mut |_| {},
            )?;
            for warning in result.warnings {
                eprintln!("{warning}");
            }
            Ok(())
        })();
        if let Err(error) = summary {
            eprintln!("Запись импортирована, конспект не создан: {error:#}");
        }
    }
    Ok(())
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
    fn accepts_file_without_required_flags_and_validates_model() {
        assert!(Cli::try_parse_from(["import", "Встреча.mp4"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "import",
                "file.opus",
                "--library",
                "notes",
                "--no-summary",
                "--mode",
                "local"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["import", "file.wav", "--model", "unknown"]).is_err());
    }
}
