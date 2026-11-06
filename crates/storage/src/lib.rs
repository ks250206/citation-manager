//! SQLite / file-store boundary; repositories and migrations are implemented in #8.

#[cfg(test)]
mod tests {
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
