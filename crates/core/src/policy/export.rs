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
/// Per-export caps applied by the daemon to every export (owner and `exports:write`
/// tokens alike): a larger selection is refused before any file is created.
pub const MAX_EXPORT_RECORDS: usize = 50_000;
pub const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;

/// Folders that may not be allowed as export folders (all paths canonical): the home
/// directory itself, the library, anything inside it (its private state included), and any
/// ancestor of the library (exports there could be created inside the library tree).
pub fn export_dir_refused(dir: &Path, home: Option<&Path>, library: &Path) -> bool {
    home.is_some_and(|home| dir == home) || library.starts_with(dir) || dir.starts_with(library)
}

/// File extensions an `exports:write` token may create for an export format name.
pub fn extension_allowed(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

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

    #[test]
    fn home_library_and_its_ancestors_are_refused_as_export_folders() {
        let home = Path::new("/home/u");
        let library = Path::new("/home/u/Library/papers");
        for refused in [
            "/home/u",
            "/home/u/Library",
            "/home/u/Library/papers",
            "/home/u/Library/papers/daemon",
            "/",
        ] {
            assert!(
                export_dir_refused(Path::new(refused), Some(home), library),
                "{refused}"
            );
        }
        assert!(!export_dir_refused(
            Path::new("/home/u/Exports"),
            Some(home),
            library
        ));
        assert!(extension_allowed(Path::new("/x/a.BIB"), &["bib"]));
        assert!(!extension_allowed(Path::new("/x/a.sh"), &["bib"]));
        assert!(!extension_allowed(Path::new("/x/bib"), &["bib"]));
    }
}
