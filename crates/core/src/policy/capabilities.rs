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
    /// Read a caller-named local path (BibTeX/RIS/PDF selection, attachment jobs).
    LocalFileRead,
    /// Create a caller-named local file (bibliography export).
    LocalFileWrite,
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
}
impl Capability {
    pub const ALL: [Capability; 12] = [
        Self::LibraryRead,
        Self::LibraryWrite,
        Self::LibraryDelete,
        Self::DestinationRead,
        Self::CapturePreview,
        Self::LocalFileRead,
        Self::LocalFileWrite,
        Self::NetworkLookup,
        Self::UiSettings,
        Self::JobsRead,
        Self::JobsControl,
        Self::GrantAdmin,
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
    /// Browser native host: capture preview plus destination names/IDs. No paper reads,
    /// deletes, exports, commits or local paths. Capture commit is a later, preview-bound op.
    pub fn native_host() -> CapabilitySet {
        [CapturePreview, DestinationRead].into_iter().collect()
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
    /// Daemon version/limits/session diagnostics; still requires an authenticated live grant.
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// Every listed capability is needed. Empty = authenticated live grant only.
    All(&'static [Capability]),
    /// Any one of the listed capabilities is enough.
    Any(&'static [Capability]),
}
impl Requirement {
    pub fn satisfied_by(self, held: &CapabilitySet) -> bool {
        match self {
            Self::All(needed) => needed.iter().all(|c| held.contains(c)),
            Self::Any(options) => options.iter().any(|c| held.contains(c)),
        }
    }
}

impl Operation {
    pub fn requirement(self) -> Requirement {
        use Capability::*;
        use Operation::*;
        use Requirement::{All, Any};
        match self {
            PaperList | PaperGet | LibraryQuery | OrganizationForPaper | AttachmentList => {
                All(&[LibraryRead])
            }
            OrganizationList => Any(&[LibraryRead, DestinationRead]),
            PaperCreate | PaperUpdate | OrganizationWrite => All(&[LibraryWrite]),
            PaperTrash | PaperRestore | OrganizationDelete => All(&[LibraryDelete]),
            SettingsRead | SettingsWrite => All(&[UiSettings]),
            ImportParseFile | PdfInspect => All(&[LocalFileRead]),
            // Preview never persists. Its duplicate matches are redacted without LibraryRead.
            ImportPreview | ImportCancel => Any(&[LibraryWrite, CapturePreview]),
            ImportCommit => All(&[LibraryWrite]),
            PdfPreviewNew => All(&[LibraryWrite, LocalFileRead]),
            PdfAttach | PdfCommitNew | PdfRetryAttachment => All(&[LibraryWrite, LocalFileRead]),
            MetadataResolve => All(&[NetworkLookup]),
            Export => All(&[LibraryRead, LocalFileWrite]),
            JobSubmit | JobCancel | JobRetry => All(&[JobsControl]),
            JobRead => All(&[JobsRead]),
            GrantIssue | GrantList | GrantRevoke => All(&[GrantAdmin]),
            Status => All(&[]),
        }
    }
}
