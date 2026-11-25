//! Local IPC envelope, handshake and framing (#25).
//!
//! Wire: each frame is a 4-byte big-endian length followed by that many bytes of body:
//! UTF-8 JSON, or (version 2, #85) JSON plus raw binary payloads for page bitmaps (see
//! `frame`). The first client frame must be `ClientFrame::Hello`; nothing else is accepted
//! (or read past the small handshake limit) before the daemon has authenticated the
//! credential. Commands are grouped by feature module and `#[non_exhaustive]`: later issues
//! add variants and bump `PROTOCOL_VERSION` only for incompatible changes.
pub mod b64;
pub mod blob;
pub mod frame;
mod pdf;
pub use frame::{FrameError, read_frame, write_frame};
pub use pdf::PdfCommand;

use crate::{
    attachments::PdfReply,
    capture::{CaptureCommand, CaptureReply},
    exchange::{ExportRequest, Exported, ImportReply, ImportRequestUi, MetadataResolution},
    jobs::{JobCommand, JobReply},
    library::{LibraryReply, LibraryRequest, ServiceError},
    library_query::{LibraryPage, LibraryQuery},
    organization::{Organization, OrganizationRequest},
    search::{FulltextReply, FulltextRequest, SearchReply, SearchRequest},
    settings::{Settings, SettingsLoaded},
};
pub use cm_core::policy::{Capability, CapabilitySet, Denial, GrantId, Operation, Requirement};
use serde::{Deserialize, Serialize};

/// 2 (#85): page bitmaps travel as binary frame payloads instead of base64 JSON text.
/// A version-1 peer is answered `VersionMismatch` (handshake frames stay plain JSON).
pub const PROTOCOL_VERSION: u32 = 2;
/// Pre-authentication frames are tiny; a larger first frame closes the connection.
pub const MAX_HANDSHAKE_FRAME: u32 = 64 * 1024;
pub const MAX_REQUEST_FRAME: u32 = 8 * 1024 * 1024;
pub const MAX_RESPONSE_FRAME: u32 = 64 * 1024 * 1024;
/// Requests a single connection may have outstanding; more are answered `Busy`.
pub const MAX_IN_FLIGHT: usize = 64;

/// Opaque bearer secret. Debug output is redacted; never log or persist it in clear.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Credential(pub String);
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol_version: u32,
    /// Diagnostic only ("desktop", "cli", "mcp", "native-host", ...). Never authority.
    pub client_kind: String,
    /// Optional narrowing of the grant for this session. Never widens it.
    pub requested: Option<CapabilitySet>,
    /// Library the client expects; a daemon serving another library rejects the session.
    pub library: Option<std::path::PathBuf>,
    pub credential: Credential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol_version: u32,
    pub daemon_version: String,
    pub session: u64,
    pub grant: GrantId,
    /// Effective capabilities at handshake time (re-checked on every request).
    pub capabilities: CapabilitySet,
    pub max_request_frame: u32,
    pub max_in_flight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandshakeRejection {
    VersionMismatch { supported: u32 },
    Unauthenticated,
    PeerRejected,
    WrongLibrary,
    Malformed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientFrame {
    Hello(Hello),
    Request {
        id: u64,
        command: Command,
    },
    /// Best-effort cancellation of an in-flight request on this connection.
    Cancel {
        id: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerFrame {
    Welcome(Welcome),
    Rejected(HandshakeRejection),
    /// PDF batch progress for request `id`; zero or more before its `Response`.
    PdfProgress {
        id: u64,
        report: crate::attachments::PdfReport,
    },
    Response {
        id: u64,
        result: Result<Reply, WireError>,
    },
}

/// One variant per feature module. Each maps to exactly one `Operation` for authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Command {
    Status,
    Library(LibraryRequest),
    Query(LibraryQuery),
    Organization(OrganizationRequest),
    Lifecycle {
        paper: crate::domain::PaperId,
        deleted: bool,
    },
    LoadSettings,
    SaveSettings(Settings),
    Import(ImportRequestUi),
    Resolve {
        doi: String,
    },
    Export(ExportRequest),
    Pdf(PdfCommand),
    Jobs(JobCommand),
    Grants(GrantCommand),
    /// #18 full-text search (bibliographic records, annotations, PDF page text).
    Search(Box<SearchRequest>),
    /// #18 extraction status and bounded page text reads.
    Fulltext(FulltextRequest),
    /// #19 highlights, comments and notes.
    Annotations(crate::annotations::AnnotationRequest),
    /// #22 browser/MCP capture (preview, preview-bound commit, PDF job status/retry).
    Capture(CaptureCommand),
    /// #26 tokens, MCP profiles, elevation, API settings and audit (GrantAdmin only).
    Api(crate::api::ApiCommand),
}
impl Command {
    pub fn operation(&self) -> Operation {
        use Operation as O;
        match self {
            Self::Status => O::Status,
            Self::Library(r) => match r {
                LibraryRequest::List(_) => O::PaperList,
                LibraryRequest::Get(_) => O::PaperGet,
                LibraryRequest::Create(_) => O::PaperCreate,
                LibraryRequest::Update(_) | LibraryRequest::Patch(..) => O::PaperUpdate,
            },
            Self::Query(_) => O::LibraryQuery,
            Self::Organization(r) => match r {
                OrganizationRequest::List => O::OrganizationList,
                OrganizationRequest::ForPaper(_) => O::OrganizationForPaper,
                OrganizationRequest::DeleteCollection(_) | OrganizationRequest::DeleteTag(_) => {
                    O::OrganizationDelete
                }
                OrganizationRequest::CreateCollection(_)
                | OrganizationRequest::UpdateCollection(..)
                | OrganizationRequest::CollectionMember(..)
                | OrganizationRequest::MoveCollectionMember(..)
                | OrganizationRequest::CreateTag(_)
                | OrganizationRequest::UpdateTag(..)
                | OrganizationRequest::TagMember(..) => O::OrganizationWrite,
            },
            Self::Lifecycle { deleted: true, .. } => O::PaperTrash,
            Self::Lifecycle { deleted: false, .. } => O::PaperRestore,
            Self::LoadSettings => O::SettingsRead,
            Self::SaveSettings(_) => O::SettingsWrite,
            Self::Import(r) => match r {
                ImportRequestUi::Parse { .. } => O::ImportParseFile,
                // Previews of an already-parsed local file need the file read capability too;
                // the daemon additionally requires the retained parse in the same session.
                ImportRequestUi::PreviewFile { .. } => O::ImportParseFile,
                ImportRequestUi::Preview(_) => O::ImportPreview,
                ImportRequestUi::Commit(_) => O::ImportCommit,
                ImportRequestUi::Cancel(_) => O::ImportCancel,
            },
            Self::Resolve { .. } => O::MetadataResolve,
            Self::Export(_) => O::Export,
            Self::Pdf(c) => c.operation(),
            Self::Annotations(r) => r.operation(),
            Self::Jobs(c) => match c {
                JobCommand::Submit(_) => O::JobSubmit,
                JobCommand::Get(_) | JobCommand::List { .. } => O::JobRead,
                JobCommand::Cancel(_) => O::JobCancel,
                JobCommand::Retry(_) => O::JobRetry,
            },
            Self::Grants(c) => match c {
                GrantCommand::Issue { .. } => O::GrantIssue,
                GrantCommand::List => O::GrantList,
                GrantCommand::Revoke(_) => O::GrantRevoke,
            },
            Self::Api(c) => c.operation(),
            Self::Search(_) => O::FulltextSearch,
            Self::Fulltext(r) => match r {
                FulltextRequest::Status(_) | FulltextRequest::AttachmentStatus(_) => {
                    O::FulltextStatus
                }
                FulltextRequest::Pages(_) | FulltextRequest::PaperPages(_) => O::FulltextRead,
            },
            Self::Capture(c) => c.operation(),
        }
    }
    /// Most OS secret-store helper runs the daemon may wait on before answering (see
    /// `ApiCommand::secret_store_steps`); 0 for commands that never touch the store.
    pub fn secret_store_steps(&self) -> u32 {
        match self {
            Self::Api(c) => c.secret_store_steps(),
            // Revoking deletes the grant's stored token copy, as `ApiCommand::Revoke` does.
            Self::Grants(GrantCommand::Revoke(_)) => 1,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Reply {
    Status(DaemonStatus),
    Library(LibraryReply),
    Page(LibraryPage),
    Organization(Organization),
    Paper(Box<crate::domain::Paper>),
    Settings(SettingsLoaded),
    Unit,
    Import(ImportReply),
    Resolution(Box<MetadataResolution>),
    Exported(Exported),
    Pdf(PdfReply),
    Jobs(JobReply),
    Grants(GrantReply),
    Search(SearchReply),
    Fulltext(FulltextReply),
    Annotations(crate::annotations::AnnotationReply),
    Capture(CaptureReply),
    Api(crate::api::ApiReply),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireError {
    Service(ServiceError),
    Denied(Denial),
    /// Too many in-flight requests on this connection.
    Busy,
    /// Request or response frame over the negotiated limit.
    TooLarge,
    /// The request was cancelled before a result was produced.
    Cancelled,
    /// Daemon is shutting down or a worker is unavailable.
    Unavailable,
}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Service(e) => e.fmt(f),
            Self::Denied(d) => d.fmt(f),
            Self::Busy => f.write_str("too many requests in flight on this connection"),
            Self::TooLarge => f.write_str("message exceeds the size limit"),
            Self::Cancelled => f.write_str("request cancelled"),
            Self::Unavailable => f.write_str("the library service is unavailable"),
        }
    }
}
impl std::error::Error for WireError {}
#[cfg(test)]
#[test]
fn wire_errors_display_for_users() {
    // #22 review P3: capture `pdf_error` uses Display, not Debug.
    let denied = WireError::Denied(Denial::Forbidden(cm_core::policy::Operation::JobSubmit));
    assert!(!denied.to_string().contains("Denied("));
    assert_eq!(
        WireError::Service(ServiceError::Unavailable).to_string(),
        ServiceError::Unavailable.to_string()
    );
}
impl From<ServiceError> for WireError {
    fn from(e: ServiceError) -> Self {
        Self::Service(e)
    }
}
impl From<WireError> for ServiceError {
    /// The `LibraryClient` view contract only knows ServiceError: authorization failures
    /// become a visible, non-retryable InvalidInput message; transport issues Unavailable.
    fn from(e: WireError) -> Self {
        match e {
            WireError::Service(e) => e,
            WireError::Denied(d) => ServiceError::InvalidInput(format!("not permitted: {d}")),
            WireError::TooLarge => ServiceError::InvalidInput("request too large".into()),
            WireError::Cancelled => ServiceError::InvalidInput("cancelled".into()),
            WireError::Busy | WireError::Unavailable => ServiceError::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub daemon_version: String,
    pub protocol_version: u32,
    pub pid: u32,
    pub session: u64,
    pub grant: GrantId,
    pub capabilities: CapabilitySet,
    pub connections: u32,
    pub active_jobs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum GrantCommand {
    /// Mint a new credential for a subset of the caller's capabilities. Must be driven by
    /// an explicit user approval in a GrantAdmin client (GUI; settings UI is #26).
    Issue {
        label: String,
        capabilities: CapabilitySet,
        expires_at_ms: Option<i64>,
    },
    List,
    Revoke(GrantId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantInfo {
    pub id: GrantId,
    pub label: String,
    pub capabilities: CapabilitySet,
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantReply {
    /// The only time the secret is returned. The daemon stores its SHA-256 only.
    Issued {
        grant: GrantInfo,
        credential: Credential,
    },
    Grants(Vec<GrantInfo>),
    Revoked(GrantId),
}
