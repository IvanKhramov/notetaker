use std::path::{Path, PathBuf};

/// Resolve WinGet's Links aliases before looking for adjacent binaries.
pub fn sibling(executable: &Path, name: &str) -> std::io::Result<PathBuf> {
    Ok(executable.canonicalize()?.with_file_name(name))
}

pub fn executable() -> std::io::Result<PathBuf> {
    std::env::current_exe()?.canonicalize()
}

pub fn current_sibling(name: &str) -> std::io::Result<PathBuf> {
    sibling(&std::env::current_exe()?, name)
}

#[cfg(test)]
mod tests {
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
            package.join("notetaker-asr")
        );
    }
}
