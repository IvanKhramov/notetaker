use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use notetaker_core::Library;
use notetaker_llm::{Config, OsCredentialStore, ktalk};
use std::{io::Write, path::PathBuf};

#[derive(Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Проверить подключение к Толку.
    Status,
    /// Сопоставить спикеров с участниками записи Толка.
    Sync {
        meeting: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        recording: Option<String>,
    },
}
fn print_plan(plan: &ktalk::Plan, out: &mut impl Write) -> Result<()> {
    writeln!(
        out,
        "Запись: {} ({})",
        plan.recording.title, plan.recording.created_date
    )?;
    writeln!(
        out,
        "Смещение (время Толка = локальное + смещение): {:.1} с",
        plan.offset_ms as f64 / 1000.0
    )?;
    writeln!(out, "Проверка текста: {:.3}", plan.validation_score)?;
    writeln!(out, "Спикер notetaker -> Участник | Реплики | Секунды")?;
    for row in &plan.rows {
        writeln!(
            out,
            "{} -> {} | {} | {:.1}",
            row.speaker, row.participant, row.segments, row.seconds
        )?;
    }
    Ok(())
}
fn sync(
    library: &Library,
    meeting: &notetaker_core::Meeting,
    plan: &ktalk::Plan,
    dry_run: bool,
    out: &mut impl Write,
) -> Result<()> {
    print_plan(plan, out)?;
    if !dry_run {
        let result = ktalk::apply(library, meeting, plan)?;
        if let Some(error) = result.index_error {
            writeln!(out, "Имена сохранены; поиск не обновлён: {error}")?;
        }
    }
    Ok(())
}
fn load_settings(path: &std::path::Path) -> Result<ktalk::Settings> {
    ktalk::Settings::from_path(path)?.context("Добавьте [ktalk] в config.toml; см. docs/ktalk.md")
}
pub fn run(args: Args) -> Result<()> {
    let settings = load_settings(&Config::path()?)?;
    let client = settings.client(&OsCredentialStore)?;
    let owner = client.profile()?;
    match args.command {
        Command::Status => println!("Толк: {}", owner.name()),
        Command::Sync {
            meeting,
            dry_run,
            recording,
        } => {
            let path = meeting
                .canonicalize()
                .context("Не удалось открыть встречу")?;
            let library = Library::open(path.parent().context("Нет каталога библиотеки")?)?;
            let meeting = library
                .list_meetings()?
                .into_iter()
                .find(|m| m.path() == path)
                .context("Не найдена встреча с meta.yaml")?;
            let local = meeting.load_transcript()?.value;
            let recording = ktalk::select_recording(&client, &meeting, recording.as_deref())?;
            let remote = client.transcript(&recording.id)?;
            let plan = ktalk::plan(
                &local,
                meeting.read_meta()?.started_at,
                recording,
                &remote,
                None,
            )?;
            sync(&library, &meeting, &plan, dry_run, &mut std::io::stdout())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn ktalk_loader_does_not_require_ai_sections() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(
            &path,
            "[ktalk]\nbase_url='https://neutral.ktalk.ru'\ncredential_id='neutral'\n",
        )
        .unwrap();
        assert_eq!(
            super::load_settings(&path).unwrap().base_url,
            "https://neutral.ktalk.ru"
        );
    }
    use super::*;
    #[test]
    fn dry_run_prints_table_and_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let start = "2026-01-01T12:00:00Z".parse().unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new("Тест", start))
            .unwrap();
        let texts = [
            "Проектирование очереди сообщений требует проверки порядка",
            "Кэширование ответов сервера сокращает задержку обработки",
            "Планируем обновление схемы хранения журнала событий",
            "Измеряем пропускную способность сетевого подключения клиента",
            "Регулярное резервирование защищает данные рабочих проектов",
            "Разделение нагрузки позволяет ускорить расчёт результатов",
        ];
        let times = [1000, 6000, 15000, 28000, 46000, 71000];
        let segments: Vec<_> = texts.iter().enumerate().map(|(i,text)| serde_json::json!({
            "id":format!("00000000-0000-0000-0000-{:012}",i+1),"speaker":"s1","channel":"system","start_ms":times[i],"end_ms":times[i]+3000,"text":text,"source":"final","edited":false
        })).collect();
        let local: notetaker_core::Transcript =
            serde_json::from_value(serde_json::json!({"version":1,"segments":segments})).unwrap();
        meeting.write_transcript(&local).unwrap();
        let before = std::fs::read(meeting.path().join("transcript.json")).unwrap();
        let revision = meeting.load_transcript().unwrap().revision;
        let words: Vec<_> = texts.iter().enumerate().map(|(i,text)| serde_json::json!({"text":text,"startTimeOffsetInMillis":times[i],"endTimeOffsetInMillis":times[i]+3000})).collect();
        let remote = serde_json::from_value(serde_json::json!({"status":"complete","tracks":[{"speaker":{"isAnonymous":true,"anonymousId":"guest","anonymousName":"Анна Петрова"},"chunks":[{"startTimeOffsetInMillis":0,"endTimeOffsetInMillis":80000,"words":words}]}]})).unwrap();
        let recording = ktalk::Recording {
            id: "test".into(),
            title: "Тест".into(),
            created_date: start,
            duration: 80,
            status: "complete".into(),
        };
        let plan = ktalk::plan(&local, start, recording, &remote, None).unwrap();
        let mut output = Vec::new();
        sync(&library, &meeting, &plan, true, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Спикер notetaker -> Участник | Реплики | Секунды"));
        assert!(output.contains("s1 -> Анна Петрова | 6 | 18.0"));
        assert!(output.contains("Проверка текста: 1.000"));
        assert_eq!(
            std::fs::read(meeting.path().join("transcript.json")).unwrap(),
            before
        );
        assert_eq!(meeting.load_transcript().unwrap().revision, revision);
        assert!(
            Args::try_parse_from(["ktalk", "sync", "meeting", "--dry-run", "--recording", "id"])
                .is_ok()
        );
    }
}
