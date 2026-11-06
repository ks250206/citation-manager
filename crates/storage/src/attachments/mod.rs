//! Immutable SHA-256 blobs and transactional, append-only paper associations.
pub(crate) mod files;
use crate::{LibraryDb, LibraryDirectory, StorageError, error::service_error};
use cm_core::{
    ServiceError,
    attachments::{AttachmentError, AttachmentLibrary, AttachmentStatus, ImportAttachment},
    ports::{AttachmentRepository, AttachmentStore},
};
use cm_domain::{Attachment, AttachmentId, StorageKey};
use files::*;
use rusqlite::{TransactionBehavior, params};
use std::{
    fs::{self, File},
    io::{Read, Write},
};
use uuid::Uuid;

#[derive(Clone)]
pub struct ManagedStore {
    pub(crate) directory: LibraryDirectory,
}
impl ManagedStore {
    pub fn new(directory: &LibraryDirectory) -> Result<Self, StorageError> {
        directory.check()?;
        Ok(Self {
            directory: directory.clone(),
        })
    }
    /// Checks actual bytes and size. Unsafe paths are errors, never disguised as missing files.
    pub fn verify(&self, attachment: &Attachment) -> Result<AttachmentStatus, StorageError> {
        self.verify_key(
            attachment.storage_key.as_str(),
            &attachment.sha256,
            attachment.byte_size,
        )
    }
    pub(crate) fn verify_key(
        &self,
        storage_key: &str,
        digest: &[u8; 32],
        size: u64,
    ) -> Result<AttachmentStatus, StorageError> {
        if key(digest) != storage_key {
            return Err(invalid("noncanonical content address"));
        }
        let path = managed_path(&self.directory, storage_key, false)?;
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AttachmentStatus::Missing);
            }
            Err(e) => return Err(e.into()),
        };
        Ok(if hash(file)? == (*digest, size) {
            AttachmentStatus::Present
        } else {
            AttachmentStatus::Corrupt
        })
    }
}
impl AttachmentStore for ManagedStore {
    fn open(&self, storage_key: &StorageKey) -> Result<Box<dyn Read + Send>, ServiceError> {
        let name = storage_key.as_str().rsplit('/').next().unwrap_or("");
        if name.len() != 68
            || !name.ends_with(".pdf")
            || !name.as_bytes()[..64]
                .iter()
                .copied()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ServiceError::InvalidInput("invalid content address".into()));
        }
        let path = managed_path(&self.directory, storage_key.as_str(), false)
            .map_err(|_| ServiceError::Unavailable)?;
        let mut file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ServiceError::NotFound
            } else {
                ServiceError::Unavailable
            }
        })?;
        let (digest, _) = hash(&mut file).map_err(|_| ServiceError::Unavailable)?;
        if key(&digest) != storage_key.as_str() {
            return Err(ServiceError::Unavailable);
        }
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .map_err(|_| ServiceError::Unavailable)?;
        Ok(Box::new(file))
    }
}

pub struct ManagedAttachments<'a> {
    db: &'a mut LibraryDb,
    store: ManagedStore,
}
impl LibraryDb {
    /// Trusted worker composition only. Rejects pairing a DB with another library's file root.
    pub fn managed_attachments(
        &mut self,
        directory: &LibraryDirectory,
    ) -> Result<ManagedAttachments<'_>, StorageError> {
        self.check_directory(directory)?;
        Ok(ManagedAttachments {
            db: self,
            store: ManagedStore::new(directory)?,
        })
    }
    pub(crate) fn check_directory(&self, directory: &LibraryDirectory) -> Result<(), StorageError> {
        directory.check()?;
        if self.connection.path() != directory.database_path().to_str() {
            return Err(StorageError::UnsafePath(directory.path().into()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    None,
    Copy,
    AfterPublish,
    BeforeCommit,
    Commit,
    AfterCommit,
}
impl ManagedAttachments<'_> {
    fn ingest(
        &mut self,
        request: ImportAttachment,
        cut: Cut,
    ) -> Result<Attachment, AttachmentError> {
        if request.original_name.trim().is_empty() || request.media_type.trim().is_empty() {
            return Err(AttachmentError::Service(ServiceError::InvalidInput(
                "attachment name and media type required".into(),
            )));
        }
        self.ingest_storage(request, cut).map_err(|e| match e {
            StorageError::UnsafePath(_) => AttachmentError::UnsafePath,
            StorageError::DigestMismatch => AttachmentError::HashMismatch,
            StorageError::CorruptBlob => AttachmentError::CorruptBlob,
            StorageError::MissingSource => AttachmentError::Service(ServiceError::InvalidInput(
                "attachment source does not exist".into(),
            )),
            StorageError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => {
                AttachmentError::Service(ServiceError::NotFound)
            }
            StorageError::Sqlite(e) => AttachmentError::Service(service_error(e)),
            _ => AttachmentError::Unavailable,
        })
    }
    fn ingest_storage(
        &mut self,
        request: ImportAttachment,
        cut: Cut,
    ) -> Result<Attachment, StorageError> {
        self.db.check_directory(&self.store.directory)?;
        let mut original = source(&request.source).map_err(|error| match error {
            StorageError::Io(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
                StorageError::MissingSource
            }
            other => other,
        })?;
        let staging = tree(&self.store.directory, "attachments/.staging", true)?;
        let temporary = staging.join(format!("{}.part", Uuid::now_v7()));
        let mut output = create_file(&temporary)?;
        sync_dir(&staging)?;
        // A failed/cancelled copy is deliberately retained for read-only recovery reporting.
        if cut == Cut::Copy {
            let mut buffer = [0u8; BUFFER_SIZE];
            let n = original.read(&mut buffer)?;
            output.write_all(&buffer[..n])?;
            output.sync_all()?;
            return Err(injected());
        }
        let (digest, size) = copy(&mut original, &mut output)?;
        drop(output);
        let verified = hash(File::open(&temporary)?)?;
        if verified != (digest, size) || request.expected_sha256.is_some_and(|h| h != digest) {
            return Err(StorageError::DigestMismatch);
        }
        #[cfg(all(test, unix))]
        crate::crash_recovery::checkpoint("attachment-staged", self.db.connection.path());
        let storage_key = key(&digest);
        let destination = managed_path(&self.store.directory, &storage_key, true)?;
        if destination.try_exists()? {
            if self.store.verify_key(&storage_key, &digest, size)? != AttachmentStatus::Present {
                return Err(StorageError::CorruptBlob);
            }
            // Only the unique private temporary is removed. Published blobs are never rolled back.
            fs::remove_file(&temporary)?;
        } else {
            match publish(&temporary, &destination) {
                Ok(()) => {}
                Err(StorageError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if self.store.verify_key(&storage_key, &digest, size)?
                        != AttachmentStatus::Present
                    {
                        return Err(StorageError::CorruptBlob);
                    }
                    fs::remove_file(&temporary)?;
                }
                Err(e) => return Err(e),
            }
        }
        // Also flush a reused blob: another cooperative importer may have just published it.
        File::open(&destination)?.sync_all()?;
        sync_dir(destination.parent().unwrap())?;
        sync_dir(&staging)?;
        #[cfg(all(test, unix))]
        crate::crash_recovery::checkpoint("attachment-published", self.db.connection.path());
        if cut == Cut::AfterPublish {
            return Err(injected());
        }
        let attachment = Attachment {
            id: AttachmentId(Uuid::now_v7()),
            paper_id: request.paper_id,
            storage_key: StorageKey::try_from(storage_key)
                .map_err(|_| invalid("bad generated key"))?,
            sha256: digest,
            byte_size: size,
            original_name: request.original_name,
            media_type: request.media_type,
        };
        let tx = self
            .db
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM papers WHERE id=?1 AND deleted_at IS NULL)",
            [attachment.paper_id.0.to_string()],
            |r| r.get(0),
        )?;
        if !active {
            return Err(StorageError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
        }
        tx.execute("INSERT INTO attachments(id,paper_id,relative_path,file_hash,original_name,mime_type,byte_size,added_at) VALUES (?1,?2,?3,?4,?5,?6,?7,strftime('%Y-%m-%dT%H:%M:%fZ','now'))", params![attachment.id.0.to_string(), attachment.paper_id.0.to_string(), attachment.storage_key.as_str(), digest.as_slice(), attachment.original_name, attachment.media_type, i64::try_from(size).map_err(|_| invalid("attachment too large"))?])?;
        #[cfg(all(test, unix))]
        crate::crash_recovery::checkpoint("attachment-transaction", tx.path());
        if cut == Cut::BeforeCommit {
            return Err(injected());
        }
        if cut == Cut::Commit {
            // Real deferred FK failure at sqlite3 COMMIT, not a simulated pre-commit return.
            tx.pragma_update(None, "defer_foreign_keys", true)?;
            tx.execute(
                "INSERT INTO paper_tags(paper_id,tag_id) VALUES (?1,?2)",
                params![Uuid::now_v7().to_string(), Uuid::now_v7().to_string()],
            )?;
        }
        tx.commit()?;
        #[cfg(all(test, unix))]
        crate::crash_recovery::checkpoint("attachment-committed", self.db.connection.path());
        if cut == Cut::AfterCommit {
            return Err(injected());
        }
        Ok(attachment)
    }
}
fn injected() -> StorageError {
    std::io::Error::other("injected interruption").into()
}
impl AttachmentLibrary for ManagedAttachments<'_> {
    fn import(&mut self, request: ImportAttachment) -> Result<Attachment, AttachmentError> {
        self.ingest(request, Cut::None)
    }
    fn status(&self, id: AttachmentId) -> Result<AttachmentStatus, AttachmentError> {
        let attachment = self
            .db
            .attachments()
            .get(id)
            .map_err(AttachmentError::Service)?
            .ok_or(AttachmentError::Service(ServiceError::NotFound))?;
        self.store.verify(&attachment).map_err(|e| match e {
            StorageError::UnsafePath(_) => AttachmentError::UnsafePath,
            _ => AttachmentError::Unavailable,
        })
    }
}

#[cfg(test)]
mod tests;
