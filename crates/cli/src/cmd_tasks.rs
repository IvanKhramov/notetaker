use anyhow::{Result, ensure};

use std::path::PathBuf;

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
type Preferences = notetaker_core::app_config::AppSettings;
pub fn run(args: Args) -> Result<()> {
    let prefs = Preferences::load()?;
    ensure!(
        !args.mine || !prefs.user_name.trim().is_empty() || !prefs.user_aliases.trim().is_empty(),
        "Укажите «Как вас зовут» в настройках"
    );
    let root = args.library.unwrap_or(prefs.library);
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
