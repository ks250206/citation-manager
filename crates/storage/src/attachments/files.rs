//! Shared private-file primitives. Same-UID hostile races/ACLs are outside the #8 boundary.
use crate::{LibraryDirectory, StorageError, db::paths::check_file_if_present};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(crate) const BUFFER_SIZE: usize = 64 * 1024;

pub(crate) fn invalid(reason: &str) -> StorageError {
    StorageError::Integrity(reason.into())
}

pub(crate) fn absolute(path: &Path) -> Result<(), StorageError> {
    let text = path
        .to_str()
        .ok_or_else(|| StorageError::UnsafePath(path.into()))?;
    if !path.is_absolute()
        || text.contains(['\\', '\0'])
        || text
            .split('/')
            .skip(1)
            .any(|p| matches!(p, "" | "." | ".."))
    {
        return Err(StorageError::UnsafePath(path.into()));
    }
    Ok(())
}

/// Validate every directory independently, including its exact 0700 mode.
pub(crate) fn tree(
    base: &LibraryDirectory,
    relative: &str,
    create: bool,
) -> Result<PathBuf, StorageError> {
    base.check()?;
    let key = cm_domain::StorageKey::try_from(relative.to_owned())
        .map_err(|_| invalid("invalid relative path"))?;
    let mut path = base.path().to_owned();
    for part in key.as_str().split('/') {
        path.push(part);
        if !create && !path.try_exists()? {
            // try_exists follows links; explicitly reject dangling symlinks too.
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(path),
                _ => return Err(StorageError::UnsafePath(path)),
            }
        }
        let existed = fs::symlink_metadata(&path).is_ok();
        LibraryDirectory::prepare(&path)?;
        if !existed {
            sync_dir(path.parent().unwrap())?;
        }
    }
    Ok(path)
}

pub(crate) fn managed_path(
    base: &LibraryDirectory,
    key: &str,
    create: bool,
) -> Result<PathBuf, StorageError> {
    cm_domain::StorageKey::try_from(key.to_owned()).map_err(|_| invalid("invalid managed key"))?;
    let (parents, _) = key
        .rsplit_once('/')
        .ok_or_else(|| invalid("key requires directory"))?;
    let parent = tree(base, parents, create)?;
    if parent != base.path().join(parents) && !create {
        return Ok(base.path().join(key));
    }
    let path = base.path().join(key);
    check_file_if_present(&path)?;
    Ok(path)
}

pub(crate) fn source(path: &Path) -> Result<File, StorageError> {
    absolute(path)?;
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        let m = fs::symlink_metadata(&current)?;
        if m.file_type().is_symlink()
            || (current != path && !m.is_dir())
            || (current == path && !m.is_file())
        {
            return Err(StorageError::UnsafePath(current));
        }
        #[cfg(unix)]
        if current == path {
            use std::os::unix::fs::MetadataExt;
            if m.nlink() != 1 {
                return Err(StorageError::UnsafePath(current));
            }
        }
    }
    let file = File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata()?;
        if !m.is_file() || m.nlink() != 1 {
            return Err(StorageError::UnsafePath(path.into()));
        }
    }
    #[cfg(not(unix))]
    {
        return Err(StorageError::UnsupportedPlatform);
    }
    Ok(file)
}

pub(crate) fn create_file(path: &Path) -> Result<File, StorageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        Ok(OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(StorageError::UnsupportedPlatform)
    }
}
pub(crate) fn sync_dir(path: &Path) -> Result<(), StorageError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub(crate) fn hash(mut reader: impl Read) -> Result<([u8; 32], u64), StorageError> {
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| invalid("file too large"))?;
    }
    Ok((hasher.finalize().into(), size))
}
pub(crate) fn copy(
    mut input: impl Read,
    output: &mut File,
) -> Result<([u8; 32], u64), StorageError> {
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n])?;
        hasher.update(&buffer[..n]);
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| invalid("file too large"))?;
    }
    output.sync_all()?;
    Ok((hasher.finalize().into(), size))
}
pub(crate) fn hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn key(hash: &[u8; 32]) -> String {
    let hex = hex(hash);
    format!("attachments/{}/{}/{}.pdf", &hex[..2], &hex[2..4], hex)
}

pub(crate) fn fresh_destination(path: &Path) -> Result<LibraryDirectory, StorageError> {
    absolute(path)?;
    if fs::symlink_metadata(path).is_ok() {
        return Err(StorageError::UnsafePath(path.into()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("destination requires parent"))?;
    // Parent must already exist; never create unrelated ancestor paths during restore.
    if !parent.is_dir() {
        return Err(StorageError::UnsafePath(parent.into()));
    }
    LibraryDirectory::prepare(parent)
}

/// Atomic no-replace publication: an existing shared blob/destination is never overwritten.
pub(crate) fn publish(from: &Path, to: &Path) -> Result<(), StorageError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)
            .map_err(|e| StorageError::Io(e.into()))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (from, to);
        Err(StorageError::UnsupportedPlatform)
    }
}
