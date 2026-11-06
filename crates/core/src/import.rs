//! Neutral import DTOs shared by DOI/browser/BibTeX/RIS/CLI/MCP adapters.
//! No HTTP, Native Messaging or MCP envelopes. Full merge policies belong to #33 (adapters: #13/#14).
use cm_domain::{CollectionId, ExternalId, PaperId, TagId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperDraft {
    pub title: String,
    pub external_ids: Vec<ExternalId>,
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

/// Opaque application handle. Service binds it to stored preview state and grant;
/// it is never a credential, client-provided permission, or serialized paper payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewCandidate {
    pub draft: PaperDraft,
    pub existing_match: Option<PaperId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPreview {
    pub id: PreviewId,
    pub candidates: Vec<PreviewCandidate>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportAction {
    Create,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecision {
    /// Index into the server-retained preview; service checks range and uniqueness.
    pub candidate_index: usize,
    pub action: ImportAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCommit {
    pub preview_id: PreviewId,
    pub decisions: Vec<ImportDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub created: Vec<PaperId>,
    pub skipped: usize,
}

/// Preview requests and commits are deliberately different types. Persisting raw
/// unreviewed candidates is not an ImportService entrypoint.
///
/// ```compile_fail
/// use cm_core::{ports::ImportService, import::ImportRequest};
/// fn bypass_preview(service: &mut dyn ImportService, input: ImportRequest) {
///     service.commit(input);
/// }
/// ```
const _: () = ();
