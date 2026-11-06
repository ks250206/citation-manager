//! WAL-safe SQLite snapshots and versioned, fresh-destination portable library bundles.
use crate::{
    LibraryDb, LibraryDirectory, StorageError,
    attachments::{ManagedStore, files::*},
    integrity::database_check,
    migrations::MigrationBackup,
};
use cm_core::attachments::AttachmentStatus;
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

const FORMAT_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupFile {
    pub path: String,
    pub sha256: [u8; 32],
    pub byte_size: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaVersion {
    pub version: String,
    pub checksum: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format_version: u32,
    pub schema_history: Vec<SchemaVersion>,
    pub database: BackupFile,
    /// One entry per distinct blob, even if multiple papers refer to it.
    pub attachments: Vec<BackupFile>,
}

fn open_snapshot(path: &Path, write: bool) -> Result<Connection, StorageError> {
    crate::db::paths::check_file_if_present(path)?;
    if !write {
        // Refuse WAL-format inputs before SQLite can create sidecars in a supposedly immutable bundle.
        let mut header = [0u8; 100];
        File::open(path)?.read_exact(&mut header)?;
        if &header[..16] != b"SQLite format 3\0" || header[18] != 1 || header[19] != 1 {
            return Err(invalid(
                "snapshot must be a standalone DELETE-mode SQLite database",
            ));
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = path.as_os_str().to_owned();
            sidecar.push(suffix);
            if fs::symlink_metadata(Path::new(&sidecar)).is_ok() {
                return Err(invalid("snapshot has sidecar files"));
            }
        }
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_NOFOLLOW
            | if write {
                OpenFlags::SQLITE_OPEN_READ_WRITE
            } else {
                OpenFlags::SQLITE_OPEN_READ_ONLY
            },
    )?;
    db.busy_timeout(Duration::from_secs(5))?;
    Ok(db)
}

/// Produces a standalone SQLite file using the online backup API, including committed WAL pages.
/// Exclusive destination, bounded page buffers and a 30-second overall deadline. Never copies live DB bytes.
fn online_snapshot(source: &Connection, destination: &Path) -> Result<(), StorageError> {
    if !source.is_autocommit() {
        return Err(invalid("backup source must be outside a transaction"));
    }
    let file = create_file(destination)?;
    let mut target = open_snapshot(destination, true)?;
    target.pragma_update(None, "synchronous", "FULL")?;
    {
        let backup = Backup::new(source, &mut target)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if Instant::now() >= deadline {
                return Err(invalid("online backup timed out"));
            }
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => return Err(invalid("unsupported SQLite backup result")),
            }
        }
    }
    // Backup may inherit WAL mode from source. Checkpoint and close as a standalone DELETE-mode DB.
    let mode: String =
        target.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))?;
    if mode != "delete" {
        return Err(invalid("snapshot journal mode"));
    }
    database_check(&target)?;
    target.close().map_err(|(_, e)| StorageError::Sqlite(e))?;
    file.sync_all()?;
    sync_dir(destination.parent().unwrap())?;
    Ok(())
}

/// Safe reusable adapter for the existing migration hook. Every successful call publishes a new snapshot.
/// Caller must quiesce writes from hook start through migration completion (#25).
pub struct SqliteMigrationBackup {
    directory: LibraryDirectory,
    pub completed: Vec<PathBuf>,
}
impl SqliteMigrationBackup {
    pub fn new(directory: &LibraryDirectory) -> Result<Self, StorageError> {
        directory.check()?;
        Ok(Self {
            directory: directory.clone(),
            completed: vec![],
        })
    }
}
impl MigrationBackup for SqliteMigrationBackup {
    fn before_migration(
        &mut self,
        source: &Connection,
        pending: &[&str],
    ) -> Result<(), StorageError> {
        if pending.is_empty() {
            return Err(invalid("migration backup requires pending versions"));
        }
        let stem = format!("pre-migration-{}", Uuid::now_v7());
        let stage = self.directory.path().join(format!("{stem}.partial.sqlite"));
        let target = self.directory.path().join(format!("{stem}.sqlite"));
        self.directory.check()?;
        online_snapshot(source, &stage)?;
        publish(&stage, &target)?;
        sync_dir(self.directory.path())?;
        self.completed.push(target);
        Ok(())
    }
}

pub(crate) fn ensure_initial(
    source: &Connection,
    directory: &LibraryDirectory,
) -> Result<(), StorageError> {
    let backups = tree(directory, "backups", true)?;
    let initial = backups.join("initial.sqlite");
    match fs::symlink_metadata(&initial) {
        Ok(_) => {
            let db = open_snapshot(&initial, false)?;
            database_check(&db)?;
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let stage = backups.join(format!("initial-{}.partial.sqlite", Uuid::now_v7()));
    online_snapshot(source, &stage)?;
    publish(&stage, &initial)?;
    sync_dir(&backups)
}

impl LibraryDb {
    /// Opens with durable automatic baseline and real pre-migration online backup.
    /// Daemon must hold its future exclusive lifecycle/write gate across this call.
    pub fn open_with_safety_backups(directory: &LibraryDirectory) -> Result<Self, StorageError> {
        let backup_path = tree(directory, "backups", true)?;
        let backup_dir = LibraryDirectory::prepare(backup_path)?;
        let mut hook = SqliteMigrationBackup::new(&backup_dir)?;
        Self::open_with_backup(directory, &mut hook)
    }

    /// Snapshot DB first, then copy only the immutable blobs referenced by that snapshot.
    /// Concurrent append-only ingest is safe; deletion/GC/replacement and migrations must be quiesced.
    pub fn backup_library(
        &self,
        directory: &LibraryDirectory,
        destination: impl AsRef<Path>,
    ) -> Result<BackupManifest, StorageError> {
        self.check_directory(directory)?;
        let destination = destination.as_ref();
        let parent = fresh_destination(destination)?;
        if destination.starts_with(directory.path()) || directory.path().starts_with(destination) {
            return Err(StorageError::UnsafePath(destination.into()));
        }
        let stage = parent
            .path()
            .join(format!(".backup-{}.partial", Uuid::now_v7()));
        let stage_dir = LibraryDirectory::prepare(&stage)?;
        sync_dir(parent.path())?;
        let db_path = stage.join("library.sqlite");
        online_snapshot(&self.connection, &db_path)?;
        let snapshot = open_snapshot(&db_path, false)?;
        crate::migrations::validate_current(&snapshot)?;
        let references = references(&snapshot)?;
        let store = ManagedStore::new(directory)?;
        for entry in references.values() {
            if store.verify_key(&entry.path, &entry.sha256, entry.byte_size)?
                != AttachmentStatus::Present
            {
                return Err(invalid("snapshot references missing/corrupt attachment"));
            }
            copy_entry(directory, &stage_dir, entry)?;
        }
        let (sha256, byte_size) = hash(File::open(&db_path)?)?;
        let manifest = BackupManifest {
            format_version: FORMAT_VERSION,
            schema_history: schema_history(&snapshot)?,
            database: BackupFile {
                path: "library.sqlite".into(),
                sha256,
                byte_size,
            },
            attachments: references.into_values().collect(),
        };
        drop(snapshot);
        let json =
            serde_json::to_vec_pretty(&manifest).map_err(|_| invalid("manifest serialization"))?;
        if json.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(invalid("manifest limit exceeded"));
        }
        let mut output = create_file(&stage.join("manifest.json"))?;
        output.write_all(&json)?;
        output.sync_all()?;
        sync_dir(&stage)?;
        // Validate the complete staged artifact before publishing its final directory name.
        validate_bundle(&stage_dir)?;
        fresh_destination(destination)?;
        publish(&stage, destination)?;
        sync_dir(parent.path())?;
        Ok(manifest)
    }
}

/// Restores into a new directory only. Does not activate/replace/open the running library.
/// On failure, existing paths remain untouched; private .restore-*.partial artifacts are retained for diagnosis.
pub fn restore_library(
    bundle: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<LibraryDirectory, StorageError> {
    restore_inner(bundle.as_ref(), destination.as_ref(), |_| Ok(()))
}
fn restore_inner(
    bundle: &Path,
    destination: &Path,
    before_attachments: impl FnOnce(&LibraryDirectory) -> Result<(), StorageError>,
) -> Result<LibraryDirectory, StorageError> {
    absolute(bundle)?;
    if !bundle.is_dir() {
        return Err(StorageError::UnsafePath(bundle.into()));
    }
    let bundle = LibraryDirectory::prepare(bundle)?;
    let manifest = validate_bundle(&bundle)?;
    let parent = fresh_destination(destination)?;
    if destination.starts_with(bundle.path()) || bundle.path().starts_with(destination) {
        return Err(StorageError::UnsafePath(destination.into()));
    }
    let stage = parent
        .path()
        .join(format!(".restore-{}.partial", Uuid::now_v7()));
    let stage_dir = LibraryDirectory::prepare(&stage)?;
    sync_dir(parent.path())?;
    copy_entry(&bundle, &stage_dir, &manifest.database)?;
    before_attachments(&stage_dir)?;
    for entry in &manifest.attachments {
        copy_entry(&bundle, &stage_dir, entry)?;
    }
    let db = open_snapshot(&stage_dir.database_path(), false)?;
    database_check(&db)?;
    crate::migrations::validate_current(&db)?;
    validate_references(&db, &manifest)?;
    let store = ManagedStore::new(&stage_dir)?;
    for entry in &manifest.attachments {
        if store.verify_key(&entry.path, &entry.sha256, entry.byte_size)?
            != AttachmentStatus::Present
        {
            return Err(invalid("restored blob hash mismatch"));
        }
    }
    drop(db);
    sync_dir(&stage)?;
    fresh_destination(destination)?;
    publish(&stage, destination)?;
    sync_dir(parent.path())?;
    LibraryDirectory::prepare(destination)
}

fn copy_entry(
    from: &LibraryDirectory,
    to: &LibraryDirectory,
    entry: &BackupFile,
) -> Result<(), StorageError> {
    let input = entry_path(from, &entry.path, false)?;
    let output = entry_path(to, &entry.path, true)?;
    let mut target = create_file(&output)?;
    if copy(File::open(input)?, &mut target)? != (entry.sha256, entry.byte_size) {
        return Err(invalid("backup copy hash mismatch"));
    }
    drop(target);
    if hash(File::open(&output)?)? != (entry.sha256, entry.byte_size) {
        return Err(invalid("persisted copy hash mismatch"));
    }
    sync_dir(output.parent().unwrap())
}
fn entry_path(
    directory: &LibraryDirectory,
    key: &str,
    create: bool,
) -> Result<PathBuf, StorageError> {
    if key == "library.sqlite" || key == "manifest.json" {
        directory.check()?;
        let path = directory.path().join(key);
        crate::db::paths::check_file_if_present(&path)?;
        Ok(path)
    } else {
        managed_path(directory, key, create)
    }
}
fn references(db: &Connection) -> Result<BTreeMap<String, BackupFile>, StorageError> {
    let mut entries = BTreeMap::new();
    let mut statement = db.prepare(
        "SELECT relative_path, file_hash, byte_size FROM attachments ORDER BY relative_path",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let hash: Vec<u8> = row.get(1)?;
        let sha256: [u8; 32] = hash.try_into().map_err(|_| invalid("invalid DB digest"))?;
        let entry = BackupFile {
            path: path.clone(),
            sha256,
            byte_size: u64::try_from(row.get::<_, i64>(2)?)
                .map_err(|_| invalid("negative DB size"))?,
        };
        if key(&sha256) != path {
            return Err(invalid("noncanonical DB attachment key"));
        }
        if entries.get(&path).is_some_and(|old| old != &entry) {
            return Err(invalid("inconsistent shared blob reference"));
        }
        entries.insert(path, entry);
        if entries.len() > MAX_ATTACHMENTS {
            return Err(invalid("attachment manifest limit"));
        }
    }
    Ok(entries)
}
fn schema_history(db: &Connection) -> Result<Vec<SchemaVersion>, StorageError> {
    Ok(db
        .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?
        .query_map([], |r| {
            Ok(SchemaVersion {
                version: r.get(0)?,
                checksum: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}
fn validate_references(db: &Connection, manifest: &BackupManifest) -> Result<(), StorageError> {
    if schema_history(db)? != manifest.schema_history {
        return Err(invalid("manifest schema history mismatch"));
    }
    let expected = references(db)?;
    let actual: BTreeMap<_, _> = manifest
        .attachments
        .iter()
        .map(|e| (e.path.clone(), e.clone()))
        .collect();
    if expected != actual || actual.len() != manifest.attachments.len() {
        return Err(invalid("manifest attachments differ from DB snapshot"));
    }
    Ok(())
}

/// Strict validation, also useful to clients before presenting a restore action.
pub fn validate_bundle(directory: &LibraryDirectory) -> Result<BackupManifest, StorageError> {
    let path = entry_path(directory, "manifest.json", false)?;
    let input = File::open(path)?;
    if input.metadata()?.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("manifest too large"));
    }
    let mut json = Vec::new();
    input.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut json)?;
    if json.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(invalid("manifest too large"));
    }
    let manifest: BackupManifest =
        serde_json::from_slice(&json).map_err(|_| invalid("malformed manifest"))?;
    if manifest.format_version != FORMAT_VERSION
        || manifest.database.path != "library.sqlite"
        || manifest.attachments.len() > MAX_ATTACHMENTS
    {
        return Err(invalid("unsupported/incomplete backup manifest"));
    }
    let mut expected = BTreeSet::from(["manifest.json".to_owned(), "library.sqlite".to_owned()]);
    for entry in &manifest.attachments {
        if entry.path != key(&entry.sha256) || !expected.insert(entry.path.clone()) {
            return Err(invalid("invalid/duplicate manifest path"));
        }
    }
    let mut allowed_dirs = BTreeSet::new();
    for entry in &manifest.attachments {
        let mut path = Path::new(&entry.path).parent();
        while let Some(parent) = path.filter(|p| !p.as_os_str().is_empty()) {
            allowed_dirs.insert(parent.to_str().unwrap().to_owned());
            path = parent.parent();
        }
    }
    let mut actual = BTreeSet::new();
    inventory_bundle(directory, "", 0, &allowed_dirs, &mut actual)?;
    if actual != expected {
        return Err(invalid("incomplete bundle or unexpected files"));
    }
    for entry in std::iter::once(&manifest.database).chain(manifest.attachments.iter()) {
        if hash(File::open(entry_path(directory, &entry.path, false)?)?)?
            != (entry.sha256, entry.byte_size)
        {
            return Err(invalid("bundle checksum mismatch"));
        }
    }
    let db = open_snapshot(&directory.database_path(), false)?;
    // A portable DB must be standalone; opening it must not require or generate WAL sidecars.
    let journal: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if journal != "delete" {
        return Err(invalid("portable DB is not a standalone snapshot"));
    }
    database_check(&db)?;
    crate::migrations::validate_current(&db)?;
    validate_references(&db, &manifest)?;
    Ok(manifest)
}
fn inventory_bundle(
    root: &LibraryDirectory,
    relative: &str,
    depth: usize,
    allowed_dirs: &BTreeSet<String>,
    files: &mut BTreeSet<String>,
) -> Result<(), StorageError> {
    if depth > 3 {
        return Err(invalid("bundle nesting limit"));
    }
    let path = if relative.is_empty() {
        root.path().to_owned()
    } else {
        tree(root, relative, false)?
    };
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("non-UTF8 bundle name"))?;
        let key = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            if !allowed_dirs.contains(&key) {
                return Err(invalid("unexpected bundle directory"));
            }
            inventory_bundle(root, &key, depth + 1, allowed_dirs, files)?;
        } else {
            entry_path(root, &key, false)?;
            files.insert(key);
            if files.len() > MAX_ATTACHMENTS + 2 {
                return Err(invalid("bundle entry limit"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_core::{
        attachments::{AttachmentLibrary, ImportAttachment},
        papers::{NewPaper, PaperLibrary},
    };

    #[test]
    fn failure_after_staging_database_never_activates_or_overwrites_a_library() {
        let temp = tempfile::tempdir().unwrap();
        let root =
            LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("private")).unwrap();
        let library = LibraryDirectory::prepare(root.path().join("library")).unwrap();
        let mut db = LibraryDb::open(&library).unwrap();
        let paper = db.papers().create(NewPaper::titled("retained")).unwrap();
        let original = root.path().join("original.pdf");
        fs::write(&original, b"original bytes").unwrap();
        let attachment = db
            .managed_attachments(&library)
            .unwrap()
            .import(ImportAttachment {
                paper_id: paper.id,
                source: original.clone(),
                original_name: "original.pdf".into(),
                media_type: "application/pdf".into(),
                expected_sha256: None,
            })
            .unwrap();
        let bundle = root.path().join("bundle");
        db.backup_library(&library, &bundle).unwrap();
        let destination = root.path().join("restored");
        let live_hash = hash(File::open(library.database_path()).unwrap()).unwrap();
        let mut staged = None;
        let result = restore_inner(&bundle, &destination, |stage| {
            assert!(stage.database_path().is_file());
            staged = Some(stage.path().to_owned());
            // Change a bundle blob after manifest validation. The copy must independently detect this.
            fs::write(
                bundle.join(attachment.storage_key.as_str()),
                b"changed after validation",
            )?;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!destination.exists());
        assert!(staged.unwrap().join("library.sqlite").is_file());
        assert_eq!(
            hash(File::open(library.database_path()).unwrap()).unwrap(),
            live_hash
        );
        assert_eq!(fs::read(original).unwrap(), b"original bytes");
        assert!(db.check_integrity(&library).unwrap().is_clean());
    }

    #[test]
    fn atomic_publish_refuses_existing_file_and_directory_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let from = root.join("temporary");
        let to = root.join("existing");
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"old").unwrap();
        assert!(publish(&from, &to).is_err());
        assert_eq!(fs::read(&from).unwrap(), b"new");
        assert_eq!(fs::read(&to).unwrap(), b"old");
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        assert!(publish(&a, &b).is_err());
        assert!(a.exists() && b.exists());
    }
}
