use anyhow::Result;
use clap::Parser;
use notetaker_core::{Library, SearchFilter, SearchMode};

#[derive(Parser)]
pub struct SearchArgs {
    pub query: String,
    #[arg(long)]
    pub folder: Option<String>,
    #[arg(long, default_value = "hybrid")]
    pub mode: SearchMode,
    #[arg(long)]
    pub library: Option<std::path::PathBuf>,
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
}
#[derive(Parser)]
pub struct ReindexArgs {
    #[arg(long)]
    pub semantic: bool,
    #[arg(long)]
    pub library: Option<std::path::PathBuf>,
}
fn library(path: Option<std::path::PathBuf>) -> Result<Library> {
    Ok(Library::open(match path {
        Some(p) => p,
        None => {
            #[derive(serde::Deserialize, Default)]
            struct Preferences {
                library: Option<std::path::PathBuf>,
            }
            let path = directories::ProjectDirs::from("", "", "notetaker")
                .map(|d| d.config_dir().join("app.toml"));
            let prefs = path
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|s| toml::from_str::<Preferences>(&s))
                .transpose()?
                .unwrap_or_default();
            prefs.library.unwrap_or_else(|| {
                directories::UserDirs::new()
                    .map(|d| d.document_dir().unwrap_or(d.home_dir()).join("notetaker"))
                    .unwrap_or_else(|| "notetaker".into())
            })
        }
    })?)
}
pub fn search(args: SearchArgs) -> Result<()> {
    let library = library(args.library)?;
    let config = notetaker_llm::Config::load().ok();
    let result = notetaker_llm::search::search(
        &library,
        &args.query,
        args.limit.min(100),
        args.mode,
        &SearchFilter {
            folder: args.folder,
            ..Default::default()
        },
        config.as_ref(),
        true,
    )?;
    if let Some(note) = result.note {
        eprintln!("{note}");
    }
    for hit in result.hits {
        println!(
            "{}  {}  {}\n{}\n{}",
            hit.meeting_id,
            hit.title,
            hit.anchor
                .as_ref()
                .map(|a| format!("{a:?}"))
                .unwrap_or_default(),
            hit.path.display(),
            hit.snippet
        );
    }
    Ok(())
}
pub fn reindex(args: ReindexArgs) -> Result<()> {
    let library = library(args.library)?;
    if args.semantic {
        let config = notetaker_llm::Config::load().ok();
        notetaker_llm::search::reindex(&library, true, config.as_ref(), &mut |p| {
            eprintln!(
                "Поиск по смыслу: {}/{}; новых фрагментов {}; Local только по словам: {}",
                p.completed, p.total, p.chunks, p.excluded_local
            )
        })?;
    } else {
        library.reindex_full()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_accepts_optional_folder_including_unassigned() {
        let args = SearchArgs::try_parse_from(["search", "бюджет"]).unwrap();
        assert!(args.folder.is_none());
        for folder in ["client/project", ""] {
            let args =
                SearchArgs::try_parse_from(["search", "бюджет", "--folder", folder]).unwrap();
            assert_eq!(args.folder.as_deref(), Some(folder));
        }
    }
}
