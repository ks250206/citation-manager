#![cfg(unix)]
use cm_core::{
    attachments::{AttachmentLibrary, ImportAttachment},
    papers::{NewPaper, PaperLibrary},
    ports::{AttachmentRepository, AttachmentStore, PaperRepository},
};
use cm_domain::{Attachment, AttachmentId, PaperId};
use cm_storage::{
    LibraryDb, LibraryDirectory,
    attachments::ManagedStore,
    backup::{BackupManifest, restore_library, validate_bundle},
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use uuid::Uuid;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    directory: LibraryDirectory,
    db: LibraryDb,
    paper: PaperId,
    attachment: Attachment,
    source: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("private");
        LibraryDirectory::prepare(&root).unwrap();
        let directory = LibraryDirectory::prepare(root.join("library")).unwrap();
        let mut db = LibraryDb::open(&directory).unwrap();
        let mut draft = NewPaper::titled("Backup metadata 日本語");
        draft.bibliography.doi = Some("10.1234/backup".into());
        draft.bibliography.journal = Some("WAL Journal".into());
        draft.bibliography.extra_json = Some(serde_json::json!({"evidence": [1, 2, 3]}));
        let paper = db.papers().create(draft).unwrap().id;
        let source = root.join("original.pdf");
        fs::write(&source, b"%PDF-1.7\nimmutable original bytes\n%%EOF").unwrap();
        let attachment = db
            .managed_attachments(&directory)
            .unwrap()
            .import(ImportAttachment {
                paper_id: paper,
                source: source.clone(),
                original_name: "original.pdf".into(),
                media_type: "application/pdf".into(),
                expected_sha256: None,
            })
            .unwrap();
        Self {
            _temp: temp,
            root,
            directory,
            db,
            paper,
            attachment,
            source,
        }
    }
    fn backup(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        self.db.backup_library(&self.directory, &path).unwrap();
        path
    }
}
fn read_manifest(bundle: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(bundle.join("manifest.json")).unwrap()).unwrap()
}
fn write_manifest(bundle: &Path, value: &serde_json::Value) {
    fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}
fn digest(path: &Path) -> [u8; 32] {
    Sha256::digest(fs::read(path).unwrap()).into()
}
fn refresh_database_manifest(bundle: &Path) {
    let mut manifest = read_manifest(bundle);
    let path = bundle.join("library.sqlite");
    manifest["database"]["sha256"] = serde_json::to_value(digest(&path)).unwrap();
    manifest["database"]["byte_size"] = serde_json::json!(fs::metadata(path).unwrap().len());
    write_manifest(bundle, &manifest);
}

#[test]
fn wal_backup_roundtrip_retains_full_metadata_relations_annotations_and_immutable_files() {
    let mut fixture = Fixture::new();
    let original_hash = digest(&fixture.source);
    let second = fixture
        .db
        .papers()
        .create(NewPaper::titled("Shared reference"))
        .unwrap();
    fixture
        .db
        .managed_attachments(&fixture.directory)
        .unwrap()
        .import(ImportAttachment {
            paper_id: second.id,
            source: fixture.source.clone(),
            original_name: "same.pdf".into(),
            media_type: "application/pdf".into(),
            expected_sha256: Some(original_hash),
        })
        .unwrap();
    // Keep a real WAL writer open with autocheckpoint disabled. These records have not been checkpointed.
    let raw = Connection::open(fixture.directory.database_path()).unwrap();
    raw.execute_batch("PRAGMA foreign_keys=ON; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    let annotation = Uuid::now_v7().to_string();
    raw.execute("INSERT INTO annotations(id,attachment_id,page_index,page_label,type,source,anchor_state,quote_text,comment,quads_json,created_at,updated_at) VALUES (?1,?2,0,'iv','highlight','ai','resolved','quoted evidence','retained comment','[]','now','now')", params![annotation,fixture.attachment.id.0.to_string()]).unwrap();
    raw.execute("INSERT INTO annotations(id,attachment_id,type,source,anchor_state,unresolved_reason,quote_text,comment,created_at,updated_at) VALUES (?1,?2,'note','import','unresolved','PDF changed','unresolved quote','preserve me','now','now')", params![Uuid::now_v7().to_string(),fixture.attachment.id.0.to_string()]).unwrap();
    raw.execute("INSERT INTO fulltext_pages(attachment_id,page_index,page_label,text,extraction_status,updated_at) VALUES (?1,0,'iv','page evidence','complete','now')", [fixture.attachment.id.0.to_string()]).unwrap();
    raw.execute_batch("INSERT INTO tags VALUES ('tag','Retained','retained',NULL); INSERT INTO collections VALUES ('collection',NULL,'Folder',0)").unwrap();
    raw.execute(
        "INSERT INTO paper_tags VALUES (?1,'tag')",
        [fixture.paper.0.to_string()],
    )
    .unwrap();
    raw.execute(
        "INSERT INTO paper_collections VALUES (?1,'collection')",
        [fixture.paper.0.to_string()],
    )
    .unwrap();
    assert!(
        fs::metadata(fixture.directory.path().join("library.sqlite-wal"))
            .unwrap()
            .len()
            > 32
    );
    let expected = fixture.db.papers().get(fixture.paper).unwrap().unwrap();
    let bundle = fixture.backup("portable");
    let manifest = validate_bundle(&LibraryDirectory::prepare(&bundle).unwrap()).unwrap();
    assert_eq!(manifest.attachments.len(), 1);
    let destination = fixture.root.join("restored");
    let restored = restore_library(&bundle, &destination).unwrap();
    let mut reopened = LibraryDb::open(&restored).unwrap();
    assert_eq!(
        reopened.papers().get(fixture.paper).unwrap(),
        Some(expected)
    );
    assert_eq!(
        reopened.attachments().get(fixture.attachment.id).unwrap(),
        Some(fixture.attachment.clone())
    );
    assert!(reopened.check_integrity(&restored).unwrap().is_clean());
    let result = Connection::open(restored.database_path()).unwrap();
    assert_eq!(
        result
            .query_row("SELECT count(*) FROM annotations", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        2
    );
    let retained: (String, String, String, String) = result
        .query_row(
            "SELECT source,page_label,quote_text,comment FROM annotations WHERE id=?1",
            [annotation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        retained,
        (
            "ai".into(),
            "iv".into(),
            "quoted evidence".into(),
            "retained comment".into()
        )
    );
    for table in [
        "tags",
        "paper_tags",
        "collections",
        "paper_collections",
        "fulltext_pages",
    ] {
        assert_eq!(
            result
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, u32>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(
        result
            .query_row(
                "SELECT unresolved_reason FROM annotations WHERE anchor_state='unresolved'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "PDF changed"
    );
    let mut bytes = vec![];
    ManagedStore::new(&restored)
        .unwrap()
        .open(&fixture.attachment.storage_key)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, fs::read(&fixture.source).unwrap());
    assert_eq!(digest(&fixture.source), original_hash);
    assert_eq!(
        digest(
            &fixture
                .directory
                .path()
                .join(fixture.attachment.storage_key.as_str())
        ),
        original_hash
    );
    assert!(
        fixture
            .db
            .backup_library(&fixture.directory, &bundle)
            .is_err()
    );
    assert!(restore_library(&bundle, fixture.directory.path()).is_err());
    assert!(restore_library(&bundle, &destination).is_err());
    assert_eq!(
        fixture.db.attachments().get(fixture.attachment.id).unwrap(),
        Some(fixture.attachment)
    );
}

#[test]
fn snapshot_references_are_stable_after_new_ingest_and_soft_delete() {
    let mut fixture = Fixture::new();
    let first = fixture.backup("snapshot");
    let later = fixture
        .db
        .papers()
        .create(NewPaper::titled("after snapshot"))
        .unwrap();
    let next = fixture.root.join("new.pdf");
    fs::write(&next, b"new content").unwrap();
    fixture
        .db
        .managed_attachments(&fixture.directory)
        .unwrap()
        .import(ImportAttachment {
            paper_id: later.id,
            source: next,
            original_name: "new.pdf".into(),
            media_type: "application/pdf".into(),
            expected_sha256: None,
        })
        .unwrap();
    fixture
        .db
        .papers()
        .set_deleted(fixture.paper, true)
        .unwrap();
    let restored = restore_library(&first, fixture.root.join("restored")).unwrap();
    let mut db = LibraryDb::open(&restored).unwrap();
    assert!(db.papers().get(later.id).unwrap().is_none());
    assert!(db.papers().get(fixture.paper).unwrap().is_some());
    let deleted_bundle = fixture.backup("with-trash");
    let deleted = restore_library(deleted_bundle, fixture.root.join("deleted-restored")).unwrap();
    let mut db = LibraryDb::open(&deleted).unwrap();
    assert!(db.papers().get(fixture.paper).unwrap().is_none());
    db.papers().set_deleted(fixture.paper, false).unwrap();
    assert_eq!(
        db.attachments().get(fixture.attachment.id).unwrap(),
        Some(fixture.attachment)
    );
}

#[test]
fn malformed_traversing_absolute_duplicate_unsupported_and_incomplete_manifests_fail_closed() {
    let fixture = Fixture::new();
    let db_hash = digest(&fixture.directory.database_path());
    let source_hash = digest(&fixture.source);
    for (index, change) in [
        "../outside.pdf",
        "/tmp/absolute.pdf",
        "attachments/../../outside.pdf",
        "attachments\\evil.pdf",
        "version",
        "missing",
        "duplicate",
        "unknown",
        "schema",
        "database",
    ]
    .into_iter()
    .enumerate()
    {
        let bundle = fixture.backup(&format!("bundle-{index}"));
        let mut value = read_manifest(&bundle);
        match change {
            "version" => value["format_version"] = 999.into(),
            "missing" => {
                value.as_object_mut().unwrap().remove("database");
            }
            "duplicate" => {
                let duplicate = value["attachments"][0].clone();
                value["attachments"].as_array_mut().unwrap().push(duplicate);
            }
            "unknown" => value["unexpected"] = true.into(),
            "schema" => value["schema_history"][0]["checksum"] = "wrong".into(),
            "database" => value["database"]["path"] = "../library.sqlite".into(),
            _ => value["attachments"][0]["path"] = change.into(),
        }
        write_manifest(&bundle, &value);
        let destination = fixture.root.join(format!("restore-{index}"));
        assert!(
            restore_library(&bundle, &destination).is_err(),
            "accepted {change}"
        );
        assert!(!destination.exists());
        assert_eq!(digest(&fixture.directory.database_path()), db_hash);
        assert_eq!(digest(&fixture.source), source_hash);
    }
    let bundle = fixture.backup("invalid-json");
    fs::write(bundle.join("manifest.json"), b"{broken json").unwrap();
    assert!(restore_library(bundle, fixture.root.join("malformed-restored")).is_err());
}

#[test]
fn missing_corrupt_and_linked_bundle_files_and_schema_mismatches_are_rejected() {
    let fixture = Fixture::new();
    let original = digest(&fixture.source);
    for variant in [
        "missing",
        "corrupt",
        "symlink",
        "hardlink",
        "shard-link",
        "db-tamper",
        "unknown-file",
        "wal",
        "public-file",
        "future-history",
        "db-reference",
        "fk",
        "unexpected-empty-directory",
    ] {
        let bundle = fixture.backup(&format!("bundle-{variant}"));
        let path = bundle.join(fixture.attachment.storage_key.as_str());
        match variant {
            "missing" => fs::remove_file(&path).unwrap(),
            "corrupt" => fs::write(&path, b"changed").unwrap(),
            "symlink" => {
                fs::remove_file(&path).unwrap();
                symlink(&fixture.source, &path).unwrap();
            }
            "hardlink" => {
                fs::remove_file(&path).unwrap();
                fs::hard_link(&fixture.source, &path).unwrap();
            }
            "shard-link" => {
                let parent = path.parent().unwrap();
                let moved = fixture.root.join("moved-shard");
                fs::rename(parent, &moved).unwrap();
                symlink(&moved, parent).unwrap();
            }
            "db-tamper" => fs::write(bundle.join("library.sqlite"), b"invalid sqlite").unwrap(),
            "unknown-file" => {
                File::create(bundle.join("extra")).unwrap();
            }
            "wal" => {
                File::create(bundle.join("library.sqlite-wal")).unwrap();
            }
            "public-file" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            "unexpected-empty-directory" => {
                LibraryDirectory::prepare(bundle.join("attachments/unexpected")).unwrap();
            }
            _ => {
                let db = Connection::open(bundle.join("library.sqlite")).unwrap();
                db.pragma_update(None, "foreign_keys", false).unwrap();
                match variant {
                    "future-history" => {
                        db.execute("UPDATE schema_migrations SET version='future' WHERE version LIKE '%002_%'", []).unwrap();
                    }
                    "db-reference" => {
                        db.execute("UPDATE attachments SET byte_size=100", [])
                            .unwrap();
                    }
                    _ => {
                        db.execute("UPDATE attachments SET paper_id='missing'", [])
                            .unwrap();
                    }
                }
                drop(db);
                refresh_database_manifest(&bundle);
            }
        }
        assert!(
            restore_library(&bundle, fixture.root.join(format!("restore-{variant}"))).is_err(),
            "accepted {variant}"
        );
        assert_eq!(digest(&fixture.source), original);
        if variant == "hardlink" {
            fs::remove_file(path).unwrap();
        }
    }
}

#[test]
fn failed_restore_preserves_existing_destination_and_unrelated_paths() {
    let fixture = Fixture::new();
    let bundle = fixture.backup("bundle");
    let unrelated = fixture.root.join("unrelated");
    fs::write(&unrelated, b"keep unrelated").unwrap();
    for destination in [fixture.root.join("existing"), fixture.root.join("linked")] {
        if destination.ends_with("existing") {
            let dir = LibraryDirectory::prepare(&destination).unwrap();
            fs::write(dir.path().join("keep"), b"untouched").unwrap();
        } else {
            symlink(fixture.directory.path(), &destination).unwrap();
        }
        assert!(restore_library(&bundle, &destination).is_err());
    }
    assert_eq!(
        fs::read(fixture.root.join("existing/keep")).unwrap(),
        b"untouched"
    );
    assert_eq!(fs::read(&unrelated).unwrap(), b"keep unrelated");
    let destination = fixture.root.join("fresh");
    fs::write(
        bundle.join(fixture.attachment.storage_key.as_str()),
        b"corrupt",
    )
    .unwrap();
    assert!(restore_library(&bundle, &destination).is_err());
    assert!(!destination.exists());
    assert!(!fixture.root.join("outside.pdf").exists());
    assert!(restore_library(&bundle, fixture.root.join("../traversal")).is_err());
}

#[test]
fn backup_refuses_missing_or_corrupt_source_blobs_and_unsafe_destinations() {
    let fixture = Fixture::new();
    let blob = fixture
        .directory
        .path()
        .join(fixture.attachment.storage_key.as_str());
    fs::write(&blob, b"corrupt").unwrap();
    let destination = fixture.root.join("failed-backup");
    assert!(
        fixture
            .db
            .backup_library(&fixture.directory, &destination)
            .is_err()
    );
    assert!(!destination.exists());
    fs::remove_file(&blob).unwrap();
    assert!(
        fixture
            .db
            .backup_library(&fixture.directory, fixture.root.join("missing-backup"))
            .is_err()
    );
    assert!(
        fixture
            .db
            .backup_library(&fixture.directory, fixture.directory.path().join("nested"))
            .is_err()
    );
    let foreign = LibraryDirectory::prepare(fixture.root.join("foreign")).unwrap();
    assert!(
        fixture
            .db
            .backup_library(&foreign, fixture.root.join("foreign-backup"))
            .is_err()
    );
}

#[test]
fn baseline_is_real_valid_idempotent_and_never_overwritten() {
    let fixture = Fixture::new();
    let baseline = fixture.directory.path().join("backups/initial.sqlite");
    let before = digest(&baseline);
    let db = Connection::open(&baseline).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM papers", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    drop(db);
    drop(LibraryDb::open_with_safety_backups(&fixture.directory).unwrap());
    assert_eq!(digest(&baseline), before);
    assert_eq!(
        fs::metadata(&baseline).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

// Compile-time reusable DTO/API check for later clients; uses no user library paths.
fn _public_contract(_: BackupManifest, _: AttachmentId) {}

#[test]
fn corrupt_diagnostic_baseline_is_reported_without_blocking_live_library_recovery() {
    let fixture = Fixture::new();
    let baseline = fixture.directory.path().join("backups/initial.sqlite");
    // Exercise both a damaged data page and a damaged header.
    let good = fs::read(&baseline).unwrap();
    for offset in [4096, 0] {
        let mut damaged = good.clone();
        damaged[offset] = 0xff;
        fs::write(&baseline, &damaged).unwrap();
        let reopened = LibraryDb::open_with_safety_backups(&fixture.directory).unwrap();
        let report = reopened.check_integrity(&fixture.directory).unwrap();
        assert_eq!(report.backup_issues, vec![baseline.clone()]);
        assert!(!report.is_clean());
        let bundle = fixture.root.join(format!("recovery-{offset}"));
        reopened
            .backup_library(&fixture.directory, &bundle)
            .unwrap();
        let restored =
            restore_library(&bundle, fixture.root.join(format!("restored-{offset}"))).unwrap();
        assert!(
            LibraryDb::open(&restored)
                .unwrap()
                .papers()
                .get(fixture.paper)
                .is_ok()
        );
        assert_eq!(fs::read(&baseline).unwrap(), damaged);
    }
    fs::write(&baseline, good).unwrap();
    assert!(
        fixture
            .db
            .check_integrity(&fixture.directory)
            .unwrap()
            .is_clean()
    );
    // Unsafe private-file boundaries still fail closed instead of following a link.
    fs::remove_file(&baseline).unwrap();
    symlink(&fixture.source, &baseline).unwrap();
    assert!(LibraryDb::open(&fixture.directory).is_err());
}

#[test]
fn bundle_ddl_changes_are_rejected_even_after_rehashing_the_database() {
    let fixture = Fixture::new();
    for (index, sql) in [
        "CREATE TRIGGER evil AFTER INSERT ON tags BEGIN DELETE FROM papers; END",
        "DROP INDEX papers_visible_id",
        "ALTER TABLE tags ADD COLUMN untrusted TEXT",
    ]
    .iter()
    .enumerate()
    {
        let bundle = fixture.backup(&format!("ddl-{index}"));
        let connection = Connection::open(bundle.join("library.sqlite")).unwrap();
        connection.execute_batch(sql).unwrap();
        drop(connection);
        refresh_database_manifest(&bundle);
        let directory = LibraryDirectory::prepare(&bundle).unwrap();
        assert!(validate_bundle(&directory).is_err());
        let destination = fixture.root.join(format!("bad-restore-{index}"));
        assert!(restore_library(&bundle, &destination).is_err());
        assert!(!destination.exists());
        assert!(
            fixture
                .db
                .check_integrity(&fixture.directory)
                .unwrap()
                .is_clean()
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn case_aliases_cannot_place_backups_or_restores_inside_their_source() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let alias = fixture.root.join("LIBRARY");
    if !alias.exists() {
        return;
    } // This volume is case-sensitive.
    assert_eq!(
        fs::metadata(&alias).unwrap().ino(),
        fs::metadata(fixture.directory.path()).unwrap().ino()
    );
    let destination = alias.join("bundle");
    assert!(
        fixture
            .db
            .backup_library(&fixture.directory, &destination)
            .is_err()
    );
    assert!(!destination.exists());
    let bundle = fixture.backup("portable");
    let alias = fixture.root.join("PORTABLE");
    let destination = alias.join("restored");
    assert!(restore_library(&bundle, &destination).is_err());
    assert!(!destination.exists());
    assert!(
        fixture
            .db
            .check_integrity(&fixture.directory)
            .unwrap()
            .is_clean()
    );
}

#[test]
fn real_pre_migration_hook_snapshots_old_wal_state_before_upgrading() {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let temp = tempfile::tempdir().unwrap();
    let directory =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("old-library")).unwrap();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.database_path())
        .unwrap();
    let raw = Connection::open(directory.database_path()).unwrap();
    raw.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA foreign_keys=ON",
    )
    .unwrap();
    let sql = include_str!("../migrations/20261106T090000Z_issue-8_001_papers.sql");
    raw.execute_batch(sql).unwrap();
    raw.execute_batch("CREATE TABLE schema_migrations(version TEXT PRIMARY KEY NOT NULL, checksum TEXT NOT NULL, applied_at TEXT NOT NULL)").unwrap();
    raw.execute(
        "INSERT INTO schema_migrations VALUES (?1,?2,'now')",
        params![
            "20261106T090000Z_issue-8_001_papers.sql",
            format!("{:x}", Sha256::digest(sql.as_bytes()))
        ],
    )
    .unwrap();
    let id = Uuid::now_v7().to_string();
    raw.execute("INSERT INTO papers(id,title,created_at,updated_at) VALUES (?1,'retained old WAL record','now','now')", [&id]).unwrap();
    assert!(matches!(
        LibraryDb::open(&directory),
        Err(cm_storage::StorageError::BackupRequired)
    ));
    let upgraded = LibraryDb::open_with_safety_backups(&directory).unwrap();
    assert!(upgraded.foreign_keys_enabled().unwrap());
    let snapshots: Vec<_> = fs::read_dir(directory.path().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("pre-migration-")
        })
        .collect();
    assert_eq!(snapshots.len(), 1);
    let old = Connection::open(&snapshots[0]).unwrap();
    assert_eq!(
        old.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        old.query_row("SELECT title FROM papers WHERE id=?1", [&id], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "retained old WAL record"
    );
    assert_eq!(
        old.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        old.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert!(raw.prepare("SELECT * FROM attachments").is_ok());
}

#[test]
fn refused_migration_snapshot_never_changes_existing_data_or_history() {
    use cm_storage::{backup::SqliteMigrationBackup, migrations::MigrationBackup};
    let fixture = Fixture::new();
    let raw = Connection::open(fixture.directory.database_path()).unwrap();
    let foreign = LibraryDirectory::prepare(fixture.root.join("backup-target")).unwrap();
    let mut hook = SqliteMigrationBackup::new(&foreign).unwrap();
    // Hook errors fail closed, and the trusted source is treated read-only.
    assert!(hook.before_migration(&raw, &[]).is_err());
    fs::set_permissions(foreign.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(hook.before_migration(&raw, &["next"]).is_err());
    assert!(hook.completed.is_empty());
    assert_eq!(
        raw.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        raw.query_row("SELECT title FROM papers", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "Backup metadata 日本語"
    );
}
