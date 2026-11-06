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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn status(&self, id: AttachmentId) -> Result<AttachmentStatus, AttachmentError>;
}

pub struct AttachmentService<L>(L);
impl<L: AttachmentLibrary> AttachmentService<L> {
    pub fn new(library: L) -> Self {
        Self(library)
    }
    pub fn import(&mut self, request: ImportAttachment) -> Result<Attachment, AttachmentError> {
        self.0.import(request)
    }
    pub fn status(&self, id: AttachmentId) -> Result<AttachmentStatus, AttachmentError> {
        self.0.status(id)
    }
}
