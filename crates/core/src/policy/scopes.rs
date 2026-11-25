//! #26 token scopes and the REST endpoint / MCP tool / CLI command -> scope table.
//!
//! A scope is a user-facing name for a fixed set of daemon capabilities. Tokens for API, MCP
//! and CLI clients are daemon grants whose capabilities come only from scopes, so REST (#27),
//! the CLI (#28) and MCP (#29) share one authorization decision: each request maps to an
//! `Operation` and the daemon runs `policy::authorize` against the grant's *current*
//! capabilities. `ENDPOINTS` is data for those adapters and for the settings UI; it is never
//! consulted as authority (the daemon is), and tests prove it agrees with the decision.
use super::capabilities::{Capability, CapabilitySet, Operation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Scope {
    #[serde(rename = "papers:read")]
    PapersRead,
    #[serde(rename = "papers:write")]
    PapersWrite,
    #[serde(rename = "papers:delete")]
    PapersDelete,
    #[serde(rename = "files:read")]
    FilesRead,
    #[serde(rename = "annotations:read")]
    AnnotationsRead,
    #[serde(rename = "annotations:write")]
    AnnotationsWrite,
    #[serde(rename = "exports:write")]
    ExportsWrite,
    #[serde(rename = "jobs:read")]
    JobsRead,
    #[serde(rename = "jobs:write")]
    JobsWrite,
    /// Not issuable: held only during a time-limited elevation (`policy::elevation`).
    #[serde(rename = "bulk:elevated")]
    BulkElevated,
}
pub type ScopeSet = BTreeSet<Scope>;

impl Scope {
    pub const ALL: [Scope; 10] = [
        Self::PapersRead,
        Self::PapersWrite,
        Self::PapersDelete,
        Self::FilesRead,
        Self::AnnotationsRead,
        Self::AnnotationsWrite,
        Self::ExportsWrite,
        Self::JobsRead,
        Self::JobsWrite,
        Self::BulkElevated,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::PapersRead => "papers:read",
            Self::PapersWrite => "papers:write",
            Self::PapersDelete => "papers:delete",
            Self::FilesRead => "files:read",
            Self::AnnotationsRead => "annotations:read",
            Self::AnnotationsWrite => "annotations:write",
            Self::ExportsWrite => "exports:write",
            Self::JobsRead => "jobs:read",
            Self::JobsWrite => "jobs:write",
            Self::BulkElevated => "bulk:elevated",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
    /// The daemon capabilities a token with this scope holds. Never `GrantAdmin`,
    /// `UiSettings`, `LocalFileRead`/`LocalFileWrite`, `CapturePreview`, `DestinationRead`
    /// or the `LibraryRead` bundle: those stay with the owner and the native host.
    pub fn capabilities(self) -> &'static [Capability] {
        use Capability as C;
        match self {
            Self::PapersRead => &[C::MetadataRead],
            // DOI resolution is part of adding papers (`POST /papers/resolve-doi`).
            Self::PapersWrite => &[C::LibraryWrite, C::NetworkLookup],
            Self::PapersDelete => &[C::LibraryDelete],
            Self::FilesRead => &[C::ContentRead],
            Self::AnnotationsRead => &[C::AnnotationsRead],
            Self::AnnotationsWrite => &[C::AnnotationsWrite],
            Self::ExportsWrite => &[C::ExportAllowedDir],
            Self::JobsRead => &[C::JobsRead],
            Self::JobsWrite => &[C::JobsControl],
            Self::BulkElevated => &[C::BulkElevated],
        }
    }
    /// Whether a token can be issued with this scope (elevation is a separate path).
    pub fn issuable(self) -> bool {
        self != Self::BulkElevated
    }
    /// Scopes that change the library or start work. They shorten the maximum lifetime.
    pub fn is_write(self) -> bool {
        !matches!(
            self,
            Self::PapersRead | Self::FilesRead | Self::AnnotationsRead | Self::JobsRead
        )
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::PapersRead => "Bibliographic metadata, collections, tags, queries",
            Self::PapersWrite => "Add/edit papers, collections, tags, imports, DOI lookup",
            Self::PapersDelete => "Move papers to Trash/restore, delete collections/tags",
            Self::FilesRead => "Extracted PDF text and rendered pages",
            Self::AnnotationsRead => "Highlights, comments and notes",
            Self::AnnotationsWrite => "Create, edit and delete annotations",
            Self::ExportsWrite => "Export BibTeX/RIS into allowed export folders only",
            Self::JobsRead => "Status of this token's background jobs",
            Self::JobsWrite => "Submit, cancel and retry this token's jobs",
            Self::BulkElevated => "Bulk delete/move (time-limited elevation only)",
        }
    }
}
impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Capabilities of a token holding exactly `scopes`.
pub fn capabilities(scopes: &ScopeSet) -> CapabilitySet {
    scopes
        .iter()
        .flat_map(|s| s.capabilities().iter().copied())
        .collect()
}

/// Display form of a capability set: every scope whose capabilities are all held (the
/// #25 `LibraryRead` bundle counts as the three read scopes). Diagnostic only.
pub fn scopes_of(held: &CapabilitySet) -> ScopeSet {
    let mut expanded = held.clone();
    if held.contains(&Capability::LibraryRead) {
        expanded.extend([
            Capability::MetadataRead,
            Capability::ContentRead,
            Capability::AnnotationsRead,
        ]);
    }
    Scope::ALL
        .into_iter()
        .filter(|s| s.capabilities().iter().all(|c| expanded.contains(c)))
        .collect()
}

/// Who a grant was issued for. Diagnostic metadata chosen at issuance by the owner; a
/// client's self-declared `Hello::client_kind` is never compared against it for authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ClientKind {
    /// Grants issued before #26 through `GrantCommand::Issue`.
    Unspecified,
    Desktop,
    /// Local REST API client (#27).
    Api,
    /// MCP connection profile (#29). Read-only by default.
    Mcp,
    /// `papers` CLI (#28).
    Cli,
    /// Browser native messaging host (#23); paired by its own flow, never by API settings.
    NativeHost,
}
impl ClientKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::Desktop => "desktop",
            Self::Api => "api",
            Self::Mcp => "mcp",
            Self::Cli => "cli",
            Self::NativeHost => "native-host",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Unspecified,
            Self::Desktop,
            Self::Api,
            Self::Mcp,
            Self::Cli,
            Self::NativeHost,
        ]
        .into_iter()
        .find(|k| k.name() == name)
    }
    /// Kinds the API settings screen and the CLI bootstrap may issue scoped tokens for.
    pub fn scoped_token(self) -> bool {
        matches!(self, Self::Api | Self::Mcp | Self::Cli)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Rest,
    Mcp,
    Cli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No credential and no library data (`GET /health`).
    Public,
    /// Every listed scope is required (empty = any live token).
    Scoped(&'static [Scope]),
    /// Needs a capability no scope grants (local file reads, arbitrary export paths): only
    /// the owner's credential, e.g. the CLI using the desktop bootstrap credential.
    OwnerOnly,
}

/// One REST endpoint, MCP tool or CLI command and the daemon operations it performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub surface: Surface,
    pub name: &'static str,
    pub operations: &'static [Operation],
    pub access: Access,
}
impl Endpoint {
    /// Convenience pre-check for adapters and the UI. Authority is still the daemon.
    pub fn allowed(&self, scopes: &ScopeSet) -> bool {
        match self.access {
            Access::Public => true,
            Access::Scoped(required) => required.iter().all(|s| scopes.contains(s)),
            Access::OwnerOnly => false,
        }
    }
}

pub fn endpoint(surface: Surface, name: &str) -> Option<&'static Endpoint> {
    ENDPOINTS
        .iter()
        .find(|e| e.surface == surface && e.name == name)
}

macro_rules! endpoints {
    ($($surface:ident $name:literal [$($op:ident),*] $access:expr;)*) => {
        &[$(Endpoint {
            surface: Surface::$surface,
            name: $name,
            operations: &[$(Operation::$op),*],
            access: $access,
        }),*]
    };
}
use Access::{OwnerOnly, Public, Scoped};
use Scope::*;

/// The mapping table (also in docs/ADR/api-tokens-grants.md). Adapters add rows here; the
/// core tests check every row against `Operation::requirement` for every scope combination.
pub const ENDPOINTS: &[Endpoint] = endpoints! {
    Rest "GET /health" [] Public;
    Rest "GET /papers" [PaperList, LibraryQuery] Scoped(&[PapersRead]);
    Rest "GET /papers/{id}" [PaperGet, OrganizationForPaper, AttachmentList] Scoped(&[PapersRead]);
    Rest "POST /papers" [PaperCreate] Scoped(&[PapersWrite]);
    Rest "PATCH /papers/{id}" [PaperUpdate] Scoped(&[PapersWrite]);
    Rest "DELETE /papers/{id}" [PaperTrash] Scoped(&[PapersDelete]);
    Rest "POST /papers/{id}/restore" [PaperRestore] Scoped(&[PapersDelete]);
    Rest "POST /papers/bulk-delete" [BulkTrash] Scoped(&[PapersDelete, BulkElevated]);
    Rest "POST /papers/resolve-doi" [MetadataResolve] Scoped(&[PapersWrite]);
    Rest "GET /search" [FulltextSearch] Scoped(&[PapersRead, FilesRead, AnnotationsRead]);
    Rest "GET /papers/{id}/fulltext/status" [FulltextStatus] Scoped(&[PapersRead]);
    Rest "GET /papers/{id}/fulltext" [FulltextRead] Scoped(&[FilesRead]);
    Rest "GET /attachments/{id}/pages/{page}" [PdfRender] Scoped(&[FilesRead]);
    Rest "GET /papers/{id}/annotations" [AnnotationRead] Scoped(&[AnnotationsRead]);
    Rest "POST /attachments/{id}/annotations" [AnnotationWrite] Scoped(&[AnnotationsWrite]);
    Rest "PATCH /annotations/{id}" [AnnotationWrite] Scoped(&[AnnotationsWrite]);
    Rest "DELETE /annotations/{id}" [AnnotationWrite] Scoped(&[AnnotationsWrite]);
    Rest "GET /collections" [OrganizationList] Scoped(&[PapersRead]);
    Rest "POST /collections" [OrganizationWrite] Scoped(&[PapersWrite]);
    Rest "PATCH /collections/{id}" [OrganizationWrite] Scoped(&[PapersWrite]);
    Rest "DELETE /collections/{id}" [OrganizationDelete] Scoped(&[PapersDelete]);
    Rest "POST /collections/{id}/papers" [OrganizationWrite] Scoped(&[PapersWrite]);
    Rest "POST /collections/{id}/papers/bulk-move" [BulkMove] Scoped(&[PapersWrite, BulkElevated]);
    Rest "GET /tags" [OrganizationList] Scoped(&[PapersRead]);
    Rest "POST /tags" [OrganizationWrite] Scoped(&[PapersWrite]);
    Rest "DELETE /tags/{id}" [OrganizationDelete] Scoped(&[PapersDelete]);
    Rest "POST /imports" [ImportPreview, ImportCommit, ImportCancel] Scoped(&[PapersWrite]);
    Rest "POST /exports" [Export] Scoped(&[PapersRead, ExportsWrite]);
    Rest "GET /jobs/{id}" [JobRead] Scoped(&[JobsRead]);
    Rest "POST /jobs/{id}/cancel" [JobCancel] Scoped(&[JobsWrite]);
    Mcp "search_papers" [LibraryQuery] Scoped(&[PapersRead]);
    Mcp "search_fulltext" [FulltextSearch] Scoped(&[PapersRead, FilesRead, AnnotationsRead]);
    Mcp "get_paper" [PaperGet, OrganizationForPaper, AttachmentList] Scoped(&[PapersRead]);
    Mcp "get_paper_fulltext" [FulltextRead] Scoped(&[FilesRead]);
    Mcp "get_paper_annotations" [AnnotationRead] Scoped(&[AnnotationsRead]);
    Mcp "add_paper_by_doi" [MetadataResolve, ImportPreview, ImportCommit] Scoped(&[PapersWrite]);
    Mcp "add_paper_from_url" [MetadataResolve, ImportPreview, ImportCommit] Scoped(&[PapersWrite]);
    Mcp "update_paper" [PaperUpdate] Scoped(&[PapersWrite]);
    Mcp "create_collection" [OrganizationWrite] Scoped(&[PapersWrite]);
    Mcp "move_paper_to_collection" [OrganizationWrite] Scoped(&[PapersWrite]);
    Mcp "add_tag_to_paper" [OrganizationWrite] Scoped(&[PapersWrite]);
    Mcp "add_annotation" [AnnotationWrite] Scoped(&[AnnotationsWrite]);
    Mcp "export_citations" [Export] Scoped(&[PapersRead, ExportsWrite]);
    Cli "papers add doi" [MetadataResolve, ImportPreview, ImportCommit] Scoped(&[PapersWrite]);
    Cli "papers add file" [PdfInspect, PdfPreviewNew, PdfCommitNew] OwnerOnly;
    Cli "papers import" [ImportParseFile, ImportPreview, ImportCommit] OwnerOnly;
    Cli "papers search" [FulltextSearch] Scoped(&[PapersRead, FilesRead, AnnotationsRead]);
    Cli "papers list" [LibraryQuery] Scoped(&[PapersRead]);
    Cli "papers show" [PaperGet, OrganizationForPaper, AttachmentList] Scoped(&[PapersRead]);
    Cli "papers fulltext" [FulltextRead] Scoped(&[FilesRead]);
    Cli "papers annotations" [AnnotationRead] Scoped(&[AnnotationsRead]);
    Cli "papers export" [Export] Scoped(&[PapersRead, ExportsWrite]);
    Cli "papers server status" [Status] Scoped(&[]);
};

/// MCP connection profiles (#29). Read-only is the default; write is a separate, explicit
/// user decision and never implied by connecting or by `source=ai`.
pub mod profiles {
    use super::{Scope, ScopeSet};
    pub fn mcp_read_only() -> ScopeSet {
        [
            Scope::PapersRead,
            Scope::FilesRead,
            Scope::AnnotationsRead,
            Scope::JobsRead,
        ]
        .into_iter()
        .collect()
    }
    /// What "Allow writes" adds. Delete and export stay separate choices; bulk operations
    /// always need a time-limited elevation on top.
    pub fn mcp_write() -> ScopeSet {
        let mut scopes = mcp_read_only();
        scopes.extend([Scope::PapersWrite, Scope::AnnotationsWrite]);
        scopes
    }
    pub const MCP_WRITE_WARNING: &str = "An AI client with write access can add and edit \
        papers, collections, tags and annotations without asking you each time. Only allow it \
        for clients you trust; you can make the profile read-only or revoke it at any time.";
}

pub const MINUTE_MS: i64 = 60_000;
pub const DAY_MS: i64 = 24 * 60 * MINUTE_MS;

/// Token lifetime policy. Scoped tokens always expire; "never" is reserved for the owner's
/// bootstrap grant. Write-capable tokens live shorter than read-only ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetime {
    pub default_ms: i64,
    pub max_ms: i64,
}
pub fn lifetime(scopes: &ScopeSet) -> Lifetime {
    if scopes.iter().any(|s| s.is_write()) {
        Lifetime {
            default_ms: 30 * DAY_MS,
            max_ms: 90 * DAY_MS,
        }
    } else {
        Lifetime {
            default_ms: 90 * DAY_MS,
            max_ms: 365 * DAY_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenPolicyError {
    /// Only API, MCP and CLI tokens are issued from scopes.
    KindNotIssuable,
    NoScopes,
    /// `bulk:elevated` is granted only by a time-limited elevation.
    ScopeNotIssuable,
    ExpiryInPast,
    ExpiryTooLong,
}
impl std::fmt::Display for TokenPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::KindNotIssuable => "only API, MCP and CLI tokens can be issued here",
            Self::NoScopes => "choose at least one scope",
            Self::ScopeNotIssuable => "bulk:elevated is only granted by a time-limited elevation",
            Self::ExpiryInPast => "expiry must be in the future",
            Self::ExpiryTooLong => "expiry exceeds the maximum lifetime for these scopes",
        })
    }
}

/// Validates a scoped token request and resolves its expiry (default when `None`).
pub fn validate_token(
    kind: ClientKind,
    scopes: &ScopeSet,
    expires_at_ms: Option<i64>,
    now_ms: i64,
) -> Result<i64, TokenPolicyError> {
    if !kind.scoped_token() {
        return Err(TokenPolicyError::KindNotIssuable);
    }
    if scopes.is_empty() {
        return Err(TokenPolicyError::NoScopes);
    }
    if scopes.iter().any(|s| !s.issuable()) {
        return Err(TokenPolicyError::ScopeNotIssuable);
    }
    let life = lifetime(scopes);
    let at = expires_at_ms.unwrap_or(now_ms.saturating_add(life.default_ms));
    if at <= now_ms {
        return Err(TokenPolicyError::ExpiryInPast);
    }
    if at > now_ms.saturating_add(life.max_ms) {
        return Err(TokenPolicyError::ExpiryTooLong);
    }
    Ok(at)
}

/// A scope change of an existing token keeps its expiry unless the new scopes allow a
/// shorter maximum lifetime; then the expiry is pulled in (never extended).
pub fn expiry_after_scope_change(scopes: &ScopeSet, current: Option<i64>, now_ms: i64) -> i64 {
    let max = now_ms.saturating_add(lifetime(scopes).max_ms);
    current.map_or(max, |at| at.min(max))
}

#[cfg(test)]
mod tests {
    use super::super::{
        authorizer::{Grant, GrantId, authorize},
        capabilities::presets,
    };
    use super::*;

    fn grant(capabilities: CapabilitySet) -> Grant {
        Grant {
            id: GrantId(uuid::Uuid::nil()),
            label: "t".into(),
            capabilities,
            expires_at_ms: None,
            revoked: false,
            elevated_until_ms: None,
        }
    }
    fn subsets() -> impl Iterator<Item = ScopeSet> {
        (0u32..1 << Scope::ALL.len()).map(|mask| {
            Scope::ALL
                .into_iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, s)| s)
                .collect()
        })
    }

    #[test]
    fn names_round_trip_and_serde_uses_them() {
        for scope in Scope::ALL {
            assert_eq!(Scope::parse(scope.name()), Some(scope));
            assert_eq!(
                serde_json::to_string(&scope).unwrap(),
                format!("\"{}\"", scope.name())
            );
        }
        assert_eq!(Scope::parse("papers:admin"), None);
        assert_eq!(
            ClientKind::parse("native-host"),
            Some(ClientKind::NativeHost)
        );
    }

    /// Every scope combination against every table row: the daemon decision (via the
    /// shared authorizer) agrees exactly with the documented scope requirement.
    #[test]
    fn every_scope_combination_matches_the_endpoint_table() {
        for scopes in subsets() {
            let g = grant(capabilities(&scopes));
            for endpoint in ENDPOINTS {
                let decided = endpoint
                    .operations
                    .iter()
                    .all(|op| authorize(Some(&g), None, *op, 0).is_ok());
                match endpoint.access {
                    Access::Public => assert!(endpoint.operations.is_empty()),
                    Access::Scoped(_) => assert_eq!(
                        decided,
                        endpoint.allowed(&scopes),
                        "{} with {scopes:?}",
                        endpoint.name
                    ),
                    Access::OwnerOnly => assert!(!decided, "{}", endpoint.name),
                }
            }
        }
    }

    #[test]
    fn every_operation_requirement_is_minimal_per_scope() {
        // Each listed scope is needed: dropping any one of them denies the endpoint.
        for endpoint in ENDPOINTS {
            if let Access::Scoped(required) = endpoint.access {
                for drop in required {
                    let scopes: ScopeSet = required.iter().copied().filter(|s| s != drop).collect();
                    let g = grant(capabilities(&scopes));
                    assert!(
                        endpoint
                            .operations
                            .iter()
                            .any(|op| authorize(Some(&g), None, *op, 0).is_err()),
                        "{} without {drop}",
                        endpoint.name
                    );
                }
            }
        }
    }

    #[test]
    fn owner_only_and_native_host_capabilities_are_never_reachable_by_scopes() {
        let all = capabilities(&Scope::ALL.into_iter().collect());
        for cap in [
            Capability::GrantAdmin,
            Capability::UiSettings,
            Capability::LocalFileRead,
            Capability::LocalFileWrite,
            Capability::CapturePreview,
            Capability::DestinationRead,
            Capability::LibraryRead,
        ] {
            assert!(!all.contains(&cap), "{cap:?}");
        }
        // The #25 native-host preset is unchanged by #26.
        assert_eq!(
            presets::native_host(),
            [Capability::CapturePreview, Capability::DestinationRead]
                .into_iter()
                .collect()
        );
        assert_eq!(
            validate_token(ClientKind::NativeHost, &profiles::mcp_read_only(), None, 0),
            Err(TokenPolicyError::KindNotIssuable)
        );
    }

    #[test]
    fn mcp_read_only_profile_allows_exactly_the_reads_of_the_25_preset() {
        let profile = grant(capabilities(&profiles::mcp_read_only()));
        let preset = grant(presets::mcp_read_only());
        for endpoint in ENDPOINTS {
            for op in endpoint.operations {
                assert_eq!(
                    authorize(Some(&profile), None, *op, 0).is_ok(),
                    authorize(Some(&preset), None, *op, 0).is_ok(),
                    "{op:?}"
                );
            }
        }
        for op in [
            Operation::PaperCreate,
            Operation::PaperTrash,
            Operation::AnnotationWrite,
            Operation::OrganizationWrite,
            Operation::ImportCommit,
            Operation::Export,
            Operation::JobSubmit,
            Operation::GrantIssue,
            Operation::ApiSettingsWrite,
        ] {
            assert!(authorize(Some(&profile), None, op, 0).is_err(), "{op:?}");
        }
        assert!(profiles::mcp_write().contains(&Scope::PapersWrite));
        assert!(!profiles::mcp_write().contains(&Scope::PapersDelete));
    }

    #[test]
    fn display_scopes_of_bundles() {
        assert_eq!(
            scopes_of(&presets::mcp_read_only()),
            profiles::mcp_read_only()
        );
        for scopes in subsets() {
            assert_eq!(scopes_of(&capabilities(&scopes)), scopes);
        }
    }

    #[test]
    fn lifetime_policy() {
        let read = profiles::mcp_read_only();
        let write = profiles::mcp_write();
        let now = 1_000;
        assert_eq!(
            validate_token(ClientKind::Mcp, &read, None, now),
            Ok(now + 90 * DAY_MS)
        );
        assert_eq!(
            validate_token(ClientKind::Mcp, &write, None, now),
            Ok(now + 30 * DAY_MS)
        );
        assert_eq!(
            validate_token(ClientKind::Api, &write, Some(now + 91 * DAY_MS), now),
            Err(TokenPolicyError::ExpiryTooLong)
        );
        assert_eq!(
            validate_token(ClientKind::Api, &read, Some(now), now),
            Err(TokenPolicyError::ExpiryInPast)
        );
        assert_eq!(
            validate_token(ClientKind::Cli, &ScopeSet::new(), None, now),
            Err(TokenPolicyError::NoScopes)
        );
        let elevated: ScopeSet = [Scope::PapersDelete, Scope::BulkElevated].into();
        assert_eq!(
            validate_token(ClientKind::Api, &elevated, None, now),
            Err(TokenPolicyError::ScopeNotIssuable)
        );
        assert_eq!(
            validate_token(ClientKind::Desktop, &read, None, now),
            Err(TokenPolicyError::KindNotIssuable)
        );
        // Widening to write pulls a long read-only expiry in; narrowing never extends it.
        assert_eq!(
            expiry_after_scope_change(&write, Some(now + 300 * DAY_MS), now),
            now + 90 * DAY_MS
        );
        assert_eq!(
            expiry_after_scope_change(&read, Some(now + DAY_MS), now),
            now + DAY_MS
        );
    }
}
