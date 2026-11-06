//! Embedded, checksum-verified migrations. Applied SQL is immutable.
use crate::StorageError;
use rusqlite::{Connection, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct Migration<'a> {
    id: &'a str,
    sql: &'a str,
    checksum: String,
}

/// Trusted composition-root hook for #9. Use SQLite online backup, not a copy of
/// library.sqlite while WAL is live. Returning an error prevents all schema changes.
/// The source must be treated read-only. No backup/file-store implementation is supplied here.
pub trait MigrationBackup {
    fn before_migration(
        &mut self,
        source: &Connection,
        pending: &[&str],
    ) -> Result<(), StorageError>;
}

fn registry() -> Result<Vec<Migration<'static>>, StorageError> {
    let manifest: BTreeMap<String, String> =
        serde_json::from_str(include_str!("../../migrations/manifest.json"))
            .map_err(|_| StorageError::Migration("invalid embedded manifest".into()))?;
    let files = [
        (
            "20261106T090000Z_issue-8_001_papers.sql",
            include_str!("../../migrations/20261106T090000Z_issue-8_001_papers.sql"),
        ),
        (
            "20261106T090000Z_issue-8_002_relations.sql",
            include_str!("../../migrations/20261106T090000Z_issue-8_002_relations.sql"),
        ),
    ];
    if manifest.len() != files.len() {
        return Err(StorageError::Migration("registry/files differ".into()));
    }
    files
        .into_iter()
        .map(|(id, sql)| {
            let checksum = manifest
                .get(id)
                .ok_or_else(|| StorageError::Migration("unregistered migration".into()))?
                .clone();
            Ok(Migration { id, sql, checksum })
        })
        .collect()
}

pub(crate) fn run(
    db: &mut Connection,
    backup: Option<&mut dyn MigrationBackup>,
) -> Result<(), StorageError> {
    run_registry(db, &registry()?, backup)
}

/// Read-only validation for portable snapshots: no migration or implicit repair.
pub(crate) fn validate_current(db: &Connection) -> Result<(), StorageError> {
    let migrations = registry()?;
    let applied = history(db)?;
    verify_history(&migrations, &applied)?;
    if applied.len() != migrations.len() {
        return Err(StorageError::Migration(
            "portable snapshot requires current schema".into(),
        ));
    }
    // A matching history is not proof that a restored DB contains only trusted DDL.
    let mut expected = Connection::open_in_memory()?;
    run(&mut expected, None)?;
    if schema_definition(db)? != schema_definition(&expected)? {
        return Err(StorageError::Migration(
            "portable snapshot schema differs from registry".into(),
        ));
    }
    Ok(())
}

type SchemaObject = (String, String, String, Option<String>);
fn schema_definition(db: &Connection) -> Result<Vec<SchemaObject>, StorageError> {
    Ok(db.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name, tbl_name, sql")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn run_registry(
    db: &mut Connection,
    migrations: &[Migration<'_>],
    backup: Option<&mut dyn MigrationBackup>,
) -> Result<(), StorageError> {
    // Verify the complete binary registry, including already-applied SQL, before writes.
    let mut previous = None;
    for m in migrations {
        if previous.is_some_and(|id| id >= m.id)
            || format!("{:x}", Sha256::digest(m.sql.as_bytes())) != m.checksum
        {
            return Err(StorageError::Migration(
                "unordered IDs or modified embedded SQL".into(),
            ));
        }
        previous = Some(m.id);
    }
    let has_history: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_migrations')", [], |r| r.get(0))?;
    let before = if has_history { history(db)? } else { vec![] };
    if before.is_empty() {
        verify_empty_bootstrap_schema(db)?;
    }
    verify_history(migrations, &before)?;
    let pending = &migrations[before.len()..];
    if !pending.is_empty() && !before.is_empty() {
        let hook = backup.ok_or(StorageError::BackupRequired)?;
        // Online backup cannot use a source connection inside a write transaction.
        // The caller must quiesce library writes during this boundary (#25).
        hook.before_migration(db, &pending.iter().map(|m| m.id).collect::<Vec<_>>())?;
    }
    // Recheck under writer reservation after the backup hook; another migrator
    // must not invalidate the version the backup covered. This is not #25's daemon lock.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
        version TEXT PRIMARY KEY NOT NULL, checksum TEXT NOT NULL, applied_at TEXT NOT NULL
    )",
    )?;
    let applied = history(&tx)?;
    if applied != before {
        return Err(StorageError::Migration(
            "schema changed during backup; reopen".into(),
        ));
    }
    verify_history(migrations, &applied)?;
    if applied.is_empty() {
        verify_empty_bootstrap_schema(&tx)?;
    }
    for m in pending {
        tx.execute_batch(m.sql)?;
        tx.execute("INSERT INTO schema_migrations(version, checksum, applied_at) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))", params![m.id, m.checksum])?;
    }
    tx.commit()?;
    Ok(())
}

fn verify_empty_bootstrap_schema(db: &Connection) -> Result<(), StorageError> {
    let user_objects: u32 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' AND name <> 'schema_migrations'",
        [],
        |r| r.get(0),
    )?;
    if user_objects != 0 {
        return Err(StorageError::Migration(
            "unversioned nonempty database".into(),
        ));
    }
    Ok(())
}

fn history(db: &Connection) -> Result<Vec<(String, String)>, StorageError> {
    Ok(db
        .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?)
}
fn verify_history(
    migrations: &[Migration<'_>],
    applied: &[(String, String)],
) -> Result<(), StorageError> {
    for (index, (id, checksum)) in applied.iter().enumerate() {
        if !migrations
            .get(index)
            .is_some_and(|m| m.id == id && m.checksum == *checksum)
        {
            return Err(StorageError::Migration(
                "unknown/future/non-prefix version or applied checksum mismatch".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LibraryDb, LibraryDirectory};
    use cm_core::{
        papers::{NewPaper, PaperLibrary},
        ports::PaperRepository,
    };
    use std::time::Duration;

    struct Snapshot {
        copy: Connection,
        calls: usize,
    }
    impl Snapshot {
        fn new() -> Self {
            Self {
                copy: Connection::open_in_memory().unwrap(),
                calls: 0,
            }
        }
    }
    impl MigrationBackup for Snapshot {
        fn before_migration(
            &mut self,
            source: &Connection,
            pending: &[&str],
        ) -> Result<(), StorageError> {
            assert!(!pending.is_empty());
            assert!(source.is_autocommit());
            rusqlite::backup::Backup::new(source, &mut self.copy)?.run_to_completion(
                32,
                Duration::ZERO,
                None,
            )?;
            self.calls += 1;
            Ok(())
        }
    }
    fn migration(id: &'static str, sql: &'static str) -> Migration<'static> {
        Migration {
            id,
            sql,
            checksum: format!("{:x}", Sha256::digest(sql.as_bytes())),
        }
    }
    fn schema(db: &Connection) -> Vec<(String, String)> {
        db.prepare(
            "SELECT name,sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn empty_history_cannot_adopt_an_unversioned_nonempty_database() {
        for table in ["foreign_records", "sqliteXrecords"] {
            for empty_history in [false, true] {
                let mut db = Connection::open_in_memory().unwrap();
                db.execute_batch(&format!(
                    "CREATE TABLE {table}(value TEXT); INSERT INTO {table} VALUES ('keep me');"
                ))
                .unwrap();
                if empty_history {
                    db.execute_batch("CREATE TABLE schema_migrations(version TEXT PRIMARY KEY NOT NULL, checksum TEXT NOT NULL, applied_at TEXT NOT NULL)").unwrap();
                }
                let before = schema(&db);
                let mut backup = Snapshot::new();
                assert!(matches!(
                    run_registry(&mut db, &registry().unwrap(), Some(&mut backup)),
                    Err(StorageError::Migration(_))
                ));
                assert_eq!(backup.calls, 0);
                assert_eq!(schema(&db), before);
                let value: String = db
                    .query_row(&format!("SELECT value FROM {table}"), [], |row| row.get(0))
                    .unwrap();
                assert_eq!(value, "keep me");
            }
        }
        let mut blank = Connection::open_in_memory().unwrap();
        blank.execute_batch("CREATE TABLE schema_migrations(version TEXT PRIMARY KEY NOT NULL, checksum TEXT NOT NULL, applied_at TEXT NOT NULL)").unwrap();
        run_registry(&mut blank, &registry().unwrap(), None).unwrap();
        assert_eq!(history(&blank).unwrap().len(), 2);
    }

    #[test]
    fn embedded_registry_matches_manifest_and_initial_bootstrap_is_atomic() {
        let mut db = Connection::open_in_memory().unwrap();
        let mut migrations = registry().unwrap();
        run_registry(&mut db, &migrations, None).unwrap();
        assert_eq!(history(&db).unwrap().len(), 2);
        let original = schema(&db);
        run_registry(&mut db, &migrations, None).unwrap();
        assert_eq!(schema(&db), original);
        migrations[0].sql = "SELECT 'modified already-applied SQL'";
        assert!(matches!(
            run_registry(&mut db, &migrations, None),
            Err(StorageError::Migration(_))
        ));
        assert_eq!(schema(&db), original);
        let mut fresh = Connection::open_in_memory().unwrap();
        let bad = [
            migration(
                "001",
                "CREATE TABLE first(x); INSERT INTO first VALUES (1);",
            ),
            migration("002", "CREATE TABLE partial(x); SELECT * FROM absent;"),
        ];
        assert!(run_registry(&mut fresh, &bad, None).is_err());
        assert!(schema(&fresh).is_empty()); // includes rollback of history bootstrap and first migration
    }

    #[test]
    fn real_old_database_upgrades_with_backup_boundary_and_preserves_records_on_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let directory =
            LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
        directory.create_database().unwrap();
        let migrations = registry().unwrap();
        let mut raw = Connection::open(directory.database_path()).unwrap();
        raw.pragma_update(None, "foreign_keys", true).unwrap();
        run_registry(&mut raw, &migrations[..1], None).unwrap();
        let mut old = LibraryDb { connection: raw };
        let paper = old
            .papers()
            .create(NewPaper::titled("old version data"))
            .unwrap();
        drop(old);
        assert!(matches!(
            LibraryDb::open(&directory),
            Err(StorageError::BackupRequired)
        ));
        let mut backup = Snapshot::new();
        let mut upgraded = LibraryDb::open_with_backup(&directory, &mut backup).unwrap();
        assert_eq!(backup.calls, 1);
        assert_eq!(history(&backup.copy).unwrap().len(), 1);
        assert_eq!(history(&upgraded.connection).unwrap().len(), 2);
        assert_eq!(
            upgraded.papers().get(paper.id).unwrap(),
            Some(paper.clone())
        );
        assert_eq!(
            backup
                .copy
                .query_row("SELECT title FROM papers", [], |r| r.get::<_, String>(0))
                .unwrap(),
            paper.title
        );
        drop(upgraded);
        let mut reopened = LibraryDb::open(&directory).unwrap();
        assert_eq!(reopened.papers().get(paper.id).unwrap(), Some(paper));
    }

    #[test]
    fn applied_checksum_future_versions_and_nonprefix_history_fail_closed() {
        for tamper in [
            "UPDATE schema_migrations SET checksum='wrong' WHERE version LIKE '%001_%'",
            "INSERT INTO schema_migrations VALUES ('20991231T235959Z_issue-8_001_future.sql','future','now')",
            "DELETE FROM schema_migrations WHERE version LIKE '%001_%'",
        ] {
            let mut db = LibraryDb::in_memory().unwrap();
            let paper = db.papers().create(NewPaper::titled("retain me")).unwrap();
            db.connection.execute_batch(tamper).unwrap();
            let before = schema(&db.connection);
            let records = history(&db.connection).unwrap();
            assert!(matches!(
                run(&mut db.connection, None),
                Err(StorageError::Migration(_))
            ));
            assert_eq!(schema(&db.connection), before);
            assert_eq!(history(&db.connection).unwrap(), records);
            assert_eq!(db.papers().get(paper.id).unwrap(), Some(paper));
        }
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE unversioned(x); INSERT INTO unversioned VALUES (42)")
            .unwrap();
        assert!(matches!(
            run(&mut db, None),
            Err(StorageError::Migration(_))
        ));
        assert_eq!(
            db.query_row("SELECT x FROM unversioned", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            42
        );
    }

    #[test]
    fn pending_batch_failure_rolls_back_prior_pending_ddl_data_and_history() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("rollback.sqlite");
        let mut db = Connection::open(&file).unwrap();
        run(&mut db, None).unwrap();
        db.execute_batch("INSERT INTO papers(id,title,created_at,updated_at) VALUES ('retained','before','now','now')").unwrap();
        let before = schema(&db);
        let applied = history(&db).unwrap();
        let mut pending = registry().unwrap();
        pending.push(migration(
            "20261106T100000Z_issue-8_003_good.sql",
            "CREATE TABLE would_exist(x); UPDATE papers SET title='after';",
        ));
        pending.push(migration(
            "20261106T100000Z_issue-8_004_bad.sql",
            "CREATE TABLE partial(x); INSERT INTO does_not_exist VALUES (1);",
        ));
        let mut snapshot = Snapshot::new();
        assert!(run_registry(&mut db, &pending, Some(&mut snapshot)).is_err());
        assert!(db.is_autocommit());
        assert_eq!(schema(&db), before);
        assert_eq!(history(&db).unwrap(), applied);
        assert_eq!(
            db.query_row("SELECT title FROM papers", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "before"
        );
        drop(db);
        let db = Connection::open(file).unwrap();
        assert_eq!(schema(&db), before);
        assert_eq!(history(&db).unwrap(), applied);
        assert_eq!(
            db.query_row("SELECT title FROM papers", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "before"
        );
    }

    #[test]
    fn backup_error_or_raced_schema_aborts_upgrade() {
        struct Refuse;
        impl MigrationBackup for Refuse {
            fn before_migration(&mut self, _: &Connection, _: &[&str]) -> Result<(), StorageError> {
                Err(StorageError::Migration("backup failed".into()))
            }
        }
        let mut db = Connection::open_in_memory().unwrap();
        let migrations = registry().unwrap();
        run_registry(&mut db, &migrations[..1], None).unwrap();
        let before = schema(&db);
        assert!(run_registry(&mut db, &migrations, Some(&mut Refuse)).is_err());
        assert_eq!(schema(&db), before);
        struct Race;
        impl MigrationBackup for Race {
            fn before_migration(
                &mut self,
                source: &Connection,
                _: &[&str],
            ) -> Result<(), StorageError> {
                // Simulate another migrator changing history at the trusted boundary.
                source.execute("UPDATE schema_migrations SET checksum='changed'", [])?;
                Ok(())
            }
        }
        assert!(matches!(
            run_registry(&mut db, &migrations, Some(&mut Race)),
            Err(StorageError::Migration(_))
        ));
        assert_eq!(schema(&db), before);
    }
}
