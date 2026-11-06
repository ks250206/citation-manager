pub mod paths;
use crate::{
    StorageError,
    migrations::{self, MigrationBackup},
    repositories::{SqliteAttachments, SqlitePapers},
};
use paths::LibraryDirectory;
use rusqlite::{Connection, OpenFlags};

/// Owns one worker-side connection. Never opens the default user library implicitly.
pub struct LibraryDb {
    pub(crate) connection: Connection,
}
impl LibraryDb {
    pub fn open(directory: &LibraryDirectory) -> Result<Self, StorageError> {
        Self::open_inner(directory, None)
    }
    pub fn open_with_backup(
        directory: &LibraryDirectory,
        backup: &mut dyn MigrationBackup,
    ) -> Result<Self, StorageError> {
        Self::open_inner(directory, Some(backup))
    }
    fn open_inner(
        directory: &LibraryDirectory,
        backup: Option<&mut dyn MigrationBackup>,
    ) -> Result<Self, StorageError> {
        directory.create_database()?;
        let mut connection = Connection::open_with_flags(
            directory.database_path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        directory.check()?;
        configure(&connection, true)?;
        migrations::run(&mut connection, backup)?;
        directory.check()?;
        Ok(Self { connection })
    }
    /// SQLite in-memory databases use journal_mode=memory; WAL is disk-only.
    /// Foreign keys and identical schema/repositories remain enabled.
    pub fn in_memory() -> Result<Self, StorageError> {
        let mut connection = Connection::open_in_memory()?;
        configure(&connection, false)?;
        migrations::run(&mut connection, None)?;
        Ok(Self { connection })
    }
    pub fn papers(&mut self) -> SqlitePapers<'_> {
        SqlitePapers {
            connection: &mut self.connection,
        }
    }
    pub fn attachments(&self) -> SqliteAttachments<'_> {
        SqliteAttachments {
            connection: &self.connection,
        }
    }
    pub fn journal_mode(&self) -> Result<String, StorageError> {
        Ok(self
            .connection
            .pragma_query_value(None, "journal_mode", |r| r.get(0))?)
    }
    pub fn foreign_keys_enabled(&self) -> Result<bool, StorageError> {
        Ok(self
            .connection
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))?)
    }
}
fn configure(db: &Connection, disk: bool) -> Result<(), StorageError> {
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "foreign_keys", true)?;
    let mode: String = db.pragma_update_and_check(
        None,
        "journal_mode",
        if disk { "WAL" } else { "MEMORY" },
        |r| r.get(0),
    )?;
    let fk: bool = db.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    if !fk || mode != if disk { "wal" } else { "memory" } {
        return Err(StorageError::Migration(
            "connection pragmas not enabled".into(),
        ));
    }
    db.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}
