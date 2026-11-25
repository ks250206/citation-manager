use std::{fmt, path::PathBuf};

#[derive(Debug)]
pub enum StorageError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    UnsafePath(PathBuf),
    UnsupportedPlatform,
    Migration(String),
    BackupRequired,
    Integrity(String),
    DigestMismatch,
    CorruptBlob,
    MissingSource,
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
            Self::Integrity(reason) => write!(f, "integrity check failed: {reason}"),
            Self::DigestMismatch => f.write_str("copied attachment digest mismatch"),
            Self::CorruptBlob => f.write_str("existing shared blob is corrupt"),
            Self::MissingSource => f.write_str("attachment source does not exist"),
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

/// Why a SQLite call failed, for callers that must tell index damage from lock contention
/// or a deadline interrupt (all of which `service_error` reports as `Unavailable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlFault {
    /// SQLITE_BUSY/SQLITE_LOCKED: another connection holds a conflicting lock. Transient.
    Busy,
    /// SQLITE_INTERRUPT: the statement was stopped (request deadline).
    Interrupted,
    /// SQLITE_CORRUPT* / SQLITE_NOTADB, or an FTS5 "malformed" report: real damage.
    Corrupt,
    Other,
}
pub fn sql_fault(e: &rusqlite::Error) -> SqlFault {
    use rusqlite::ErrorCode;
    match e {
        rusqlite::Error::SqliteFailure(failure, message) => match failure.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => SqlFault::Busy,
            ErrorCode::OperationInterrupted => SqlFault::Interrupted,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => SqlFault::Corrupt,
            _ if message
                .as_deref()
                .is_some_and(|m| m.contains("malformed") || m.contains("fts5: corrupt")) =>
            {
                SqlFault::Corrupt
            }
            _ => SqlFault::Other,
        },
        _ => SqlFault::Other,
    }
}
