use anyhow::{Context, Result};
use clap::Parser;
use notetaker_asr_proto::{AsrClient, AudioInput, Provider};
use std::path::PathBuf;

#[derive(Parser)]
pub struct Args {
    pub file: PathBuf,
    #[arg(long, default_value = "gigaam-v3", value_parser = ["auto", "gigaam-v3", "parakeet-v3"])]
    pub model: String,
}

pub fn run(args: Args) -> Result<()> {
    let file = args.file.canonicalize().context("cannot open input WAV")?;
    let binary = worker_binary()?;
    let mut client = spawn_worker(&binary).with_context(|| {
        format!(
            "launch {}; build/install notetaker-asr alongside notetaker",
            binary.display()
        )
    })?;
    client.load_model(&args.model, Provider::Cpu)?;
    for segment in client.transcribe("cli", AudioInput::WavPath(file), "file", 0)? {
        println!(
            "[{} - {}] {}",
            timestamp(segment.start_ms),
            timestamp(segment.end_ms),
            segment.text
        );
    }
    client.shutdown()?;
    Ok(())
}

/// Starts the ASR worker with the models folder chosen in the app settings.
pub(crate) fn spawn_worker(binary: &std::path::Path) -> Result<AsrClient> {
    let mut command = std::process::Command::new(binary);
    command.env(
        "NOTETAKER_MODELS_DIR",
        notetaker_core::app_config::AppSettings::load()?.models_path(),
    );
    Ok(AsrClient::spawn_command(
        command,
        std::time::Duration::from_secs(300),
    )?)
}

pub(crate) fn worker_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NOTETAKER_ASR_BIN") {
        return Ok(path.into());
    }
    let name = format!("notetaker-asr{}", std::env::consts::EXE_SUFFIX);
    let sibling = notetaker_runtime::paths::current_sibling(&name)?;
    Ok(if sibling.is_file() {
        sibling
    } else {
        name.into()
    })
}

fn timestamp(ms: u64) -> String {
    format!("{:02}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps() {
        assert_eq!(timestamp(62_003), "01:02.003");
        assert_eq!(timestamp(3_600_001), "60:00.001");
    }
}
