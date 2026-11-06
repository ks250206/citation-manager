//! SQLite persistence, immutable attachments and WAL-safe backup/restore. Daemon ownership is #25.
pub mod attachments;
pub mod backup;
pub mod db;
mod error;
pub mod integrity;
pub mod migrations;
pub mod repositories;
pub use db::{LibraryDb, paths::LibraryDirectory};
pub use error::StorageError;

#[cfg(all(test, unix))]
mod crash_recovery;

#[cfg(test)]
mod tests {
    #[test]
    fn every_factory_connection_enforces_foreign_keys() {
        let temp = tempfile::tempdir().unwrap();
        let directory =
            super::LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library"))
                .unwrap();
        let file = super::LibraryDb::open(&directory).unwrap();
        let reopened = super::LibraryDb::open(&directory).unwrap();
        let memory = super::LibraryDb::in_memory().unwrap();
        for db in [file, reopened, memory] {
            let error = db
                .connection
                .execute(
                    "INSERT INTO paper_tags(paper_id,tag_id) VALUES ('absent','absent')",
                    [],
                )
                .unwrap_err();
            assert_eq!(
                error.sqlite_error().unwrap().extended_code,
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
            );
        }
    }

    /// Dependency capability probe, not a library schema or repository implementation.
    #[test]
    fn pinned_bundled_sqlite_supports_fts5_and_backup() -> rusqlite::Result<()> {
        let db = rusqlite::Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE VIRTUAL TABLE probe USING fts5(text); INSERT INTO probe VALUES ('citation');",
        )?;
        let count: u32 = db.query_row(
            "SELECT count(*) FROM probe WHERE probe MATCH 'citation'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1);
        let mut copy = rusqlite::Connection::open_in_memory()?;
        rusqlite::backup::Backup::new(&db, &mut copy)?.run_to_completion(
            8,
            std::time::Duration::ZERO,
            None,
        )?;
        assert_eq!(
            copy.query_row("SELECT count(*) FROM probe", [], |r| r.get::<_, u32>(0))?,
            1
        );
        Ok(())
    }
}
