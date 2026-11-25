//! #26 token/grant management and local API settings over IPC (`Command::Api`).
//!
//! Every command needs the owner's `GrantAdmin` (the desktop GUI or the `papersd token`
//! bootstrap using the desktop credential); `SaveSettings` additionally needs the owner-only
//! `ApiAdmin`, and `Elevate`/`EndElevation` the owner-only `ElevationAdmin`. Tokens are daemon grants: issuing, re-issuing,
//! scope changes, elevation and revocation update the daemon's live registry, so they apply
//! to the next request of every live session regardless of transport.
use crate::ipc::{Credential, GrantId, Operation};
pub use cm_core::policy::{
    Capability, CapabilitySet,
    api::{ApiSettings, ApiSettingsError, BindAddress, DEFAULT_PORT},
    elevation::{BulkKind, MAX_ELEVATION_MS, elevatable_kinds},
    scopes::{
        Access, ClientKind, ENDPOINTS, Endpoint, Lifetime, Scope, ScopeSet, Surface,
        TokenPolicyError, capabilities as scope_capabilities, endpoint, lifetime, profiles,
        scopes_of, validate_token,
    },
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Longest one OS secret-store helper run (`/usr/bin/security`, `/usr/bin/secret-tool`) may
/// take before the daemon kills it (`client::secrets`). A slow keyring, or one waiting for
/// the user to unlock it, can use all of it.
pub const SECRET_HELPER_TIMEOUT: Duration = Duration::from_secs(20);

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
    /// Time-limited bulk elevation of one kind (at most `MAX_ELEVATION_MS`). Owner only.
    Elevate {
        grant: GrantId,
        kind: BulkKind,
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
            Self::SetScopes { .. } => Operation::GrantUpdate,
            Self::Elevate { .. } | Self::EndElevation(_) => Operation::GrantElevate,
            Self::Audit { .. } => Operation::AuditRead,
        }
    }
    /// Most OS secret-store helper runs the daemon may wait on, one after another, before
    /// it answers this command (each bounded by `SECRET_HELPER_TIMEOUT`). Clients size the
    /// command's deadline from it: a reply discarded after the daemon finished issuing would
    /// lose the only copy of the new secret the person sees.
    pub fn secret_store_steps(&self) -> u32 {
        match self {
            // `put`: the Keychain store writes, then reads back to confirm.
            Self::IssueToken(_) => 2,
            // Issue (write + read-back), delete the old secret, and if the old grant cannot
            // be revoked, revoke the new one too (deleting its secret).
            Self::Reissue(_) => 4,
            // Deletes the stored secret.
            Self::Revoke(_) => 1,
            Self::Overview
            | Self::SaveSettings(_)
            | Self::ListTokens
            | Self::SetScopes { .. }
            | Self::Elevate { .. }
            | Self::EndElevation(_)
            | Self::Audit { .. } => 0,
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
    /// Wall-clock end of a running elevation, for display (the daemon enforces it on a
    /// monotonic clock; elevations do not survive a daemon restart).
    pub elevated_until_ms: Option<i64>,
    #[serde(default)]
    pub elevated_kind: Option<BulkKind>,
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
    /// 0600 file in a per-library 0700 folder outside the library (user data folder).
    /// Weaker: any process of the same user can read it and home backups include it.
    File,
    /// Tests only.
    Memory,
}
impl SecretBackend {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Keychain => "macOS Keychain",
            Self::SecretService => "Secret Service (desktop keyring)",
            Self::File => "private 0600 file outside the library (weaker fallback)",
            Self::Memory => "memory only (test store)",
        }
    }
}

/// The REST listener is #27; until then the daemon only stores settings.
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
    /// Set when the audit log had to be moved aside (unsafe permissions/links) or cannot
    /// be written; entries are not silently dropped.
    #[serde(default)]
    pub audit_warning: Option<String>,
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
    /// Identical denials (grant, operation, reason) within a minute are coalesced into one
    /// record carrying how many occurred.
    #[serde(default = "one")]
    pub count: u32,
}
fn one() -> u32 {
    1
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
