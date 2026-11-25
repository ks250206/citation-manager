//! Selected local sources only. In-process cancellation/report state is not a durable job.
use crate::{domain::*, import::*};
pub use cm_core::{
    attachments::AttachmentStatus,
    import::file_metadata::{FileDisposition, PdfIntent},
    metadata_resolve::Cancellation,
};
use std::path::PathBuf;
#[derive(Debug, Clone)]
pub enum Selection {
    Files(Vec<PathBuf>),
    Folder { path: PathBuf, recursive: bool },
}
#[derive(Debug, Clone)]
pub struct PdfFile {
    pub path: PathBuf,
    pub name: String,
    pub sha256: Option<[u8; 32]>,
    pub bytes: u64,
    pub draft: Option<PaperDraft>,
    pub messages: Vec<String>,
    pub disposition: FileDisposition,
    /// Paper retained after a real bibliographic commit, including attachment failure.
    pub paper: Option<PaperId>,
    pub attachment: Option<AttachmentId>,
}
#[derive(Debug, Clone, Default)]
pub struct PdfReport {
    pub files: Vec<PdfFile>,
    pub assessed: usize,
    pub cancelled: bool,
}
#[derive(Debug, Clone)]
pub enum PdfRequest {
    Inspect {
        selection: Selection,
        intent: PdfIntent,
        cancel: Cancellation,
    },
    /// Uses retained inspected source/digest, never arbitrary UI replacement paths.
    Attach {
        indices: Vec<usize>,
        paper: PaperId,
        cancel: Cancellation,
    },
    /// Binds exactly one retained file to a normal reviewed common preview.
    PreviewNew {
        index: usize,
        explicit: Option<Box<PaperDraft>>,
        destination: ImportDestination,
    },
    CommitNew {
        index: usize,
        commit: ImportCommit,
        cancel: Cancellation,
    },
    RetryAttachment {
        index: usize,
        cancel: Cancellation,
    },
    List(PaperId),
}
#[derive(Debug, Clone)]
pub enum PdfReply {
    Report(PdfReport),
    Preview(ImportPreview),
    Attachments(Vec<(Attachment, AttachmentStatus)>),
}
pub type PdfProgress = Box<dyn Fn(PdfReport) + Send>;
