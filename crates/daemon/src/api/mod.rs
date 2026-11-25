//! #26 token/grant management and local API settings (`Command::Api`).
//!
//! Tokens for API, MCP and CLI clients are #25 daemon grants whose capabilities come from
//! `policy::scopes`. Everything goes through the live `Authenticator`, so issuing,
//! re-issuing, scope changes, elevation and revocation affect the next request of every
//! live session on any transport. The secret is returned once and kept in the OS secret
//! store (`cm_protocol::client::secrets`); the database holds only its SHA-256.
use crate::{
    audit::AuditLog,
    ipc::auth::{Authenticator, DESKTOP_BOOTSTRAP, IssueError, NewGrant, StoredGrantExt},
    jobs::JobEngine,
};
use cm_core::{
    ServiceError,
    policy::{
        self, CapabilitySet, Denial, GrantId, Operation,
        api::ApiSettings,
        elevation::{self, BulkKind},
        scopes::{self, TokenPolicyError},
    },
};
use cm_protocol::{
    api::{ApiCommand, ApiOverview, ApiReply, IssueToken, ListenerState, TokenInfo},
    client::secrets::{self, SecretStore},
    ipc::WireError,
};
use cm_storage::daemon::StoredGrant;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

pub struct ApiService {
    auth: Arc<Authenticator>,
    secrets: Arc<dyn SecretStore>,
    library_key: String,
    /// Canonical library path (export folders may not be inside or above it).
    library: PathBuf,
    settings: RwLock<ApiSettings>,
    pub audit: Arc<AuditLog>,
}

fn invalid(reason: impl Into<String>) -> WireError {
    WireError::Service(ServiceError::InvalidInput(reason.into()))
}
fn issue_error(e: IssueError) -> WireError {
    match e {
        IssueError::Denied(d) => WireError::Denied(d),
        IssueError::Invalid(r) => invalid(r),
        IssueError::Service(e) => WireError::Service(e),
        IssueError::Unavailable => WireError::Unavailable,
    }
}
fn policy_error(e: TokenPolicyError) -> WireError {
    invalid(e.to_string())
}

/// Grant metadata for display. Pre-#26 grants show the scopes their capabilities cover.
/// `elevation`: the kind of the running (monotonic) elevation, if any.
pub fn token_info(stored: &StoredGrant, elevation: Option<BulkKind>) -> TokenInfo {
    TokenInfo {
        id: stored.grant.id,
        label: stored.grant.label.clone(),
        kind: stored.display_kind(),
        scopes: stored
            .scopes
            .clone()
            .unwrap_or_else(|| scopes::scopes_of(&stored.grant.capabilities)),
        capabilities: stored.grant.capabilities.clone(),
        created_at_ms: stored.created_at_ms,
        expires_at_ms: stored.grant.expires_at_ms,
        last_used_at_ms: stored.last_used_at_ms,
        revoked: stored.grant.revoked,
        elevated_until_ms: elevation.and(stored.grant.elevated_until_ms),
        elevated_kind: elevation,
        issued_by: stored.issued_by,
        replaces: stored.replaces,
    }
}

/// A real directory reached without symlinks (each component checked).
fn real_directory(dir: &Path) -> bool {
    crate::desktop_worker::files::safe(dir, false).is_ok()
}

/// Canonical `$HOME`, refused as an export folder.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .and_then(|h| std::fs::canonicalize(h).ok())
}

impl ApiService {
    pub fn new(
        auth: Arc<Authenticator>,
        secrets: Arc<dyn SecretStore>,
        library_key: String,
        library: PathBuf,
        audit: Arc<AuditLog>,
    ) -> Result<Self, ServiceError> {
        let settings = auth
            .control()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .api_settings()
            .load()?;
        // Remove stored copies of tokens that can no longer authenticate. In the background:
        // an OS store helper may be slow; nothing depends on it finishing.
        let stale: Vec<String> = auth
            .list_stored()
            .into_iter()
            .filter(|s| {
                s.client_kind.scoped_token()
                    && (s.grant.revoked
                        || s.grant
                            .expires_at_ms
                            .is_some_and(|at| at <= crate::ipc::auth::now_ms()))
            })
            .map(|s| secrets::account(&library_key, s.grant.id))
            .collect();
        if !stale.is_empty() {
            let store = secrets.clone();
            let _ = std::thread::Builder::new()
                .name("papersd-secret-gc".into())
                .spawn(move || {
                    for account in stale {
                        let _ = store.delete(&account);
                    }
                });
        }
        Ok(Self {
            auth,
            secrets,
            library_key,
            library,
            settings: RwLock::new(settings),
            audit,
        })
    }
    fn info(&self, stored: &StoredGrant) -> TokenInfo {
        token_info(stored, self.auth.elevation_of(stored.grant.id))
    }
    fn overview(&self, settings: ApiSettings) -> ApiOverview {
        ApiOverview {
            settings,
            secret_backend: self.secrets.backend(),
            listener: ListenerState::NotAvailable,
            audit_warning: self.audit.warning(),
        }
    }
    /// Export folder rules beyond `ApiSettings::validate`: an existing directory reached
    /// without symlinks, not `$HOME`, not the library, inside it or one of its ancestors.
    fn check_export_dir(&self, dir: &Path) -> Result<(), WireError> {
        if !real_directory(dir) {
            return Err(invalid(format!(
                "export folder is not an existing directory reached without symlinks: {}",
                dir.display()
            )));
        }
        let canonical = std::fs::canonicalize(dir).map_err(|e| invalid(e.to_string()))?;
        if policy::export::export_dir_refused(&canonical, home_dir().as_deref(), &self.library) {
            return Err(invalid(format!(
                "export folder may not be your home folder, the library or a folder containing it: {}",
                dir.display()
            )));
        }
        Ok(())
    }
    pub fn settings(&self) -> ApiSettings {
        self.settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Export folders `exports:write` tokens may write into.
    pub fn export_dirs(&self) -> Vec<PathBuf> {
        self.settings().export_dirs
    }
    pub fn secret_account(&self, grant: GrantId) -> String {
        secrets::account(&self.library_key, grant)
    }
    /// Best effort; a stale entry only holds a secret the daemon no longer accepts.
    pub fn forget_secret(&self, grant: GrantId) {
        let _ = self.secrets.delete(&self.secret_account(grant));
    }

    /// `session`/`requested` identify the authorized GrantAdmin caller (checked already).
    pub fn handle(
        &self,
        session: GrantId,
        requested: Option<&CapabilitySet>,
        command: ApiCommand,
        jobs: &JobEngine,
        now: i64,
    ) -> Result<ApiReply, WireError> {
        match command {
            ApiCommand::Overview => Ok(ApiReply::Overview(self.overview(self.settings()))),
            ApiCommand::SaveSettings(settings) => {
                settings.validate().map_err(|e| invalid(e.to_string()))?;
                for dir in &settings.export_dirs {
                    self.check_export_dir(dir)?;
                }
                self.auth
                    .control()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .api_settings()
                    .save(&settings, now)?;
                *self.settings.write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
                Ok(ApiReply::Overview(self.overview(settings)))
            }
            ApiCommand::ListTokens => Ok(ApiReply::Tokens(
                self.auth
                    .list_stored()
                    .iter()
                    .map(|s| self.info(s))
                    .collect(),
            )),
            ApiCommand::IssueToken(request) => self.issue(session, requested, request, None, now),
            ApiCommand::Reissue(old) => {
                let stored = self.scoped_live(old, now)?;
                let scopes = stored
                    .scopes
                    .clone()
                    .unwrap_or_else(|| scopes::scopes_of(&stored.grant.capabilities));
                // Same lifetime length as before, within today's policy maximum.
                let max = scopes::lifetime(&scopes).max_ms;
                let length = stored
                    .grant
                    .expires_at_ms
                    .map_or(max, |at| (at - stored.created_at_ms).clamp(1, max));
                let reply = self.issue(
                    session,
                    requested,
                    IssueToken {
                        label: stored.grant.label.clone(),
                        kind: stored.client_kind,
                        scopes,
                        expires_at_ms: Some(now.saturating_add(length)),
                    },
                    Some(old),
                    now,
                )?;
                // Rotation is all or nothing: if the old token cannot be revoked, the new
                // one (whose secret the caller will never see) is revoked as well.
                if let Err(e) = self.revoke(old, jobs, now) {
                    if let ApiReply::Issued { token, .. } = &reply {
                        let _ = self.revoke(token.id, jobs, now);
                    }
                    return Err(e);
                }
                Ok(reply)
            }
            ApiCommand::Revoke(grant) => {
                self.revoke(grant, jobs, now)?;
                Ok(ApiReply::Revoked(grant))
            }
            ApiCommand::SetScopes { grant, scopes } => {
                let stored = self.scoped_live(grant, now)?;
                if scopes.is_empty() {
                    return Err(policy_error(TokenPolicyError::NoScopes));
                }
                if scopes.iter().any(|s| !s.issuable()) {
                    return Err(policy_error(TokenPolicyError::ScopeNotIssuable));
                }
                let capabilities = scopes::capabilities(&scopes);
                // Widening is an owner decision bounded by the owner's own session.
                let issuer = self.auth.stored(session).map(|s| s.grant);
                policy::may_issue(issuer.as_ref(), requested, &capabilities, now)
                    .map_err(WireError::Denied)?;
                let expires =
                    scopes::expiry_after_scope_change(&scopes, stored.grant.expires_at_ms, now);
                if !self
                    .auth
                    .set_access(grant, capabilities, Some(scopes), Some(expires))?
                {
                    return Err(WireError::Service(ServiceError::NotFound));
                }
                // The running elevation needs the base scope it lifts.
                if let (Some(updated), Some(kind)) =
                    (self.auth.stored(grant), self.auth.elevation_of(grant))
                    && elevation::elevation_length(&updated.grant.capabilities, kind, 1).is_err()
                {
                    self.auth.set_elevation(grant, None, now)?;
                }
                self.updated(grant)
            }
            ApiCommand::Elevate {
                grant,
                kind,
                duration_ms,
            } => {
                let stored = self.scoped_live(grant, now)?;
                let mut length =
                    elevation::elevation_length(&stored.grant.capabilities, kind, duration_ms)
                        .map_err(|e| invalid(e.to_string()))?;
                // Never past the token's own expiry.
                if let Some(at) = stored.grant.expires_at_ms {
                    length = length.min(std::time::Duration::from_millis(
                        u64::try_from(at - now).unwrap_or(0),
                    ));
                }
                self.auth.set_elevation(grant, Some((length, kind)), now)?;
                self.updated(grant)
            }
            ApiCommand::EndElevation(grant) => {
                self.auth.set_elevation(grant, None, now)?;
                self.updated(grant)
            }
            ApiCommand::Audit { limit } => Ok(ApiReply::Audit(self.audit.tail(limit as usize))),
            _ => Err(invalid("unsupported API command")),
        }
    }

    fn updated(&self, grant: GrantId) -> Result<ApiReply, WireError> {
        self.auth
            .stored(grant)
            .map(|s| ApiReply::Updated(self.info(&s)))
            .ok_or(WireError::Service(ServiceError::NotFound))
    }

    /// A live API/MCP/CLI token (never the desktop grant or a native host).
    fn scoped_live(&self, grant: GrantId, now: i64) -> Result<StoredGrant, WireError> {
        let stored = self
            .auth
            .stored(grant)
            .ok_or(WireError::Service(ServiceError::NotFound))?;
        policy::check_live(Some(&stored.grant), now).map_err(|d| match d {
            Denial::Revoked => invalid("token is revoked"),
            Denial::Expired => invalid("token has expired; issue a new one"),
            _ => WireError::Service(ServiceError::NotFound),
        })?;
        if !stored.client_kind.scoped_token() {
            return Err(invalid(
                "only API, MCP and CLI tokens can be changed here; revoke other grants instead",
            ));
        }
        Ok(stored)
    }

    fn issue(
        &self,
        session: GrantId,
        requested: Option<&CapabilitySet>,
        request: IssueToken,
        replaces: Option<GrantId>,
        now: i64,
    ) -> Result<ApiReply, WireError> {
        let expires =
            scopes::validate_token(request.kind, &request.scopes, request.expires_at_ms, now)
                .map_err(policy_error)?;
        let (stored, credential) = self
            .auth
            .issue_with(
                NewGrant {
                    issuer: Some(session),
                    issuer_requested: requested,
                    label: &request.label,
                    capabilities: scopes::capabilities(&request.scopes),
                    expires_at_ms: Some(expires),
                    client_kind: request.kind,
                    scopes: Some(request.scopes),
                    replaces,
                },
                now,
            )
            .map_err(issue_error)?;
        let account = self.secret_account(stored.grant.id);
        let stored_in = self.secrets.put(&account, &credential).ok();
        Ok(ApiReply::Issued {
            token: self.info(&stored),
            credential,
            stored_in,
            secret_account: account,
        })
    }

    fn revoke(&self, grant: GrantId, jobs: &JobEngine, now: i64) -> Result<(), WireError> {
        let stored = self
            .auth
            .stored(grant)
            .ok_or(WireError::Service(ServiceError::NotFound))?;
        if stored.grant.label == DESKTOP_BOOTSTRAP {
            return Err(invalid(
                "the desktop grant is the owner's own; it rotates when its credential file is removed",
            ));
        }
        let revoked = self.auth.revoke(grant, now);
        // Fail closed like GrantCommand::Revoke: jobs stop even if persisting failed.
        let jobs = jobs.owner_revoked(grant);
        self.forget_secret(grant);
        revoked?;
        jobs?;
        Ok(())
    }
}

/// Operations that are recorded in the audit log when they succeed or fail. Reads (and
/// frequent UI preference saves) are not recorded; every denial is.
pub fn audited(operation: Operation) -> bool {
    use Operation::*;
    !matches!(
        operation,
        PaperList
            | PaperGet
            | LibraryQuery
            | OrganizationList
            | OrganizationForPaper
            | SettingsRead
            | SettingsWrite
            | ImportParseFile
            | ImportPreview
            | ImportCancel
            | MetadataResolve
            // #22: a capture preview never persists (like ImportPreview), cancelling one is
            // ImportCancel's analogue and PDF job status is a read. Capture saves and PDF
            // retries are recorded.
            | CaptureSubmit
            | CaptureCancel
            | CapturePdfStatus
            | PdfInspect
            | PdfPreviewNew
            | AttachmentList
            | JobRead
            | GrantList
            | FulltextSearch
            | FulltextStatus
            | FulltextRead
            | PdfRender
            | AnnotationRead
            | ApiSettingsRead
            | AuditRead
            | Status
    )
}

/// Fixed reason code for an error (never its message, which may echo input).
pub fn reason_code(error: &WireError) -> &'static str {
    match error {
        WireError::Denied(Denial::Unauthenticated) => "unauthenticated",
        WireError::Denied(Denial::Revoked) => "revoked",
        WireError::Denied(Denial::Expired) => "expired",
        WireError::Denied(Denial::Forbidden(_)) => "forbidden",
        WireError::Service(ServiceError::InvalidInput(_)) => "invalid_input",
        WireError::Service(ServiceError::NotFound) => "not_found",
        WireError::Service(ServiceError::Conflict) => "conflict",
        WireError::Service(ServiceError::Unavailable) | WireError::Unavailable => "unavailable",
        WireError::Busy => "busy",
        WireError::TooLarge => "too_large",
        WireError::Cancelled => "cancelled",
    }
}
