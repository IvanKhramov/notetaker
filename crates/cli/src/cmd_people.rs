use anyhow::Result;
use notetaker_core::{DigestFilter, DigestPeriod, Library};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct PeopleArgs {
    #[arg(long)]
    library: Option<PathBuf>,
    #[arg(long)]
    folder: Option<String>,
}
#[derive(clap::Args)]
pub struct PersonArgs {
    pub person: String,
    #[arg(long)]
    library: Option<PathBuf>,
    #[arg(long)]
    folder: Option<String>,
}
#[derive(clap::Args)]
pub struct DigestArgs {
    #[arg(long, required = true)]
    week: bool,
    #[arg(long)]
    prev: bool,
    #[arg(long)]
    library: Option<PathBuf>,
    #[arg(long)]
    folder: Option<String>,
}
type Preferences = notetaker_core::app_config::AppSettings;
fn library(path: Option<PathBuf>) -> Result<(Library, Preferences)> {
    let prefs = Preferences::load()?;
    let root = path.unwrap_or_else(|| prefs.library.clone());
    Ok((Library::open(root)?, prefs))
}
pub fn people(args: PeopleArgs) -> Result<()> {
    let (library, prefs) = library(args.library)?;
    for person in library
        .people_in_folder(
            false,
            &prefs.user_name,
            &prefs.user_aliases,
            args.folder.as_deref(),
        )?
        .into_iter()
        .filter(|p| !p.hidden)
    {
        println!(
            "{} <{}>",
            person.name,
            person.emails.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}
pub fn person(args: PersonArgs) -> Result<()> {
    let (library, prefs) = library(args.library)?;
    let matches: Vec<_> = library
        .people_in_folder(
            false,
            &prefs.user_name,
            &prefs.user_aliases,
            args.folder.as_deref(),
        )?
        .into_iter()
        .filter(|p| p.id == args.person || p.matches(&args.person))
        .collect();
    anyhow::ensure!(
        matches.len() == 1,
        "Человек не найден или имя неоднозначно; укажите email"
    );
    let page = library.person_page_in_folder(
        matches.into_iter().next().unwrap(),
        false,
        args.folder.as_deref(),
    )?;
    println!("{}", serde_json::to_string_pretty(&page)?);
    Ok(())
}
pub fn digest(args: DigestArgs) -> Result<()> {
    let (library, _) = library(args.library)?;
    let digest = library.digest(
        DigestPeriod::week(chrono::Local::now(), args.prev),
        &DigestFilter {
            folder: args.folder,
            ..Default::default()
        },
        &chrono::Local,
        false,
    )?;
    let digest = notetaker_llm::summarize_digest(
        digest,
        notetaker_llm::Config::load().ok().as_ref(),
        notetaker_llm::Cancellation::default(),
    );
    let path = library.save_digest(&digest)?;
    println!(
        "{}\n\n{}",
        digest.markdown,
        notetaker_core::display_path(path)
    );
    Ok(())
}
