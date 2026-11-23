use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use notetaker_core::Library;
use notetaker_pipeline::ProsodyStage;

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
    let progress = |stage| {
        eprintln!(
            "{}",
            match stage {
                ProsodyStage::Decode => "Декодирование аудиодорожек…",
                ProsodyStage::Diarize =>
                    "Диаризация системной дорожки. Это может занять несколько минут…",
                ProsodyStage::Measure => "Расчёт речевых показателей…",
            }
        )
    };
    let lines = notetaker_pipeline::recompute_prosody(&meeting, progress, |wav, duration| {
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
    write_outliers(io::stdout().lock(), &lines)?;
    eprintln!("Готово.");
    Ok(())
}

fn write_outliers(mut output: impl Write, lines: &str) -> io::Result<()> {
    for line in lines.lines() {
        writeln!(output, "{line}")?;
    }
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_each_outlier_on_its_own_line_without_padding() {
        let mut output = Vec::new();
        write_outliers(&mut output, "Громче\r\nБыстрее").unwrap();
        assert_eq!(output, "Громче\nБыстрее\n".as_bytes());
        output.clear();
        write_outliers(&mut output, "").unwrap();
        assert!(output.is_empty());
    }
}
