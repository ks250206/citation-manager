//! Small synchronous worker-side ports; callers keep blocking I/O off the UI thread.
//! Transactions, pagination and job orchestration are specified by their feature issues.
use crate::{ServiceError, import::*};
use cm_domain::{Attachment, AttachmentId, Paper, PaperId, StorageKey};

pub trait PaperRepository {
    fn get(&self, id: PaperId) -> Result<Option<Paper>, ServiceError>;
    /// Save one logical record; adapter must preserve external identifiers.
    /// SQLite implements this as insert-only (explicit internal ID), never an upsert.
    /// For generated IDs, updates and lifecycle operations use papers::PaperLibrary.
    fn save(&mut self, paper: &Paper) -> Result<(), ServiceError>;
}

pub trait AttachmentRepository {
    fn get(&self, id: AttachmentId) -> Result<Option<Attachment>, ServiceError>;
    fn for_paper(&self, id: PaperId) -> Result<Vec<Attachment>, ServiceError>;
}

pub trait AttachmentStore {
    /// Resolve keys only inside the managed root, including symlink containment checks.
    fn open(&self, key: &StorageKey) -> Result<Box<dyn std::io::Read + Send>, ServiceError>;
}

/// All import adapters feed neutral candidates here. Preview never persists a Paper
/// or Attachment (temporary preview state is allowed); commit revalidates the preview
/// and decisions and writes atomically. Neither DTO is proof of authorization.
/// Authorization is enforced by the daemon before invoking a service (#25).
pub trait ImportService {
    fn preview(&mut self, request: ImportRequest) -> Result<ImportPreview, ServiceError>;
    fn commit(&mut self, request: ImportCommit) -> Result<ImportOutcome, ServiceError>;
}
