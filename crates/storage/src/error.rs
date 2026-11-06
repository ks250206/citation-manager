use std::{fmt, path::PathBuf};

#[derive(Debug)]
pub enum StorageError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    UnsafePath(PathBuf),
    UnsupportedPlatform,
    Migration(String),
    BackupRequired,
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "library I/O: {e}"),
            Self::Sqlite(e) => write!(f, "library database: {e}"),
            Self::UnsafePath(_) => f.write_str("unsafe library path, owner, type or permissions"),
            Self::UnsupportedPlatform => {
                f.write_str("private library paths require Unix permissions")
            }
            Self::Migration(e) => write!(f, "migration refused: {e}"),
            Self::BackupRequired => {
                f.write_str("existing library upgrade requires a pre-migration backup hook")
            }
        }
    }
}
impl std::error::Error for StorageError {}
impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<rusqlite::Error> for StorageError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

pub(crate) fn service_error(e: rusqlite::Error) -> cm_core::ServiceError {
    use cm_core::ServiceError;
    match e {
        rusqlite::Error::SqliteFailure(ref failure, _) => match failure.extended_code {
            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY => ServiceError::Conflict,
            _ if failure.code == rusqlite::ErrorCode::ConstraintViolation => {
                ServiceError::InvalidInput("relational constraint rejected the record".into())
            }
            _ => ServiceError::Unavailable,
        },
        _ => ServiceError::Unavailable,
    }
}
