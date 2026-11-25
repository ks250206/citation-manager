//! #25 capability vocabulary and the operation -> requirement table.
//!
//! A capability is held only by a daemon-owned grant. Client kind, requested scopes,
//! UID/PID or executable names are diagnostics/narrowing inputs, never a source of authority.
//! Feature issues add operations/capabilities additively; unknown operations stay denied.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    /// Paper/collection/tag/attachment metadata reads and library queries.
    LibraryRead,
    /// Create/update papers, organization membership, import/PDF commits.
    LibraryWrite,
    /// Trash/restore papers and delete collections/tags.
    LibraryDelete,
    /// Collection/tag names and IDs only, for choosing a capture destination.
    DestinationRead,
    /// Non-persisting import preview of caller-supplied drafts (browser capture).
    CapturePreview,
    /// Commit a capture preview this grant created: Create/Skip only (merging needs
    /// LibraryWrite), plus the optional preview-bound public PDF fetch job (#22).
    CaptureCommit,
    /// Read a caller-named local path (BibTeX/RIS/PDF selection, attachment jobs).
    LocalFileRead,
    /// Create a caller-named local file (bibliography export).
    LocalFileWrite,
    /// #20: atomically replace an existing caller-named `.pdf` the person confirmed in the
    /// desktop's own replace prompt. Only the desktop preset holds it; `LocalFileWrite` alone
    /// (e.g. an approved MCP write grant) can create new files but never replace one.
    LocalFileReplace,
    /// Network metadata lookups (DOI providers).
    NetworkLookup,
    /// Private UI preferences.
    UiSettings,
    /// Status of jobs owned by this grant (all jobs for GrantAdmin).
    JobsRead,
    /// Submit/cancel/retry jobs. Each job kind additionally names its own capabilities.
    JobsControl,
    /// Issue/list/revoke grants. Never implied by any other capability.
    GrantAdmin,
    // #26 fine-grained capabilities behind the API/MCP/CLI scopes (`policy::scopes`).
    // `LibraryRead` stays the #25 read bundle and still satisfies every read below.
    /// Bibliographic metadata, collections/tags, library queries, attachment lists and
    /// extraction status (`papers:read`). No page text, annotations or rendered pages.
    MetadataRead,
    /// Extracted PDF page text and rendered pages (`files:read`).
    ContentRead,
    /// Highlights, comments and notes (`annotations:read`).
    AnnotationsRead,
    /// Create/update/delete annotations (`annotations:write`). Required for every holder,
    /// the owner's grant included: `LibraryWrite` alone no longer writes annotations.
    AnnotationsWrite,
    /// Export a bibliography only into a directory the user allowed in the API settings
    /// (`exports:write`). Arbitrary paths stay `LocalFileWrite` (desktop only).
    ExportAllowedDir,
    /// Bulk delete/move beyond the per-grant burst limit (`policy::elevation`). Held
    /// permanently only by the owner's bootstrap grant; other grants get it only through a
    /// time-limited, user-approved elevation (`Grant::elevated_until_ms`).
    BulkElevated,
    /// Owner only: change the local API listener settings (enabled flag, address, port,
    /// export folders). Never delegated (`GrantCommand::Issue` and scopes refuse it).
    ApiAdmin,
    /// Owner only: approve or end a time-limited bulk elevation of another grant.
    ElevationAdmin,
}
impl Capability {
    pub const ALL: [Capability; 22] = [
        Self::LibraryRead,
        Self::LibraryWrite,
        Self::LibraryDelete,
        Self::DestinationRead,
        Self::CapturePreview,
        Self::CaptureCommit,
        Self::LocalFileRead,
        Self::LocalFileWrite,
        Self::LocalFileReplace,
        Self::NetworkLookup,
        Self::UiSettings,
        Self::JobsRead,
        Self::JobsControl,
        Self::GrantAdmin,
        Self::MetadataRead,
        Self::ContentRead,
        Self::AnnotationsRead,
        Self::AnnotationsWrite,
        Self::ExportAllowedDir,
        Self::BulkElevated,
        Self::ApiAdmin,
        Self::ElevationAdmin,
    ];
    /// Held only by the owner's bootstrap grant: no other grant can be issued or widened
    /// to hold them (#26 review: a delegate admin must not mint admins or elevations).
    pub const OWNER_ONLY: [Capability; 4] = [
        Self::GrantAdmin,
        Self::BulkElevated,
        Self::ApiAdmin,
        Self::ElevationAdmin,
    ];
}
pub type CapabilitySet = BTreeSet<Capability>;

/// Capability bundles used when the daemon (or a GrantAdmin holder acting on explicit user
/// approval) issues a grant. Presets are issuance templates, not something a client can claim.
pub mod presets {
    use super::{Capability::*, CapabilitySet};
    /// Interactive owner on this machine: the GUI's bootstrap grant.
    pub fn desktop() -> CapabilitySet {
        super::Capability::ALL.into_iter().collect()
    }
    /// Browser native host: capture preview, the preview-bound capture commit (#22) and
    /// destination names/IDs. No paper reads, deletes, exports, general import commits,
    /// merges, network lookups outside a capture, job control or local paths.
    pub fn native_host() -> CapabilitySet {
        [CapturePreview, CaptureCommit, DestinationRead]
            .into_iter()
            .collect()
    }
    /// MCP default: read-only library access. Writes need a separately approved grant.
    pub fn mcp_read_only() -> CapabilitySet {
        [LibraryRead, JobsRead].into_iter().collect()
    }
}

/// Every daemon command maps to exactly one operation. Unknown/new commands must add a
/// variant here; there is no wildcard or "authenticated is enough" fallback except `Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Operation {
    PaperList,
    PaperGet,
    PaperCreate,
    PaperUpdate,
    PaperTrash,
    PaperRestore,
    LibraryQuery,
    OrganizationList,
    OrganizationForPaper,
    OrganizationWrite,
    OrganizationDelete,
    SettingsRead,
    SettingsWrite,
    ImportParseFile,
    ImportPreview,
    ImportCommit,
    ImportCancel,
    MetadataResolve,
    /// #22: extract page metadata, complement via the fixed DOI providers, common preview.
    CaptureSubmit,
    /// #22: commit an own capture preview (Create/Skip; Merge additionally needs LibraryWrite).
    CaptureSave,
    CaptureCancel,
    /// #22: status / explicit retry of an own `capture.pdf_fetch` job.
    CapturePdfStatus,
    CapturePdfRetry,
    Export,
    PdfInspect,
    PdfAttach,
    PdfPreviewNew,
    PdfCommitNew,
    PdfRetryAttachment,
    AttachmentList,
    JobSubmit,
    JobRead,
    JobCancel,
    JobRetry,
    GrantIssue,
    GrantList,
    GrantRevoke,
    /// #18 full-text search over bibliographic records, annotations and PDF page text.
    FulltextSearch,
    /// #18 per-attachment extraction state (pending/complete/no_text/encrypted/...).
    FulltextStatus,
    /// #18 bounded read of extracted page text (page range, character and count limits).
    FulltextRead,
    /// #17 render one page of a stored attachment for viewing.
    PdfRender,
    /// #20 write an annotated copy of a stored PDF to a caller-named new file.
    PdfExport,
    /// #20 the same export replacing an existing file the person confirmed.
    PdfExportReplace,
    /// #19 list annotations / propose a position for an unresolved one.
    AnnotationRead,
    /// #19 create/update/delete/undo/confirm/carry-over/resolve annotations.
    AnnotationWrite,
    /// #26 more than the burst limit of trash/restore/delete operations in the window.
    BulkTrash,
    /// #26 more than the burst limit of collection/tag membership changes in the window.
    BulkMove,
    /// #26 API listener settings (enabled flag, loopback address, port, export dirs).
    ApiSettingsRead,
    ApiSettingsWrite,
    /// #26 change the scopes of an existing grant in place.
    GrantUpdate,
    /// #26 start or end a time-limited bulk elevation of another grant (owner only).
    GrantElevate,
    /// #26 bounded audit log (who/what/when/result; never content or secrets).
    AuditRead,
    /// Daemon version/limits/session diagnostics; still requires an authenticated live grant.
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// Every listed capability is needed. Empty = authenticated live grant only.
    All(&'static [Capability]),
    /// Any one of the listed capabilities is enough.
    Any(&'static [Capability]),
    /// Any one alternative, each needing all of its capabilities (#26: the #25 bundle or
    /// the fine-grained scope capabilities).
    AnyOf(&'static [&'static [Capability]]),
}
impl Requirement {
    pub fn satisfied_by(self, held: &CapabilitySet) -> bool {
        match self {
            Self::All(needed) => needed.iter().all(|c| held.contains(c)),
            Self::Any(options) => options.iter().any(|c| held.contains(c)),
            Self::AnyOf(alternatives) => alternatives
                .iter()
                .any(|needed| needed.iter().all(|c| held.contains(c))),
        }
    }
}

impl Operation {
    pub fn requirement(self) -> Requirement {
        use Capability::*;
        use Operation::*;
        use Requirement::{All, Any, AnyOf};
        match self {
            PaperList | PaperGet | LibraryQuery | OrganizationForPaper | AttachmentList
            | FulltextStatus => AnyOf(&[&[LibraryRead], &[MetadataRead]]),
            // Search spans metadata, annotation text and page text: a fine-grained holder
            // needs all three read scopes (otherwise snippets would leak page/annotation text).
            FulltextSearch => AnyOf(&[
                &[LibraryRead],
                &[MetadataRead, ContentRead, AnnotationsRead],
            ]),
            FulltextRead | PdfRender => AnyOf(&[&[LibraryRead], &[ContentRead]]),
            AnnotationRead => AnyOf(&[&[LibraryRead], &[AnnotationsRead]]),
            AnnotationWrite => All(&[AnnotationsWrite]),
            OrganizationList => AnyOf(&[&[LibraryRead], &[DestinationRead], &[MetadataRead]]),
            PaperCreate | PaperUpdate | OrganizationWrite => All(&[LibraryWrite]),
            PaperTrash | PaperRestore | OrganizationDelete => All(&[LibraryDelete]),
            BulkTrash => All(&[LibraryDelete, BulkElevated]),
            BulkMove => All(&[LibraryWrite, BulkElevated]),
            SettingsRead | SettingsWrite => All(&[UiSettings]),
            ImportParseFile | PdfInspect => All(&[LocalFileRead]),
            // Preview never persists. Its duplicate matches are redacted without LibraryRead.
            ImportPreview | ImportCancel => Any(&[LibraryWrite, CapturePreview]),
            ImportCommit => All(&[LibraryWrite]),
            PdfPreviewNew => All(&[LibraryWrite, LocalFileRead]),
            PdfAttach | PdfCommitNew | PdfRetryAttachment => All(&[LibraryWrite, LocalFileRead]),
            MetadataResolve => All(&[NetworkLookup]),
            // The provider lookup inside a capture is bound to the saved page's identifier and
            // the fixed #14 endpoints; it is not a general NetworkLookup.
            CaptureSubmit => All(&[CapturePreview]),
            CaptureSave | CapturePdfStatus | CapturePdfRetry => All(&[Capability::CaptureCommit]),
            CaptureCancel => Any(&[CapturePreview, Capability::CaptureCommit]),
            // ExportAllowedDir additionally restricts the destination (daemon, `policy::export`).
            Export => AnyOf(&[
                &[LibraryRead, LocalFileWrite],
                &[MetadataRead, LocalFileWrite],
                &[MetadataRead, ExportAllowedDir],
            ]),
            PdfExport => All(&[LibraryRead, LocalFileWrite]),
            PdfExportReplace => All(&[LibraryRead, LocalFileWrite, LocalFileReplace]),
            JobSubmit | JobCancel | JobRetry => All(&[JobsControl]),
            JobRead => All(&[JobsRead]),
            GrantIssue | GrantList | GrantRevoke | GrantUpdate => All(&[GrantAdmin]),
            GrantElevate => All(&[GrantAdmin, ElevationAdmin, BulkElevated]),
            ApiSettingsRead | AuditRead => All(&[GrantAdmin]),
            ApiSettingsWrite => All(&[GrantAdmin, ApiAdmin]),
            Status => All(&[]),
        }
    }
}
