//! Shared worker-side import application service. Transport authorization belongs to #25.
use cm_domain::{Bibliography, CollectionId, ExternalId, Paper, PaperId, TagId};
pub mod citation_keys;
pub mod dedup;
pub mod file_metadata;
pub mod normalize;
mod service;
pub use service::{ImportCoordinator, ImportPolicy};

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PaperDraft {
    pub title: String,
    pub external_ids: Vec<ExternalId>,
    pub bibliography: Bibliography,
    pub source: ImportSource,
    pub diagnostics: Vec<RowDiagnostic>,
}
impl PaperDraft {
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ImportSource {
    pub adapter: String,
    pub locator: Option<String>,
    pub row: Option<u64>,
    /// Adapter-computed digest of its input; never authority to read a file.
    pub sha256: Option<[u8; 32]>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RowDiagnostic {
    pub field: String,
    pub message: String,
    pub error: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportDestination {
    pub collection: Option<CollectionId>,
    pub tags: Vec<TagId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    pub candidates: Vec<PaperDraft>,
    pub destination: ImportDestination,
}
/// Service-local retained state handle; NOT a credential. Never log private previews.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewId(pub String);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewCandidate {
    pub draft: PaperDraft,
    /// Legacy convenience: first exact persisted match only.
    pub existing_match: Option<PaperId>,
    pub matches: Vec<DuplicateCandidate>,
    pub batch_matches: Vec<usize>,
    pub diagnostics: Vec<RowDiagnostic>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Doi,
    ExternalId,
    TitleAuthorYear,
    SimilarTitle,
}
impl MatchKind {
    pub fn exact(self) -> bool {
        matches!(self, Self::Doi | Self::ExternalId)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCandidate {
    pub existing: Paper,
    pub kind: MatchKind,
    pub differences: Vec<FieldDiff>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    /// title, external_ids, or a Bibliography JSON field name.
    pub field: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPreview {
    pub id: PreviewId,
    pub candidates: Vec<PreviewCandidate>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAction {
    Create,
    /// Explicit field selection from a reviewed persisted match. No arbitrary payload.
    Merge {
        match_index: usize,
        fields: Vec<String>,
    },
    Skip,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecision {
    pub candidate_index: usize,
    pub action: ImportAction,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCommit {
    pub preview_id: PreviewId,
    pub decisions: Vec<ImportDecision>,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportOutcome {
    pub created: Vec<PaperId>,
    pub merged: Vec<PaperId>,
    pub skipped: usize,
    pub rows: Vec<ImportRowOutcome>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRowOutcome {
    pub candidate_index: usize,
    pub paper: Option<PaperId>,
    pub source: ImportSource,
    pub diagnostics: Vec<RowDiagnostic>,
}
/// Consistent bounded inventory including trash. The adapter-local revision detects
/// intervening writes, including changes back to the original value.
#[derive(Debug, Clone)]
pub struct ImportSnapshot {
    pub revision: String,
    pub papers: Vec<Paper>,
}
/// One write transaction must cover revalidation, paper writes and membership.
pub trait ImportBackend {
    fn snapshot(
        &mut self,
        destination: &ImportDestination,
    ) -> Result<ImportSnapshot, crate::ServiceError>;
    fn apply(&mut self, approved: &PreparedImport) -> Result<ImportOutcome, crate::ServiceError>;
}
/// Only the coordinator constructs this. Backends call writes inside their write
/// transaction against a fresh snapshot before executing any mutation.
pub struct PreparedImport {
    pub(crate) revision: String,
    pub(crate) preview: ImportPreview,
    pub(crate) destination: ImportDestination,
    pub(crate) decisions: Vec<ImportDecision>,
}
#[derive(Debug, Clone)]
pub enum ImportWrite {
    Create { index: usize, draft: PaperDraft },
    Merge { index: usize, paper: Paper },
}
impl PreparedImport {
    pub fn destination(&self) -> &ImportDestination {
        &self.destination
    }
    pub fn preview(&self) -> &ImportPreview {
        &self.preview
    }
}
/// Raw candidates cannot be committed.
/// ```compile_fail
/// use cm_core::{ports::ImportService, import::ImportRequest};
/// fn bypass(service: &mut dyn ImportService, input: ImportRequest) { service.commit(input); }
/// ```
const _: () = ();
