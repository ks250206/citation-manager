//! Local path handling. Inputs: the directory the user named is resolved, the final file is
//! not (papersd refuses a symlinked or non-regular source). Exports: the daemon writes into a
//! private temporary directory; the CLI then publishes atomically, never into the library,
//! never through a symlink and never over an existing file without `--force`.
use crate::output::{CliError, Kind};
use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

fn invalid(message: impl Into<String>) -> CliError {
    CliError::new(Kind::InvalidInput, message)
}

/// Absolute source path for the daemon: canonical parent directory + the literal file name.
pub fn input_file(path: &Path) -> Result<PathBuf, CliError> {
    let absolute = crate::connect::absolute(path)?;
    let name = file_name(&absolute)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| invalid("select a file, not a root directory"))?;
    let parent = fs::canonicalize(parent)
        .map_err(|e| CliError::new(Kind::NotFound, format!("{}: {e}", parent.display())))?;
    let resolved = parent.join(name);
    match fs::symlink_metadata(&resolved) {
        Ok(m) if m.file_type().is_symlink() => Err(invalid(format!(
            "{} is a symlink; select the file itself",
            resolved.display()
        ))),
        Ok(m) if !m.is_file() => Err(invalid(format!(
            "{} is not a regular file",
            resolved.display()
        ))),
        Ok(_) => Ok(resolved),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(CliError::new(
            Kind::NotFound,
            format!("{}: no such file", resolved.display()),
        )),
        Err(e) => Err(invalid(format!("{}: {e}", resolved.display()))),
    }
}

fn file_name(path: &Path) -> Result<OsString, CliError> {
    match path.components().next_back() {
        Some(std::path::Component::Normal(name)) => Ok(name.to_owned()),
        _ => Err(invalid(format!(
            "{} does not name a file (no trailing '.', '..' or '/')",
            path.display()
        ))),
    }
}

/// A checked export destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportTarget {
    /// Canonical destination directory (outside the library).
    pub dir: PathBuf,
    pub path: PathBuf,
    pub replace: bool,
}

/// Validates `--output`: parent must exist and lie outside `library` (canonical); an existing
/// destination is refused unless `force`, and is never a symlink or non-regular file.
pub fn export_target(output: &Path, library: &Path, force: bool) -> Result<ExportTarget, CliError> {
    let absolute = crate::connect::absolute(output)?;
    let name = file_name(&absolute)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| invalid("--output must name a file"))?;
    let dir = fs::canonicalize(parent).map_err(|e| {
        invalid(format!(
            "output directory {} must exist: {e}",
            parent.display()
        ))
    })?;
    let library = fs::canonicalize(library).unwrap_or_else(|_| library.to_owned());
    if dir.starts_with(&library) {
        return Err(CliError::new(
            Kind::PermissionDenied,
            format!(
                "refusing to write inside the library directory {}",
                library.display()
            ),
        ));
    }
    let path = dir.join(name);
    let replace = match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(CliError::new(
                Kind::Conflict,
                format!(
                    "{} is a symlink; refusing to follow or replace it",
                    path.display()
                ),
            ));
        }
        Ok(m) if !m.is_file() => {
            return Err(CliError::new(
                Kind::Conflict,
                format!("{} exists and is not a regular file", path.display()),
            ));
        }
        Ok(_) if !force => {
            return Err(CliError::new(
                Kind::Conflict,
                format!("{} exists; pass --force to replace it", path.display()),
            ));
        }
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(invalid(format!("{}: {e}", path.display()))),
    };
    Ok(ExportTarget { dir, path, replace })
}

/// Private (0700) scratch directory, removed with its contents when dropped.
pub struct Scratch {
    pub dir: PathBuf,
}
impl Scratch {
    pub fn new(parent: &Path) -> Result<Self, CliError> {
        let parent =
            fs::canonicalize(parent).map_err(|e| invalid(format!("{}: {e}", parent.display())))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = parent.join(format!(".papers-export-{}-{nanos}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| {
                invalid(format!(
                    "cannot create scratch directory in {}: {e}",
                    parent.display()
                ))
            })?;
        Ok(Self { dir })
    }
    /// Reads a regular, single-link file the daemon wrote here, bounded by `max` bytes.
    pub fn read(&self, name: &str, max: u64) -> Result<Vec<u8>, CliError> {
        let path = self.dir.join(name);
        let m = fs::symlink_metadata(&path)
            .map_err(|e| CliError::internal(format!("export output missing: {e}")))?;
        if !m.is_file() || m.nlink() != 1 || m.len() > max {
            return Err(CliError::internal("unexpected export output file"));
        }
        fs::read(&path).map_err(|e| CliError::internal(format!("cannot read export: {e}")))
    }
    /// Atomically publishes `name` at `target`: no-clobber hard link, or rename with `--force`.
    pub fn publish(&self, name: &str, target: &ExportTarget) -> Result<(), CliError> {
        let source = self.dir.join(name);
        let failed = |e: io::Error| {
            CliError::new(
                if e.kind() == io::ErrorKind::AlreadyExists {
                    Kind::Conflict
                } else {
                    Kind::Internal
                },
                format!("cannot write {}: {e}", target.path.display()),
            )
        };
        if target.replace {
            // Re-check right before replacing: still a regular file, not swapped for a link.
            let m = fs::symlink_metadata(&target.path).map_err(failed)?;
            if !m.is_file() {
                return Err(CliError::new(
                    Kind::Conflict,
                    format!("{} changed; refusing to replace", target.path.display()),
                ));
            }
            fs::rename(&source, &target.path).map_err(failed)?;
        } else {
            fs::hard_link(&source, &target.path).map_err(failed)?;
        }
        File::open(&target.dir)
            .and_then(|d| d.sync_all())
            .map_err(failed)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        let _ = fs::remove_dir(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_targets_refuse_library_symlinks_and_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let library = root.join("library");
        fs::create_dir_all(library.join("sub")).unwrap();
        let out = root.join("out");
        fs::create_dir(&out).unwrap();

        assert_eq!(
            export_target(&library.join("x.bib"), &library, false)
                .unwrap_err()
                .kind,
            Kind::PermissionDenied
        );
        assert_eq!(
            export_target(&library.join("sub/x.bib"), &library, true)
                .unwrap_err()
                .kind,
            Kind::PermissionDenied
        );
        // A symlinked directory pointing into the library is caught after canonicalization.
        std::os::unix::fs::symlink(&library, root.join("alias")).unwrap();
        assert_eq!(
            export_target(&root.join("alias/x.bib"), &library, false)
                .unwrap_err()
                .kind,
            Kind::PermissionDenied
        );

        let fresh = export_target(&out.join("refs.bib"), &library, false).unwrap();
        assert!(!fresh.replace);
        fs::write(out.join("refs.bib"), "old").unwrap();
        assert_eq!(
            export_target(&out.join("refs.bib"), &library, false)
                .unwrap_err()
                .kind,
            Kind::Conflict
        );
        assert!(
            export_target(&out.join("refs.bib"), &library, true)
                .unwrap()
                .replace
        );
        std::os::unix::fs::symlink(out.join("refs.bib"), out.join("link.bib")).unwrap();
        assert_eq!(
            export_target(&out.join("link.bib"), &library, true)
                .unwrap_err()
                .kind,
            Kind::Conflict
        );
        assert_eq!(
            export_target(&out.join(".."), &library, true)
                .unwrap_err()
                .kind,
            Kind::InvalidInput
        );
        assert!(export_target(&root.join("missing/x.bib"), &library, false).is_err());
    }

    #[test]
    fn scratch_publishes_atomically_without_clobbering() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let library = root.join("library");
        let scratch = Scratch::new(&root).unwrap();
        let mode = fs::metadata(&scratch.dir).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
        fs::write(scratch.dir.join("export.bib"), "@misc{a}").unwrap();
        assert_eq!(scratch.read("export.bib", 1024).unwrap(), b"@misc{a}");
        let target = export_target(&root.join("refs.bib"), &library, false).unwrap();
        // Someone creates the file between the check and the publish: still no clobber.
        fs::write(root.join("refs.bib"), "theirs").unwrap();
        assert_eq!(
            scratch.publish("export.bib", &target).unwrap_err().kind,
            Kind::Conflict
        );
        assert_eq!(fs::read_to_string(root.join("refs.bib")).unwrap(), "theirs");
        let forced = export_target(&root.join("refs.bib"), &library, true).unwrap();
        scratch.publish("export.bib", &forced).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("refs.bib")).unwrap(),
            "@misc{a}"
        );
        let dir = scratch.dir.clone();
        drop(scratch);
        assert!(!dir.exists());
    }

    #[test]
    fn input_files_keep_their_final_component() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("a.pdf"), "%PDF").unwrap();
        std::os::unix::fs::symlink(root.join("a.pdf"), root.join("b.pdf")).unwrap();
        fs::create_dir(root.join("d")).unwrap();
        assert_eq!(
            input_file(&root.join("d/../a.pdf")).unwrap(),
            root.join("a.pdf")
        );
        assert_eq!(
            input_file(&root.join("b.pdf")).unwrap_err().kind,
            Kind::InvalidInput
        );
        assert_eq!(
            input_file(&root.join("d")).unwrap_err().kind,
            Kind::InvalidInput
        );
        assert_eq!(
            input_file(&root.join("none.pdf")).unwrap_err().kind,
            Kind::NotFound
        );
    }
}
