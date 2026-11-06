//! Read-only recovery inventory. No scan or failed association deletes a published blob.
use crate::{
    LibraryDb, LibraryDirectory, StorageError,
    attachments::{ManagedStore, files::*},
};
use cm_core::attachments::AttachmentStatus;
use std::{collections::BTreeSet, fs, path::PathBuf};

#[derive(Debug, Default)]
pub struct IntegrityReport {
    pub dangling: Vec<ReferenceIssue>,
    pub corrupt: Vec<ReferenceIssue>,
    pub orphans: Vec<PathBuf>,
    pub partials: Vec<PathBuf>,
    pub unsafe_entries: Vec<PathBuf>,
    /// Missing, unsafe or corrupt diagnostic baseline. Does not block live-library access.
    pub backup_issues: Vec<PathBuf>,
}
#[derive(Debug)]
pub struct ReferenceIssue {
    pub attachment_id: String,
    pub storage_key: String,
}
impl IntegrityReport {
    pub fn is_clean(&self) -> bool {
        self.dangling.is_empty()
            && self.corrupt.is_empty()
            && self.orphans.is_empty()
            && self.partials.is_empty()
            && self.unsafe_entries.is_empty()
            && self.backup_issues.is_empty()
    }
}
impl LibraryDb {
    /// Includes soft-deleted papers. Call with writes quiesced for a stable recovery inventory.
    pub fn check_integrity(
        &self,
        directory: &LibraryDirectory,
    ) -> Result<IntegrityReport, StorageError> {
        self.check_directory(directory)?;
        database_check(&self.connection)?;
        let store = ManagedStore::new(directory)?;
        let mut report = IntegrityReport::default();
        if crate::backup::validate_initial(directory).is_err() {
            report
                .backup_issues
                .push(directory.path().join("backups/initial.sqlite"));
        }
        let mut referenced = BTreeSet::new();
        let mut statement = self.connection.prepare(
            "SELECT id, relative_path, file_hash, byte_size FROM attachments ORDER BY id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let storage_key: String = row.get(1)?;
            let digest: Vec<u8> = row.get(2)?;
            let size =
                u64::try_from(row.get::<_, i64>(3)?).map_err(|_| invalid("negative DB size"))?;
            let issue = ReferenceIssue {
                attachment_id: id,
                storage_key: storage_key.clone(),
            };
            referenced.insert(storage_key.clone());
            match digest
                .try_into()
                .ok()
                .map(|hash| store.verify_key(&storage_key, &hash, size))
            {
                Some(Ok(AttachmentStatus::Present)) => {}
                Some(Ok(AttachmentStatus::Missing)) => report.dangling.push(issue),
                Some(Ok(AttachmentStatus::Corrupt)) => report.corrupt.push(issue),
                _ => report
                    .unsafe_entries
                    .push(directory.path().join(storage_key)),
            }
        }
        let attachment_root = directory.path().join("attachments");
        if fs::symlink_metadata(&attachment_root).is_ok() {
            inventory(directory, "attachments", 0, &referenced, &mut report)?;
        }
        Ok(report)
    }
}
fn inventory(
    root: &LibraryDirectory,
    relative: &str,
    depth: usize,
    referenced: &BTreeSet<String>,
    report: &mut IntegrityReport,
) -> Result<(), StorageError> {
    let path = root.path().join(relative);
    if depth > 3 || tree(root, relative, false).is_err() {
        report.unsafe_entries.push(path);
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            report.unsafe_entries.push(entry.path());
            continue;
        };
        let key = format!("{relative}/{name}");
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            inventory(root, &key, depth + 1, referenced, report)?;
        } else if managed_path(root, &key, false).is_err() {
            report.unsafe_entries.push(entry.path());
        } else if relative == "attachments/.staging" {
            report.partials.push(entry.path());
        } else if !referenced.contains(&key) {
            report.orphans.push(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn database_check(connection: &rusqlite::Connection) -> Result<(), StorageError> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let results = statement.query_map([], |r| r.get::<_, String>(0))?;
    for result in results {
        if result? != "ok" {
            return Err(invalid("SQLite integrity_check failed"));
        }
    }
    if connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(invalid("SQLite foreign_key_check failed"));
    }
    Ok(())
}
