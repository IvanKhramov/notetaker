use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    library: Option<PathBuf>,
    #[arg(long)]
    mine: bool,
    #[arg(long)]
    folder: Option<String>,
    #[arg(long)]
    open: bool,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct Preferences {
    library: Option<PathBuf>,
    user_name: String,
    user_aliases: String,
}
pub fn run(args: Args) -> Result<()> {
    let path = directories::ProjectDirs::from("", "", "notetaker")
        .context("Нет каталога настроек")?
        .config_dir()
        .join("app.toml");
    let prefs: Preferences = match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).context("Некорректный app.toml")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        !args.mine || !prefs.user_name.trim().is_empty() || !prefs.user_aliases.trim().is_empty(),
        "Укажите «Как вас зовут» в настройках"
    );
    let root = args.library.or(prefs.library).unwrap_or_else(|| {
        let home = directories::UserDirs::new();
        home.as_ref()
            .map(|d| d.document_dir().unwrap_or(d.home_dir()).to_owned())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("notetaker")
    });
    let library = notetaker_core::Library::open(root)?;
    library.reindex_incremental()?;
    let names: Vec<_> = std::iter::once(prefs.user_name.as_str())
        .chain(prefs.user_aliases.split([',', ';', '\n']))
        .collect();
    for task in library.list_tasks_in_folder(
        args.open.then_some(false),
        None,
        false,
        args.folder.as_deref(),
    )? {
        if args.mine
            && !names
                .iter()
                .any(|name| notetaker_core::person_matches(&task.owner, name))
        {
            continue;
        }
        println!(
            "- [{}] {} - {} - {} ({}; {}:{})",
            if task.completed { "x" } else { " " },
            task.text,
            task.owner,
            task.due.map_or(task.due_text, |date| date.to_string()),
            task.meeting_title,
            notetaker_core::display_path(task.path.join("enhanced.md")),
            task.line
        );
    }
    Ok(())
}
