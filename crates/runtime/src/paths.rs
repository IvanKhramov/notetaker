use std::path::{Path, PathBuf};

/// Resolve WinGet's Links aliases before looking for adjacent binaries.
pub fn sibling(executable: &Path, name: &str) -> std::io::Result<PathBuf> {
    Ok(simplified(executable.canonicalize()?).with_file_name(name))
}

pub fn executable() -> std::io::Result<PathBuf> {
    Ok(simplified(std::env::current_exe()?.canonicalize()?))
}

/// Windows canonical paths are verbatim (`\\?\C:\...`). Installers, the Run key
/// and most tools reject them, so hand out the ordinary form instead.
pub fn simplified(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        PathBuf::from(rest)
    } else {
        path
    }
}

pub fn current_sibling(name: &str) -> std::io::Result<PathBuf> {
    sibling(&std::env::current_exe()?, name)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[test]
    fn strips_verbatim_prefixes() {
        let simplified = |p: &str| super::simplified(PathBuf::from(p));
        assert_eq!(
            simplified(r"\\?\C:\Users\main\AppData\Local\Programs\notetaker"),
            PathBuf::from(r"C:\Users\main\AppData\Local\Programs\notetaker")
        );
        assert_eq!(
            simplified(r"\\?\UNC\server\share\notetaker"),
            PathBuf::from(r"\\server\share\notetaker")
        );
        assert_eq!(
            simplified(r"\\?\Volume{abc}\notetaker"),
            PathBuf::from(r"\\?\Volume{abc}\notetaker")
        );
        assert_eq!(
            simplified("/opt/notetaker"),
            PathBuf::from("/opt/notetaker")
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolves_alias_to_package_directory() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("package");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("notetaker"), "exe").unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(package.join("notetaker"), &alias).unwrap();
        assert_eq!(
            super::sibling(&alias, "notetaker-asr").unwrap(),
            // macOS temp dirs live behind the /var -> /private/var symlink.
            package.canonicalize().unwrap().join("notetaker-asr")
        );
    }
}
