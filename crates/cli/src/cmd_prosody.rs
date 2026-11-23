use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use notetaker_core::Library;

#[derive(Parser)]
pub struct Args {
    /// Каталог встречи с сохранённым аудио.
    pub meeting: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let path = args
        .meeting
        .canonicalize()
        .context("Не удалось открыть каталог встречи")?;
    let library = Library::open(
        path.parent()
            .context("Каталог встречи должен быть внутри библиотеки")?,
    )?;
    let meeting = library
        .list_meetings()?
        .into_iter()
        .find(|m| m.path() == path)
        .context("В каталоге встречи нет корректного meta.yaml")?;
    let lines = notetaker_pipeline::recompute_prosody(&meeting, |wav, duration| {
        let binary = super::cmd_transcribe::worker_binary()?;
        let mut client = super::cmd_transcribe::spawn_worker(&binary)
            .with_context(|| format!("Запуск {}", binary.display()))?;
        client.set_timeout(Duration::from_secs(300).saturating_add(duration.saturating_mul(4)));
        let diarization = client
            .diarize(wav, "system", None, false)
            .context("Поиск пересечений голосов на системной дорожке")?;
        client.shutdown()?;
        Ok(diarization.turns)
    })?;
    if !lines.is_empty() {
        println!("{lines}");
    }
    Ok(())
}
