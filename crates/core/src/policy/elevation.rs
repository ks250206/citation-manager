//! #26 explicit elevation for bulk delete / bulk move.
//!
//! The daemon has per-item commands only, so a client could turn one "bulk" request into
//! many single ones. The guard therefore counts destructive and move operations per grant
//! in a sliding window, in the daemon, for every transport alike: past the burst limit the
//! request is decided as `BulkTrash`/`BulkMove`. Only the owner's bootstrap grant is exempt
//! permanently (it holds `BulkElevated`); any other grant is exempt only for the one
//! `BulkKind` the owner elevated it for, for at most `MAX_ELEVATION_MS`.
//!
//! Windows and elevation deadlines use a monotonic clock (`Instant`), so changing the wall
//! clock neither extends an elevation nor resets a window. Wall-clock instants are kept for
//! display only.
use super::{
    authorizer::GrantId,
    capabilities::{Capability, CapabilitySet, Operation},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

/// Longest elevation the user can approve at once.
pub const MAX_ELEVATION_MS: i64 = 15 * 60_000;
/// Sliding window of the burst limits.
pub const WINDOW: Duration = Duration::from_secs(60);
/// Trash/restore/delete operations (papers, collections/tags, annotations) a grant without
/// elevation may run per window.
pub const DESTRUCTIVE_BURST: usize = 10;
/// Collection/tag membership changes a grant without elevation may run per window.
pub const MOVE_BURST: usize = 25;

/// What an elevation lifts. One elevation covers exactly one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkKind {
    /// Trash/restore papers, delete collections/tags, delete annotations.
    Delete,
    /// Add/remove papers to/from collections and tags.
    Move,
}
impl BulkKind {
    /// Every kind, in the order a chooser offers them.
    pub const ALL: [BulkKind; 2] = [Self::Delete, Self::Move];
    /// The operation a request is decided as once the burst limit is exceeded.
    pub fn operation(self) -> Operation {
        match self {
            Self::Delete => Operation::BulkTrash,
            Self::Move => Operation::BulkMove,
        }
    }
    pub fn limit(self) -> usize {
        match self {
            Self::Delete => DESTRUCTIVE_BURST,
            Self::Move => MOVE_BURST,
        }
    }
    /// Base capability the elevated grant must already hold (elevation only lifts limits).
    fn base(self, held: &CapabilitySet) -> bool {
        match self {
            Self::Delete => {
                held.contains(&Capability::LibraryDelete)
                    || held.contains(&Capability::AnnotationsWrite)
            }
            Self::Move => held.contains(&Capability::LibraryWrite),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationError {
    TooLong,
    NotPositive,
    /// Elevation only lifts the burst limit; the grant must already hold the base scope.
    NothingToElevate,
}
impl std::fmt::Display for ElevationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLong => "elevation is limited to 15 minutes",
            Self::NotPositive => "elevation duration must be positive",
            Self::NothingToElevate => {
                "the token holds neither the delete nor the write scope this elevation lifts"
            }
        })
    }
}

/// The kinds a grant holding `capabilities` may be elevated for (each needs its base
/// capability; see `elevation_length`), in `BulkKind::ALL` order. A grant holding both the
/// delete and the write scope may be elevated for either, but only one at a time, so
/// clients must let the owner choose instead of picking one.
pub fn elevatable_kinds(capabilities: &CapabilitySet) -> Vec<BulkKind> {
    BulkKind::ALL
        .into_iter()
        .filter(|kind| kind.base(capabilities))
        .collect()
}

/// Validates a requested elevation and returns its monotonic length.
pub fn elevation_length(
    capabilities: &CapabilitySet,
    kind: BulkKind,
    duration_ms: i64,
) -> Result<Duration, ElevationError> {
    if duration_ms <= 0 {
        return Err(ElevationError::NotPositive);
    }
    if duration_ms > MAX_ELEVATION_MS {
        return Err(ElevationError::TooLong);
    }
    if !kind.base(capabilities) {
        return Err(ElevationError::NothingToElevate);
    }
    Ok(Duration::from_millis(duration_ms as u64))
}

/// Per-grant sliding-window counter.
#[derive(Debug, Default)]
pub struct BulkGuard {
    recent: HashMap<(GrantId, BulkKind), VecDeque<Instant>>,
}
impl BulkGuard {
    /// Records the operation if allowed. `exempt`: the grant holds a permanent `BulkElevated`
    /// or a running elevation for exactly this `kind`. `Err(op)` names the bulk operation
    /// that would be required; nothing is recorded then.
    pub fn check(
        &mut self,
        grant: GrantId,
        kind: Option<BulkKind>,
        exempt: bool,
        now: Instant,
    ) -> Result<(), Operation> {
        let Some(kind) = kind else {
            return Ok(());
        };
        if exempt {
            return Ok(());
        }
        let window = self.recent.entry((grant, kind)).or_default();
        while window
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW)
        {
            window.pop_front();
        }
        if window.len() >= kind.limit() {
            return Err(kind.operation());
        }
        window.push_back(now);
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
    fn burst_limit_per_grant_kind_and_monotonic_window() {
        let mut guard = BulkGuard::default();
        let a = GrantId(uuid::Uuid::from_u128(1));
        let b = GrantId(uuid::Uuid::from_u128(2));
        let t0 = Instant::now();
        let delete = Some(BulkKind::Delete);
        for _ in 0..DESTRUCTIVE_BURST {
            assert!(guard.check(a, delete, false, t0).is_ok());
        }
        assert_eq!(guard.check(a, delete, false, t0), Err(Operation::BulkTrash));
        // Another grant has its own budget; unclassified operations never count.
        assert!(guard.check(b, delete, false, t0).is_ok());
        assert!(guard.check(a, None, false, t0).is_ok());
        // Moves have their own budget, and an elevation for one kind is not the other.
        for _ in 0..MOVE_BURST {
            assert!(guard.check(a, Some(BulkKind::Move), false, t0).is_ok());
        }
        assert_eq!(
            guard.check(a, Some(BulkKind::Move), false, t0),
            Err(Operation::BulkMove)
        );
        assert!(guard.check(a, delete, true, t0).is_ok());
        // The window slides on the monotonic clock.
        assert!(guard.check(a, delete, false, t0 + WINDOW).is_ok());
        guard.forget(a);
        assert!(guard.check(a, Some(BulkKind::Move), false, t0).is_ok());
    }

    #[test]
    fn elevation_is_short_and_needs_the_base_scope_of_its_kind() {
        let delete = set(&[LibraryDelete]);
        assert_eq!(
            elevation_length(&delete, BulkKind::Delete, MAX_ELEVATION_MS),
            Ok(Duration::from_millis(MAX_ELEVATION_MS as u64))
        );
        assert_eq!(
            elevation_length(&delete, BulkKind::Delete, MAX_ELEVATION_MS + 1),
            Err(ElevationError::TooLong)
        );
        assert_eq!(
            elevation_length(&delete, BulkKind::Delete, 0),
            Err(ElevationError::NotPositive)
        );
        assert_eq!(
            elevation_length(&delete, BulkKind::Move, 1_000),
            Err(ElevationError::NothingToElevate)
        );
        assert!(elevation_length(&set(&[AnnotationsWrite]), BulkKind::Delete, 1_000).is_ok());
        assert_eq!(
            elevation_length(&set(&[LibraryRead, MetadataRead]), BulkKind::Delete, 1_000),
            Err(ElevationError::NothingToElevate)
        );
    }

    #[test]
    fn elevatable_kinds_match_elevation_length() {
        assert_eq!(
            elevatable_kinds(&set(&[LibraryDelete, LibraryWrite])),
            vec![BulkKind::Delete, BulkKind::Move]
        );
        assert_eq!(
            elevatable_kinds(&set(&[LibraryDelete])),
            vec![BulkKind::Delete]
        );
        assert_eq!(
            elevatable_kinds(&set(&[AnnotationsWrite])),
            vec![BulkKind::Delete]
        );
        assert_eq!(
            elevatable_kinds(&set(&[LibraryWrite])),
            vec![BulkKind::Move]
        );
        assert!(elevatable_kinds(&set(&[LibraryRead, MetadataRead])).is_empty());
        for caps in [
            set(&[LibraryDelete, LibraryWrite]),
            set(&[AnnotationsWrite]),
            set(&[LibraryWrite]),
            set(&[MetadataRead]),
        ] {
            for kind in BulkKind::ALL {
                assert_eq!(
                    elevatable_kinds(&caps).contains(&kind),
                    elevation_length(&caps, kind, 1_000).is_ok(),
                    "{caps:?} {kind:?}"
                );
            }
        }
    }
}
