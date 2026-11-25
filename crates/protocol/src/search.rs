//! #18 full-text search and extraction status DTOs (wire re-exports of core contracts).
//! `Command::Search` -> `Reply::Search`; `Command::Fulltext` -> `Reply::Fulltext`.
//! Extraction and index rebuild run as durable jobs (`jobs::kinds::FULLTEXT_EXTRACT`,
//! `jobs::kinds::SEARCH_REBUILD`) and report the shared `JobStatus`.
pub use cm_core::search::{
    extraction::{AttachmentExtraction, ExtractScope, ExtractionState, ExtractionSummary},
    types::*,
};
use cm_domain::{AttachmentId, PaperId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FulltextRequest {
    /// Extraction state of every PDF attachment of a (non-trashed) paper.
    Status(PaperId),
    /// Extraction state of one attachment.
    AttachmentStatus(AttachmentId),
    /// Bounded read of stored page text.
    Pages(PageTextRequest),
    /// #27 bounded page text of a paper's PDF: `attachment` (which must belong to the paper)
    /// or else its first PDF attachment. Same limits and operation as `Pages`.
    PaperPages(PaperPageTextRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperPageTextRequest {
    pub paper: PaperId,
    pub attachment: Option<AttachmentId>,
    pub from_page: u32,
    pub max_pages: u32,
    pub max_chars: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FulltextReply {
    Status(Vec<AttachmentExtraction>),
    Pages(Box<PageTextRead>),
}
