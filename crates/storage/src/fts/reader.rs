//! Dedicated, interruptible read connections for search (#18). The daemon keeps a small pool
//! of these so concurrent searches do not queue behind one shared connection, and every
//! request's deadline is enforced inside SQLite (progress handler -> SQLITE_INTERRUPT), not
//! only between short-term scan chunks.
use super::{SqliteSearch, extraction::FulltextReader};
use crate::{LibraryDirectory, StorageError, db::configure, migrations};
use rusqlite::{Connection, OpenFlags};
use std::{
    ops::Deref,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

/// VM instructions between deadline checks (a few microseconds of work).
const PROGRESS_OPS: i32 = 1_000;

/// Interrupt deadline shared with the connection's progress handler.
pub struct Deadline {
    base: Instant,
    /// Nanoseconds after `base` plus one; 0 = no deadline.
    at: AtomicU64,
}
impl Deadline {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            at: AtomicU64::new(0),
        }
    }
    pub(crate) fn set(&self, deadline: Option<Instant>) {
        let value = deadline.map_or(0, |d| {
            (d.saturating_duration_since(self.base).as_nanos() as u64).saturating_add(1)
        });
        self.at.store(value, Ordering::SeqCst);
    }
    fn expired(&self) -> bool {
        match self.at.load(Ordering::SeqCst) {
            0 => false,
            at => self.base.elapsed().as_nanos() as u64 + 1 >= at,
        }
    }
}

/// One search read connection. It never runs migrations: the library must already be
/// migrated by the daemon's control connection (schema history is checked on open).
pub struct SearchReader {
    pub(crate) connection: Connection,
    deadline: Arc<Deadline>,
}
impl SearchReader {
    pub fn open(directory: &LibraryDirectory) -> Result<Self, StorageError> {
        directory.check()?;
        let mut connection = Connection::open_with_flags(
            directory.database_path(),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        directory.check()?;
        configure(&connection, true)?;
        migrations::check_applied(&connection)?;
        let deadline = Arc::new(Deadline::new());
        let handler = deadline.clone();
        connection.progress_handler(PROGRESS_OPS, Some(move || handler.expired()))?;
        Ok(Self {
            connection,
            deadline,
        })
    }
    /// One read snapshot for a whole search request: every port call (FTS, scan, document
    /// fetch, extraction gaps) sees the same committed state. Ends when the guard drops.
    pub fn snapshot(&self) -> Result<SearchSnapshot<'_>, StorageError> {
        self.connection.execute_batch("BEGIN DEFERRED")?;
        Ok(SearchSnapshot {
            index: SqliteSearch::new(&self.connection, Some(&self.deadline)),
        })
    }
    /// Search port outside a snapshot (verification after a failed search).
    pub fn search_index(&self) -> SqliteSearch<'_> {
        SqliteSearch::new(&self.connection, Some(&self.deadline))
    }
    pub fn fulltext_reader(&self) -> FulltextReader<'_> {
        FulltextReader {
            connection: &self.connection,
        }
    }
}

/// Search port inside one read transaction; rolls back and clears the deadline on drop.
pub struct SearchSnapshot<'a> {
    index: SqliteSearch<'a>,
}
impl<'a> Deref for SearchSnapshot<'a> {
    type Target = SqliteSearch<'a>;
    fn deref(&self) -> &Self::Target {
        &self.index
    }
}
impl Drop for SearchSnapshot<'_> {
    fn drop(&mut self) {
        if let Some(deadline) = self.index.deadline {
            deadline.set(None);
        }
        if !self.index.connection.is_autocommit() {
            let _ = self.index.connection.execute_batch("ROLLBACK");
        }
    }
}
