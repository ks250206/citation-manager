use crate::StorageError;
use std::path::{Path, PathBuf};

/// The directory is dedicated to this application; existing directories are never chmod'ed.
#[derive(Debug, Clone)]
pub struct LibraryDirectory(PathBuf);
impl LibraryDirectory {
    /// Computes a path only; does not read/create the user's actual library.
    pub fn default_macos_path() -> Result<PathBuf, StorageError> {
        let home = std::env::var_os("HOME").ok_or(StorageError::UnsupportedPlatform)?;
        Ok(PathBuf::from(home).join("Library/Application Support/Citation Manager"))
    }
    pub fn prepare(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        prepare(path)?;
        Ok(Self(path.to_owned()))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    pub fn database_path(&self) -> PathBuf {
        self.0.join("library.sqlite")
    }
    pub(crate) fn check(&self) -> Result<(), StorageError> {
        prepare(&self.0)?;
        for name in [
            "library.sqlite",
            "library.sqlite-wal",
            "library.sqlite-shm",
            "library.sqlite-journal",
        ] {
            check_file_if_present(&self.0.join(name))?;
        }
        Ok(())
    }
    pub(crate) fn create_database(&self) -> Result<(), StorageError> {
        self.check()?;
        create_database(&self.database_path())
    }
}

#[cfg(unix)]
fn prepare(path: &Path) -> Result<(), StorageError> {
    use std::{
        fs,
        os::unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt},
        },
    };
    let unsafe_path = || StorageError::UnsafePath(path.to_owned());
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.contains(&0)
        || bytes.split(|c| *c == b'/').any(|p| p == b"." || p == b"..")
        || path.parent().is_none()
    {
        return Err(unsafe_path());
    }
    let uid = rustix::process::geteuid().as_raw();
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::DirBuilder::new().mode(0o700).create(&current)?;
                fs::symlink_metadata(&current)?
            }
            Err(e) => return Err(e.into()),
        };
        let mode = metadata.mode() & 0o7777;
        let own_root = current == path;
        let safe_parent = (metadata.uid() == 0 || metadata.uid() == uid)
            && (mode & 0o022 == 0 || (metadata.uid() == 0 && mode & 0o1000 != 0));
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || (own_root && (metadata.uid() != uid || mode != 0o700))
            || (!own_root && !safe_parent)
        {
            return Err(unsafe_path());
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn check_file_if_present(path: &Path) -> Result<(), StorageError> {
    use std::{fs, os::unix::fs::MetadataExt};
    match fs::symlink_metadata(path) {
        Ok(m)
            if m.is_file()
                && !m.file_type().is_symlink()
                && m.uid() == rustix::process::geteuid().as_raw()
                && m.mode() & 0o7777 == 0o600
                && m.nlink() == 1 =>
        {
            Ok(())
        }
        Ok(_) => Err(StorageError::UnsafePath(path.to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
fn create_database(path: &Path) -> Result<(), StorageError> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => file.sync_all().map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => check_file_if_present(path),
        Err(e) => Err(e.into()),
    }
}

#[cfg(not(unix))]
fn prepare(_: &Path) -> Result<(), StorageError> {
    Err(StorageError::UnsupportedPlatform)
}
#[cfg(not(unix))]
pub(crate) fn check_file_if_present(_: &Path) -> Result<(), StorageError> {
    Err(StorageError::UnsupportedPlatform)
}
#[cfg(not(unix))]
fn create_database(_: &Path) -> Result<(), StorageError> {
    Err(StorageError::UnsupportedPlatform)
}
