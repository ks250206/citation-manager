//! #26 token/grant management and local API settings over IPC (`Command::Api`).
//!
//! Every command needs the owner's `GrantAdmin` (the desktop GUI or the `papersd token`
//! bootstrap using the desktop credential). Tokens are daemon grants: issuing, re-issuing,
//! scope changes, elevation and revocation update the daemon's live registry, so they apply
//! to the next request of every live session regardless of transport.
use crate::ipc::{Credential, GrantId, Operation};
pub use cm_core::policy::{
    Capability, CapabilitySet,
    api::{ApiSettings, ApiSettingsError, BindAddress, DEFAULT_PORT},
    elevation::MAX_ELEVATION_MS,
    scopes::{
        Access, ClientKind, ENDPOINTS, Endpoint, Lifetime, Scope, ScopeSet, Surface,
        TokenPolicyError, endpoint, lifetime, profiles, scopes_of, validate_token,
    },
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ApiCommand {
    /// Settings, secret store backend and listener availability.
    Overview,
    SaveSettings(ApiSettings),
    /// Every grant (desktop, native hosts, tokens), revoked ones included.
    ListTokens,
    IssueToken(IssueToken),
    /// New secret with the same label/kind/scopes; the old grant is revoked in the same step.
    Reissue(GrantId),
    /// Revokes the grant, stops its jobs and deletes its stored secret.
    Revoke(GrantId),
    /// In-place scope change of a live API/MCP/CLI token (e.g. MCP "Allow writes"). Widening
    /// is an explicit owner decision; narrowing applies to live sessions immediately.
    SetScopes {
        grant: GrantId,
        scopes: ScopeSet,
    },
    /// Time-limited bulk elevation (at most `MAX_ELEVATION_MS`).
    Elevate {
        grant: GrantId,
        duration_ms: i64,
    },
    EndElevation(GrantId),
    /// Newest audit records first, at most `limit` (capped by the daemon).
    Audit {
        limit: u32,
    },
}
impl ApiCommand {
    pub fn operation(&self) -> Operation {
        match self {
            Self::Overview => Operation::ApiSettingsRead,
            Self::SaveSettings(_) => Operation::ApiSettingsWrite,
            Self::ListTokens => Operation::GrantList,
            Self::IssueToken(_) | Self::Reissue(_) => Operation::GrantIssue,
            Self::Revoke(_) => Operation::GrantRevoke,
            Self::SetScopes { .. } | Self::Elevate { .. } | Self::EndElevation(_) => {
                Operation::GrantUpdate
            }
            Self::Audit { .. } => Operation::AuditRead,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueToken {
    pub label: String,
    pub kind: ClientKind,
    pub scopes: ScopeSet,
    /// `None` = the policy default for these scopes (`lifetime`). Never "no expiry".
    pub expires_at_ms: Option<i64>,
}

/// Grant metadata. Never contains the secret or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub id: GrantId,
    pub label: String,
    pub kind: ClientKind,
    /// Issued scopes, or the scopes the capabilities cover for pre-#26 grants.
    pub scopes: ScopeSet,
    pub capabilities: CapabilitySet,
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub last_used_at_ms: Option<i64>,
    pub revoked: bool,
    pub elevated_until_ms: Option<i64>,
    pub issued_by: Option<GrantId>,
    pub replaces: Option<GrantId>,
}

/// Where token plaintext is kept besides being shown once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackend {
    /// macOS login Keychain (generic password, service `citation-manager`).
    Keychain,
    /// freedesktop Secret Service (GNOME Keyring/KWallet) through `secret-tool`.
    SecretService,
    /// 0600 file in the private library daemon directory. Weaker: any process of the same
    /// user can read it and it is included in file-level backups.
    File,
    /// Tests only.
    Memory,
}
impl SecretBackend {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Keychain => "macOS Keychain",
            Self::SecretService => "Secret Service (desktop keyring)",
            Self::File => "private 0600 file in the library (weaker fallback)",
            Self::Memory => "memory only (test store)",
        }
    }
}

/// #27 REST listener state as reported by papersd. `NotAvailable` is kept for builds or
/// daemons without the listener (pre-#27); papersd reports `Stopped` or `Listening`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenerState {
    NotAvailable,
    Stopped,
    Listening,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiOverview {
    pub settings: ApiSettings,
    pub secret_backend: SecretBackend,
    pub listener: ListenerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Succeeded,
    Denied,
    Failed,
}

/// Who/what/when/result. Never content, query text, paths, labels or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at_ms: i64,
    pub grant: Option<GrantId>,
    pub kind: Option<ClientKind>,
    pub operation: Operation,
    pub outcome: AuditOutcome,
    /// Fixed code (`forbidden`, `revoked`, `expired`, `invalid_input`, `not_found`, ...).
    pub reason: Option<String>,
    /// Grant the operation targeted (token management), if any.
    pub target: Option<GrantId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ApiReply {
    Overview(ApiOverview),
    Tokens(Vec<TokenInfo>),
    /// The only time a secret is returned. The daemon keeps its SHA-256; the plaintext is in
    /// the secret store under (`SECRET_SERVICE`, `secret_account`).
    Issued {
        token: TokenInfo,
        credential: Credential,
        stored_in: Option<SecretBackend>,
        secret_account: String,
    },
    Updated(TokenInfo),
    Revoked(GrantId),
    Audit(Vec<AuditEntry>),
}

/// Secret store service name for every token entry.
pub const SECRET_SERVICE: &str = "citation-manager";
