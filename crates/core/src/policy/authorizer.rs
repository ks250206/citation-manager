//! Default-deny authorization over daemon-owned grants (#25, security ADR).
//!
//! The daemon resolves an authenticated credential to a stored `Grant` on every request
//! (so revocation/expiry take effect for live sessions). Client-requested scopes can only
//! narrow a session; they never add capabilities.
use super::capabilities::{Capability, CapabilitySet, Operation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrantId(pub uuid::Uuid);
impl std::fmt::Display for GrantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A daemon-owned grant. `label` is a diagnostic name chosen at issuance, not authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: GrantId,
    pub label: String,
    pub capabilities: CapabilitySet,
    /// Unix epoch milliseconds; `None` means until revoked.
    pub expires_at_ms: Option<i64>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Denial {
    /// No credential, unknown credential, or a grant that no longer exists.
    Unauthenticated,
    Revoked,
    Expired,
    /// The live grant (after session narrowing) lacks the operation's requirement.
    Forbidden(Operation),
}
impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthenticated => f.write_str("unauthenticated"),
            Self::Revoked => f.write_str("grant revoked"),
            Self::Expired => f.write_str("grant expired"),
            Self::Forbidden(op) => write!(f, "permission denied for {op:?}"),
        }
    }
}
impl std::error::Error for Denial {}

/// Liveness only: revoked/expired grants authenticate nothing.
pub fn check_live(grant: Option<&Grant>, now_ms: i64) -> Result<&Grant, Denial> {
    let grant = grant.ok_or(Denial::Unauthenticated)?;
    if grant.revoked {
        return Err(Denial::Revoked);
    }
    if grant.expires_at_ms.is_some_and(|at| now_ms >= at) {
        return Err(Denial::Expired);
    }
    Ok(grant)
}

/// Effective session capabilities: stored grant ∩ requested narrowing (if any).
pub fn effective(grant: &Grant, requested: Option<&CapabilitySet>) -> CapabilitySet {
    match requested {
        Some(requested) => grant
            .capabilities
            .intersection(requested)
            .copied()
            .collect(),
        None => grant.capabilities.clone(),
    }
}

/// Default deny: live grant AND the operation's requirement within the narrowed set.
pub fn authorize(
    grant: Option<&Grant>,
    requested: Option<&CapabilitySet>,
    operation: Operation,
    now_ms: i64,
) -> Result<CapabilitySet, Denial> {
    let grant = check_live(grant, now_ms)?;
    let held = effective(grant, requested);
    if operation.requirement().satisfied_by(&held) {
        Ok(held)
    } else {
        Err(Denial::Forbidden(operation))
    }
}

/// A grant may only be issued by a live GrantAdmin session and only as a subset of what
/// that session itself holds. This is the "separate user approval path" for elevation:
/// a read-only MCP grant can never mint itself write access.
pub fn may_issue(
    issuer: Option<&Grant>,
    issuer_requested: Option<&CapabilitySet>,
    capabilities: &CapabilitySet,
    now_ms: i64,
) -> Result<(), Denial> {
    let held = authorize(issuer, issuer_requested, Operation::GrantIssue, now_ms)?;
    if capabilities.is_empty() || !capabilities.is_subset(&held) {
        return Err(Denial::Forbidden(Operation::GrantIssue));
    }
    Ok(())
}

/// Whether job/preview visibility spans all grants (owner/admin) or only this grant's own.
pub fn sees_all_owners(held: &CapabilitySet) -> bool {
    held.contains(&Capability::GrantAdmin)
}

#[cfg(test)]
mod tests {
    use super::super::capabilities::{Capability::*, presets};
    use super::*;

    fn grant(caps: CapabilitySet) -> Grant {
        Grant {
            id: GrantId(uuid::Uuid::nil()),
            label: "test".into(),
            capabilities: caps,
            expires_at_ms: Some(1_000),
            revoked: false,
        }
    }

    #[test]
    fn default_deny_and_liveness() {
        assert_eq!(
            authorize(None, None, Operation::Status, 0),
            Err(Denial::Unauthenticated)
        );
        let mut g = grant(presets::desktop());
        assert!(authorize(Some(&g), None, Operation::PaperTrash, 999).is_ok());
        assert_eq!(
            authorize(Some(&g), None, Operation::PaperList, 1_000),
            Err(Denial::Expired)
        );
        g.revoked = true;
        assert_eq!(
            authorize(Some(&g), None, Operation::Status, 0),
            Err(Denial::Revoked)
        );
    }

    #[test]
    fn native_host_and_mcp_presets_are_minimal() {
        let host = grant(presets::native_host());
        for op in [
            Operation::PaperGet,
            Operation::PaperTrash,
            Operation::Export,
            Operation::ImportParseFile,
            Operation::PdfInspect,
            Operation::ImportCommit,
            Operation::OrganizationDelete,
            Operation::GrantIssue,
            // #22: capture commit is preview-bound; no general network/job control.
            Operation::MetadataResolve,
            Operation::JobSubmit,
            Operation::JobRetry,
            Operation::PaperCreate,
            Operation::FulltextSearch,
            Operation::FulltextStatus,
            Operation::FulltextRead,
        ] {
            assert_eq!(
                authorize(Some(&host), None, op, 0),
                Err(Denial::Forbidden(op))
            );
        }
        assert!(authorize(Some(&host), None, Operation::OrganizationList, 0).is_ok());
        assert!(authorize(Some(&host), None, Operation::ImportPreview, 0).is_ok());
        for op in [
            Operation::CaptureSubmit,
            Operation::CaptureSave,
            Operation::CaptureCancel,
            Operation::CapturePdfStatus,
            Operation::CapturePdfRetry,
        ] {
            assert!(authorize(Some(&host), None, op, 0).is_ok(), "{op:?}");
        }
        let mcp = grant(presets::mcp_read_only());
        // MCP read-only cannot capture.
        assert!(authorize(Some(&mcp), None, Operation::CaptureSave, 0).is_err());
        assert!(authorize(Some(&mcp), None, Operation::CaptureSubmit, 0).is_err());
        assert!(authorize(Some(&mcp), None, Operation::LibraryQuery, 0).is_ok());
        for op in [
            Operation::FulltextSearch,
            Operation::FulltextStatus,
            Operation::FulltextRead,
        ] {
            assert!(authorize(Some(&mcp), None, op, 0).is_ok());
        }
        for op in [
            Operation::PaperCreate,
            Operation::PaperUpdate,
            Operation::ImportCommit,
            Operation::JobSubmit,
        ] {
            assert!(authorize(Some(&mcp), None, op, 0).is_err());
        }
    }

    #[test]
    fn requested_scopes_only_narrow() {
        let mcp = grant(presets::mcp_read_only());
        let everything: CapabilitySet = Capability::ALL.into_iter().collect();
        assert_eq!(
            authorize(Some(&mcp), Some(&everything), Operation::PaperCreate, 0),
            Err(Denial::Forbidden(Operation::PaperCreate))
        );
        let desktop = grant(presets::desktop());
        let read: CapabilitySet = [LibraryRead].into_iter().collect();
        assert!(authorize(Some(&desktop), Some(&read), Operation::PaperGet, 0).is_ok());
        assert!(authorize(Some(&desktop), Some(&read), Operation::PaperCreate, 0).is_err());
    }

    #[test]
    fn issuance_requires_admin_and_subset() {
        let mcp = grant(presets::mcp_read_only());
        let write: CapabilitySet = [LibraryWrite].into_iter().collect();
        assert!(may_issue(Some(&mcp), None, &write, 0).is_err());
        let desktop = grant(presets::desktop());
        assert!(may_issue(Some(&desktop), None, &write, 0).is_ok());
        assert!(may_issue(Some(&desktop), None, &CapabilitySet::new(), 0).is_err());
        let narrowed: CapabilitySet = [GrantAdmin, LibraryRead].into_iter().collect();
        assert!(may_issue(Some(&desktop), Some(&narrowed), &write, 0).is_err());
    }
}
