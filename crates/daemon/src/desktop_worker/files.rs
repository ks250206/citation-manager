//! Validates explicit selected sources; never resolves a symlink into authorization.
use cm_protocol::library::ServiceError;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
pub(super) fn invalid(s: impl Into<String>) -> ServiceError {
    ServiceError::InvalidInput(s.into())
}
pub(super) fn safe(path: &Path, file: bool) -> Result<(), ServiceError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|p| matches!(p, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid("Select an absolute path without dot components"));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        let m = fs::symlink_metadata(&current)
            .map_err(|e| invalid(format!("Cannot inspect {}: {e}", current.display())))?;
        if m.file_type().is_symlink()
            || (current != path && !m.is_dir())
            || (current == path && if file { !m.is_file() } else { !m.is_dir() })
        {
            return Err(invalid("Symlinks and nonregular sources are excluded"));
        }
        #[cfg(unix)]
        if current == path && file {
            use std::os::unix::fs::MetadataExt;
            if m.nlink() != 1 {
                return Err(invalid("Hard-linked sources are excluded"));
            }
        }
    }
    Ok(())
}
pub(super) fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, ServiceError> {
    safe(path, true)?;
    let f = File::open(path).map_err(|e| invalid(e.to_string()))?;
    let mut data = Vec::new();
    f.take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| invalid(e.to_string()))?;
    if data.len() > limit {
        return Err(invalid(format!("Selected file exceeds {limit} bytes")));
    }
    Ok(data)
}
pub(super) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ServiceError> {
    safe(
        path.parent()
            .ok_or_else(|| invalid("Select destination directory"))?,
        false,
    )?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(path)
        .map_err(|e| invalid(format!("Export needs a new filename: {e}")))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| invalid(format!("Export incomplete at {}: {e}", path.display())))?;
    File::open(path.parent().expect("validated"))
        .and_then(|f| f.sync_all())
        .map_err(|e| invalid(e.to_string()))
}
