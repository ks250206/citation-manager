//! PDF policy contracts only. No filesystem access or PDF parser is implemented here.
use super::*;
use crate::ServiceError;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfIntent {
    AttachExisting(PaperId),
    RegisterNew,
}
#[derive(Debug, Clone)]
pub struct FileMetadataCandidate {
    pub intent: PdfIntent,
    pub filename: String,
    pub explicit: Option<PaperDraft>,
    /// Supplied by a future parser/DOI adapter, validated through the shared normalizer.
    pub embedded: Option<PaperDraft>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfProposal {
    /// Embedded metadata never edits an existing paper through this path.
    AttachOnly(PaperId),
    Register {
        draft: Box<PaperDraft>,
        unresolved: Vec<String>,
    },
}
pub fn propose(input: FileMetadataCandidate) -> Result<PdfProposal, ServiceError> {
    if let PdfIntent::AttachExisting(id) = input.intent {
        if id.0.is_nil() {
            return Err(normalize::invalid("existing paper ID is required"));
        }
        return Ok(PdfProposal::AttachOnly(id));
    }
    let embedded = input
        .embedded
        .as_ref()
        .and_then(|d| normalize::normalize(d).ok());
    let mut draft = embedded.clone().unwrap_or_default();
    if let Some(explicit) = input.explicit {
        if !normalize::spaces(&explicit.title).is_empty() {
            draft.title = explicit.title.clone();
        }
        let mut bibliography =
            serde_json::to_value(&draft.bibliography).map_err(|_| ServiceError::Unavailable)?;
        for (field, value) in serde_json::to_value(&explicit.bibliography)
            .map_err(|_| ServiceError::Unavailable)?
            .as_object()
            .expect("bibliography object")
        {
            bibliography[field] = value.clone();
        }
        draft.bibliography =
            serde_json::from_value(bibliography).map_err(|_| ServiceError::Unavailable)?;
        // Explicit identifiers supersede embedded identifiers in the same namespace.
        for id in &explicit.external_ids {
            draft
                .external_ids
                .retain(|old| !old.namespace.eq_ignore_ascii_case(&id.namespace));
        }
        // An explicit bibliography DOI also supersedes an embedded external DOI.
        if explicit.bibliography.doi.is_some() {
            draft
                .external_ids
                .retain(|id| !id.namespace.eq_ignore_ascii_case("doi"));
        }
        if explicit
            .external_ids
            .iter()
            .any(|id| id.namespace.eq_ignore_ascii_case("doi"))
            && explicit.bibliography.doi.is_none()
        {
            draft.bibliography.doi = None;
        }
        draft.external_ids.extend(explicit.external_ids);
        draft.source = explicit.source;
        draft.diagnostics.extend(explicit.diagnostics);
    }
    let mut unresolved = Vec::new();
    if draft.title.trim().is_empty() {
        if input.filename.contains(['/', '\\', '\0'])
            || !input.filename.to_ascii_lowercase().ends_with(".pdf")
        {
            return Err(normalize::invalid("expected PDF basename"));
        }
        draft.title = input.filename[..input.filename.len() - 4].to_owned();
        unresolved
            .push("filename is a provisional title; bibliography unresolved and editable".into());
    }
    if embedded.is_some() {
        unresolved.push("embedded metadata requires review".into());
    }
    let draft = normalize::normalize(&draft)?;
    if draft.bibliography.authors.is_empty() {
        unresolved.push("contributors unresolved and editable".into());
    }
    if draft.bibliography.year.is_none() {
        unresolved.push("publication date unresolved and editable".into());
    }
    Ok(PdfProposal::Register {
        draft: Box::new(draft),
        unresolved,
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderScope {
    SelectedFiles,
    SelectedDirectory { recursive: bool },
}
#[derive(Debug, Clone)]
pub struct FolderPolicy {
    pub scope: FolderScope,
    pub max_file_bytes: u64,
}
impl Default for FolderPolicy {
    fn default() -> Self {
        Self {
            scope: FolderScope::SelectedFiles,
            max_file_bytes: 256 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone)]
pub struct SelectedFile {
    /// Future adapter must verify containment in the user-selected scope and all ancestors.
    pub in_selected_scope: bool,
    pub symlink_or_unsafe_ancestor: bool,
    pub regular: bool,
    pub filename: String,
    pub byte_size: u64,
    pub sha256: Option<[u8; 32]>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDisposition {
    Ready,
    OutsideScope,
    UnsafeLink,
    NonPdf,
    Oversized,
    DuplicateHash,
    Failed(String),
    Cancelled,
    Imported(AttachmentIdRef),
}
/// An attachment ID returned by a future #9 adapter; no file persistence in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentIdRef(pub cm_domain::AttachmentId);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProgress {
    pub index: usize,
    pub disposition: FileDisposition,
    pub retryable: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderReport {
    pub total: usize,
    pub completed: usize,
    pub cancelled: bool,
    pub files: Vec<FileProgress>,
}
/// Deterministic planning from mock/adapter-supplied inventory. No directory traversal.
/// Hash duplicates are skipped within this selected batch; the #9 store independently
/// reuses verified blobs across papers after an explicit attachment decision.
pub fn plan_folder(
    files: &[SelectedFile],
    policy: &FolderPolicy,
    cancel_before: Option<usize>,
) -> FolderReport {
    let mut hashes = std::collections::HashSet::new();
    let mut report = FolderReport {
        total: files.len(),
        completed: 0,
        cancelled: false,
        files: vec![],
    };
    for (index, file) in files.iter().enumerate() {
        let disposition = if cancel_before.is_some_and(|n| index >= n) {
            report.cancelled = true;
            FileDisposition::Cancelled
        } else if !file.in_selected_scope {
            FileDisposition::OutsideScope
        } else if file.symlink_or_unsafe_ancestor || !file.regular {
            FileDisposition::UnsafeLink
        } else if !file.filename.to_ascii_lowercase().ends_with(".pdf") {
            FileDisposition::NonPdf
        } else if file.byte_size > policy.max_file_bytes {
            FileDisposition::Oversized
        } else if file.sha256.is_some_and(|h| !hashes.insert(h)) {
            FileDisposition::DuplicateHash
        } else {
            FileDisposition::Ready
        };
        let retryable = matches!(
            disposition,
            FileDisposition::Ready | FileDisposition::Cancelled | FileDisposition::Failed(_)
        );
        if disposition != FileDisposition::Cancelled {
            report.completed += 1;
        }
        report.files.push(FileProgress {
            index,
            disposition,
            retryable,
        });
    }
    report
}
