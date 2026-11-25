//! #22 capture wire DTOs shared by the native messaging host (#23), the browser extension
//! (#24) and MCP (#29). The request/resolution types are core's; this module adds the
//! commands. Every command maps to a capture `Operation` checked against the daemon grant.
use crate::{
    domain::PaperId,
    import::{ImportCommit, ImportOutcome, ImportPreview, PreviewId},
    jobs::{JobId, JobStatus},
};
pub use cm_core::capture::{
    CapturePdf, CaptureRequest, CaptureResolution, FieldSource, FoundIdentifier, MetaTag,
    MetadataTier, PdfOrigin,
};
use cm_core::policy::Operation;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CaptureCommand {
    /// Extract + (optional) DOI complement + common #33 preview. Never writes.
    Preview(Box<CaptureRequest>),
    /// Commit this grant's capture preview; optionally fetch one of the preview's PDF
    /// candidates (by index) as a durable `capture.pdf_fetch` job.
    Commit(CaptureCommit),
    Cancel(PreviewId),
    /// Status of this grant's `capture.pdf_fetch` job.
    PdfJob(JobId),
    /// Explicit retry of this grant's failed/cancelled `capture.pdf_fetch` job.
    RetryPdf(JobId),
}
impl CaptureCommand {
    pub fn operation(&self) -> Operation {
        match self {
            Self::Preview(_) => Operation::CaptureSubmit,
            Self::Commit(_) => Operation::CaptureSave,
            Self::Cancel(_) => Operation::CaptureCancel,
            Self::PdfJob(_) => Operation::CapturePdfStatus,
            Self::RetryPdf(_) => Operation::CapturePdfRetry,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureCommit {
    /// Decisions over the capture preview. Without LibraryWrite only Create/Skip.
    pub commit: ImportCommit,
    /// Index into `CapturePreview::resolution.pdf_candidates`. Never a caller URL.
    pub pdf: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturePreview {
    /// Common #33 preview (duplicate matches hidden without LibraryRead).
    pub preview: ImportPreview,
    pub resolution: CaptureResolution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureCommitted {
    pub outcome: ImportOutcome,
    /// Created or merged paper (None when the row was skipped).
    pub paper: Option<PaperId>,
    /// The PDF job, if one was requested. The paper stays saved whatever the job does.
    pub pdf_job: Option<JobStatus>,
    /// Why no PDF job could be submitted (the paper is still saved).
    pub pdf_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CaptureReply {
    Preview(Box<CapturePreview>),
    Committed(Box<CaptureCommitted>),
    Cancelled,
    PdfJob(Box<JobStatus>),
}
