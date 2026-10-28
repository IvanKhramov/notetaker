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
    #[arg(long, value_parser = ["auto", "gigaam-v3", "parakeet-v3"])]
    pub model: Option<String>,
    #[arg(long, value_parser = ["local", "hybrid"])]
    pub mode: Option<String>,
    #[arg(long)]
    pub no_summary: bool,
    #[arg(long)]
    pub folder: Option<String>,
}

#[derive(Deserialize)]
#[serde(default)]
struct Preferences {
    library: PathBuf,
    model: String,
    asr_acceleration: notetaker_asr_proto::Provider,
    asr_threads: u32,
    mode: Mode,
    retention_days: Option<u32>,
    auto_summary: bool,
    diarize: bool,
    speakers: Option<u32>,
    voice_library: bool,
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
            asr_acceleration: notetaker_asr_proto::Provider::default(),
            asr_threads: 0,
            mode: Mode::Local,
            retention_days: Some(7),
            auto_summary: true,
            diarize: true,
            speakers: None,
            voice_library: false,
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
    let folder_defaults = args
        .folder
        .as_ref()
        .and_then(|name| library.load_folders().ok()?.value.get(name).cloned());
    let mut asr = AsrConfig::new(args.model.unwrap_or(settings.model))?;
    asr.provider = settings.asr_acceleration;
    asr.num_threads = settings.asr_threads;
    asr.vocabulary = Vocabulary::load(library.root())?;
    if let Some(defaults) = &folder_defaults {
        asr.vocabulary.merge(&defaults.vocabulary);
    }
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
            folder: args.folder,
            mode_override: args.mode.as_ref().map(|_| mode),
            diarization: notetaker_pipeline::DiarizationConfig {
                enabled: settings.diarize,
                speakers: folder_defaults
                    .and_then(|d| d.speaker_hint)
                    .or(settings.speakers),
                voice_library: settings.voice_library,
            },
            retention: settings
                .retention_days
                .map(|days| Duration::from_secs(u64::from(days) * 86400)),
            ..RecordingConfig::default()
        },
    )?;
    let stop = session.stop_handle();
    let cancel = Cancellation::default();
    let signal = cancel.clone();
    ctrlc::set_handler(move || {
        signal.cancel();
        stop.stop();
    })
    .context("install Ctrl+C handler")?;
    eprintln!("Импорт: {}", session.meeting().path().display());
    for event in session.events() {
        match event {
            PipelineEvent::FinalPassProgress { completed, total } => {
                eprintln!("Импорт: {}%", completed * 100 / total.max(1))
            }
            PipelineEvent::Warning(message) => eprintln!("Предупреждение: {message}"),
            PipelineEvent::Error(error) => eprintln!("Ошибка: {error}"),
            PipelineEvent::Finished { .. } => break,
            _ => {}
        }
    }
    let meeting = session.wait()?;
    library.reindex_incremental()?;
    println!("{}", meeting.path().display());
    if settings.auto_summary && !args.no_summary && !cancel.is_cancelled() {
        let summary = (|| -> Result<()> {
            let meta = meeting.read_meta()?;
            let mode = meta.mode;
            let template_id = meta.template.as_deref().unwrap_or("general");
            let template = Template::load(library.root(), template_id)?;
            let config = Config::load()?;
            let provider = create_provider(
                config.select_scenario(mode, notetaker_llm::Scenario::Summary, None)?,
                mode,
                cancel,
            )?;
            let result = enhance_meeting_if_unedited(
                &meeting,
                template_id,
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
