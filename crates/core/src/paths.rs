//! Windows extended-length prefixes are storage details, not display text.
pub fn display_path(path: impl AsRef<std::path::Path>) -> String {
    display_text(&path.as_ref().to_string_lossy())
}

pub fn display_text(text: &str) -> String {
    text.replace(r"\\?\UNC\", r"\\").replace(r"\\?\", "")
}

#[cfg(test)]
mod tests {
    #[test]
    fn drive_unc_and_embedded_paths_are_readable() {
        assert_eq!(
            super::display_path(r"\\?\C:\Users\Имя\notes.md"),
            r"C:\Users\Имя\notes.md"
        );
        assert_eq!(
            super::display_path(r"\\?\UNC\server\share\notes.md"),
            r"\\server\share\notes.md"
        );
        assert_eq!(
            super::display_text(r"Ошибка: \\?\C:\file.md"),
            r"Ошибка: C:\file.md"
        );
    }
}
