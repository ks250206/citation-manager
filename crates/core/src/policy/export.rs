//! #26 export destinations for scoped tokens.
//!
//! A token holding `exports:write` (`ExportAllowedDir`) may only create an export file
//! directly inside or below a directory the user allowed in the API settings. The owner's
//! `LocalFileWrite` (GUI save dialog, owner CLI) keeps choosing any path. This check is
//! lexical: the daemon's writer additionally refuses symlinks anywhere in the parent chain,
//! existing files (`create_new`) and dot components, so the lexical prefix cannot be escaped.
use std::path::{Component, Path};

/// Maximum number of allowed export directories.
pub const MAX_EXPORT_DIRS: usize = 16;

/// Absolute, no `.`/`..` components, not the filesystem root.
pub fn plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.parent().is_some()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// Whether `path` (the file to create) lies inside one of `allowed`.
pub fn destination_allowed(path: &Path, allowed: &[std::path::PathBuf]) -> bool {
    plain_absolute(path)
        && path.parent().is_some_and(|parent| {
            allowed
                .iter()
                .any(|dir| plain_absolute(dir) && parent.starts_with(dir))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn only_inside_allowed_directories() {
        let allowed = vec![PathBuf::from("/home/u/Exports")];
        assert!(destination_allowed(
            Path::new("/home/u/Exports/a.bib"),
            &allowed
        ));
        assert!(destination_allowed(
            Path::new("/home/u/Exports/sub/a.bib"),
            &allowed
        ));
        for denied in [
            "/home/u/a.bib",
            "/home/u/Exports/../a.bib",
            "/home/u/ExportsX/a.bib",
            "relative/a.bib",
            "/home/u/Exports",
            "/",
        ] {
            assert!(
                !destination_allowed(Path::new(denied), &allowed),
                "{denied}"
            );
        }
        // `Path` drops interior `.` components; the spelling is the same directory.
        assert!(destination_allowed(
            Path::new("/home/u/Exports/./a.bib"),
            &allowed
        ));
        assert!(!destination_allowed(Path::new("/x/a.bib"), &[]));
        assert!(!destination_allowed(
            Path::new("/a.bib"),
            &[PathBuf::from("/")]
        ));
    }
}
