//! Worker-side attachment ingestion. The daemon authorizes local file selection before calling.
use crate::ServiceError;
use cm_domain::{Attachment, AttachmentId, PaperId};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ImportAttachment {
    pub paper_id: PaperId,
    pub source: PathBuf,
    pub original_name: String,
    pub media_type: String,
    /// Optional caller-known digest; copied bytes are always independently re-read and checked.
    pub expected_sha256: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttachmentStatus {
    Present,
    Missing,
    Corrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentError {
    Service(ServiceError),
    UnsafePath,
    HashMismatch,
    CorruptBlob,
    Unavailable,
}
impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Service(e) => e.fmt(f),
            Self::UnsafePath => f.write_str("unsafe attachment path"),
            Self::HashMismatch => f.write_str("attachment digest mismatch"),
            Self::CorruptBlob => f.write_str("existing shared attachment is corrupt"),
            Self::Unavailable => f.write_str("attachment storage unavailable"),
        }
    }
}
impl std::error::Error for AttachmentError {}

pub trait AttachmentLibrary {
    /// Append a reference only after verified, durable byte publication. Existing references survive errors.
    fn import(&mut self, request: ImportAttachment) -> Result<Attachment, AttachmentError>;
    /// Like `import`, but registers `(paper, sha256)` at most once: if the paper already
    /// references exactly this content, that reference is returned and nothing is appended.
    /// The check runs inside the registering write transaction, so importers racing on
    /// separate connections (job runner, library worker) cannot both append it.
    fn import_once(&mut self, request: ImportAttachment) -> Result<Imported, AttachmentError>;
    fn status(&self, id: AttachmentId) -> Result<AttachmentStatus, AttachmentError>;
}

/// Result of `import_once`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub attachment: Attachment,
    /// The paper already referenced this content; nothing was written.
    pub existing: bool,
}

pub struct AttachmentService<L>(L);
impl<L: AttachmentLibrary> AttachmentService<L> {
    pub fn new(library: L) -> Self {
        Self(library)
    }
    pub fn import(&mut self, request: ImportAttachment) -> Result<Attachment, AttachmentError> {
        self.0.import(request)
    }
    pub fn import_once(&mut self, request: ImportAttachment) -> Result<Imported, AttachmentError> {
        self.0.import_once(request)
    }
    pub fn status(&self, id: AttachmentId) -> Result<AttachmentStatus, AttachmentError> {
        self.0.status(id)
    }
}
