//! #26 explicit elevation for bulk delete / bulk move.
//!
//! The daemon has per-item commands only, so a client could turn one "bulk" request into
//! many single ones. The guard therefore counts destructive and move operations per grant
//! in a sliding window, in the daemon, for every transport alike: past the burst limit the
//! request is decided as `BulkTrash`/`BulkMove`, which needs `BulkElevated`. Only the owner's
//! bootstrap grant holds that permanently; any other grant gets it through a user-approved
//! elevation of at most `MAX_ELEVATION_MS` (`Grant::elevated_until_ms`).
use super::{
    authorizer::GrantId,
    capabilities::{Capability, CapabilitySet, Operation},
};
use std::collections::{HashMap, VecDeque};

/// Longest elevation the user can approve at once.
pub const MAX_ELEVATION_MS: i64 = 15 * 60_000;
/// Sliding window of the burst limits.
pub const WINDOW_MS: i64 = 60_000;
/// Trash/restore/delete operations a grant without elevation may run per window.
pub const DESTRUCTIVE_BURST: usize = 10;
/// Collection/tag membership changes a grant without elevation may run per window.
pub const MOVE_BURST: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Destructive,
    Move,
}

/// The bulk operation an operation counts toward, if any. Callers classify membership
/// changes (`OrganizationWrite` that adds/removes a paper) as `move_operation = true`.
pub fn bulk_operation(operation: Operation, move_operation: bool) -> Option<Operation> {
    match operation {
        Operation::PaperTrash | Operation::PaperRestore | Operation::OrganizationDelete => {
            Some(Operation::BulkTrash)
        }
        Operation::OrganizationWrite if move_operation => Some(Operation::BulkMove),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationError {
    TooLong,
    NotPositive,
    /// Elevation only lifts the burst limit; the grant must already hold delete or write.
    NothingToElevate,
}
impl std::fmt::Display for ElevationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLong => "elevation is limited to 15 minutes",
            Self::NotPositive => "elevation duration must be positive",
            Self::NothingToElevate => "the token holds neither papers:write nor papers:delete",
        })
    }
}

/// Validates a requested elevation and returns its end.
pub fn elevation_until(
    capabilities: &CapabilitySet,
    duration_ms: i64,
    now_ms: i64,
) -> Result<i64, ElevationError> {
    if duration_ms <= 0 {
        return Err(ElevationError::NotPositive);
    }
    if duration_ms > MAX_ELEVATION_MS {
        return Err(ElevationError::TooLong);
    }
    if !capabilities.contains(&Capability::LibraryDelete)
        && !capabilities.contains(&Capability::LibraryWrite)
    {
        return Err(ElevationError::NothingToElevate);
    }
    Ok(now_ms.saturating_add(duration_ms))
}

/// Per-grant sliding-window counter. Holders of `BulkElevated` are not limited.
#[derive(Debug, Default)]
pub struct BulkGuard {
    recent: HashMap<(GrantId, u8), VecDeque<i64>>,
}
impl BulkGuard {
    /// Records the operation if allowed. `Err(op)` names the bulk operation that would be
    /// required (`BulkTrash`/`BulkMove`); nothing is recorded then.
    pub fn check(
        &mut self,
        grant: GrantId,
        held: &CapabilitySet,
        operation: Operation,
        move_operation: bool,
        now_ms: i64,
    ) -> Result<(), Operation> {
        let Some(bulk) = bulk_operation(operation, move_operation) else {
            return Ok(());
        };
        if held.contains(&Capability::BulkElevated) {
            return Ok(());
        }
        let (kind, limit) = if bulk == Operation::BulkTrash {
            (Kind::Destructive, DESTRUCTIVE_BURST)
        } else {
            (Kind::Move, MOVE_BURST)
        };
        let window = self.recent.entry((grant, kind as u8)).or_default();
        while window.front().is_some_and(|at| now_ms - at >= WINDOW_MS) {
            window.pop_front();
        }
        if window.len() >= limit {
            return Err(bulk);
        }
        window.push_back(now_ms);
        Ok(())
    }
    /// Forget a revoked grant.
    pub fn forget(&mut self, grant: GrantId) {
        self.recent.retain(|(g, _), _| *g != grant);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Capability::*;

    fn set(caps: &[Capability]) -> CapabilitySet {
        caps.iter().copied().collect()
    }

    #[test]
    fn burst_limit_requires_elevation_per_grant_and_window() {
        let mut guard = BulkGuard::default();
        let a = GrantId(uuid::Uuid::from_u128(1));
        let b = GrantId(uuid::Uuid::from_u128(2));
        let delete = set(&[LibraryDelete]);
        for i in 0..DESTRUCTIVE_BURST {
            assert!(
                guard
                    .check(a, &delete, Operation::PaperTrash, false, i as i64)
                    .is_ok()
            );
        }
        assert_eq!(
            guard.check(a, &delete, Operation::PaperTrash, false, 100),
            Err(Operation::BulkTrash)
        );
        // Another grant has its own budget; reads and plain writes never count.
        assert!(
            guard
                .check(b, &delete, Operation::OrganizationDelete, false, 100)
                .is_ok()
        );
        assert!(
            guard
                .check(a, &delete, Operation::PaperUpdate, false, 100)
                .is_ok()
        );
        // The window slides.
        assert!(
            guard
                .check(a, &delete, Operation::PaperTrash, false, WINDOW_MS)
                .is_ok()
        );
        // Elevation lifts the limit.
        let elevated = set(&[LibraryDelete, BulkElevated]);
        for _ in 0..50 {
            assert!(
                guard
                    .check(a, &elevated, Operation::PaperTrash, false, 200)
                    .is_ok()
            );
        }
        let write = set(&[LibraryWrite]);
        for i in 0..MOVE_BURST {
            assert!(
                guard
                    .check(a, &write, Operation::OrganizationWrite, true, i as i64)
                    .is_ok()
            );
        }
        assert_eq!(
            guard.check(a, &write, Operation::OrganizationWrite, true, 30),
            Err(Operation::BulkMove)
        );
        assert!(
            guard
                .check(a, &write, Operation::OrganizationWrite, false, 30)
                .is_ok()
        );
    }

    #[test]
    fn elevation_is_short_and_needs_a_base_scope() {
        let delete = set(&[LibraryDelete]);
        assert_eq!(
            elevation_until(&delete, MAX_ELEVATION_MS, 5),
            Ok(5 + MAX_ELEVATION_MS)
        );
        assert_eq!(
            elevation_until(&delete, MAX_ELEVATION_MS + 1, 5),
            Err(ElevationError::TooLong)
        );
        assert_eq!(
            elevation_until(&delete, 0, 5),
            Err(ElevationError::NotPositive)
        );
        assert_eq!(
            elevation_until(&set(&[LibraryRead, MetadataRead]), 1_000, 5),
            Err(ElevationError::NothingToElevate)
        );
    }
}
