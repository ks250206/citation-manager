//! Daemon-only tables (#25): grants and durable jobs. Only the lock-holding daemon
//! composition root should call these; GUI/CLI/MCP never open storage directly.
mod grants;
mod jobs;
pub use grants::{SqliteGrants, StoredGrant};
pub use jobs::{Finish, Heartbeat, JobRow, NewJob, SqliteJobs};

impl crate::LibraryDb {
    pub fn daemon_grants(&self) -> SqliteGrants<'_> {
        SqliteGrants {
            connection: &self.connection,
        }
    }
    pub fn daemon_jobs(&mut self) -> SqliteJobs<'_> {
        SqliteJobs {
            connection: &mut self.connection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LibraryDb;
    use cm_core::policy::{Grant, GrantId, presets};

    fn job<'a>(key: &'a str, payload: &'a str) -> NewJob<'a> {
        NewJob {
            kind: "diagnostic.wait",
            idempotency_key: key,
            payload_json: payload,
            owner_grant: None,
            max_attempts: 2,
            deadline_at_ms: None,
            now_ms: 1_000,
        }
    }

    #[test]
    fn grant_round_trip_and_revoke() {
        let db = LibraryDb::in_memory().unwrap();
        let stored = StoredGrant {
            grant: Grant {
                id: GrantId(uuid::Uuid::now_v7()),
                label: "desktop".into(),
                capabilities: presets::desktop(),
                expires_at_ms: None,
                revoked: false,
            },
            token_sha256: [7; 32],
            issued_by: None,
            created_at_ms: 5,
        };
        db.daemon_grants().insert(&stored).unwrap();
        assert_eq!(
            db.daemon_grants().by_token_hash(&[7; 32]).unwrap(),
            Some(stored.clone())
        );
        assert!(db.daemon_grants().revoke(stored.grant.id, 9).unwrap());
        assert!(db.daemon_grants().all().unwrap()[0].grant.revoked);
        assert!(
            db.daemon_grants()
                .by_token_hash(&[8; 32])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn idempotent_submit_lease_fencing_and_recovery() {
        let mut db = LibraryDb::in_memory().unwrap();
        let mut jobs = db.daemon_jobs();
        let (first, created) = jobs.submit(&job("k", r#"{"a":1}"#)).unwrap();
        assert!(created);
        let (again, created) = jobs.submit(&job("k", r#"{ "a" : 1 }"#)).unwrap();
        assert!(!created);
        assert_eq!(again.id, first.id);
        assert_eq!(
            jobs.submit(&job("k", r#"{"a":2}"#)),
            Err(cm_core::ServiceError::Conflict)
        );
        assert_eq!(jobs.active_count().unwrap(), 1);

        let claimed = jobs.claim("daemon-a", 2_000, 100).unwrap().unwrap();
        assert_eq!((claimed.attempts, claimed.lease_generation), (1, 1));
        assert!(jobs.claim("daemon-a", 2_000, 100).unwrap().is_none());
        // Daemon A dies; daemon B recovers its lease and leases generation 2.
        let recovered = jobs.recover("daemon-b", 2_010).unwrap();
        assert_eq!(recovered[0].state, "retry_wait");
        let second = jobs.claim("daemon-b", 2_010, 100).unwrap().unwrap();
        assert_eq!((second.attempts, second.lease_generation), (2, 2));
        // The stale generation can neither heartbeat nor commit.
        assert!(!jobs.heartbeat(first.id, 1, 2_020, 100, None).unwrap().held);
        let stale = Finish::Succeeded {
            result_json: "{}".into(),
            detected_committed: false,
        };
        assert!(!jobs.finish(first.id, 1, 2_020, stale.clone()).unwrap());
        // B dies too: attempt limit (2) reached -> failed with reason, not retried.
        let recovered = jobs.recover("daemon-c", 2_030).unwrap();
        assert_eq!(recovered[0].state, "failed");
        assert_eq!(
            recovered[0].error.as_deref(),
            Some("lease expired; attempt limit reached")
        );
        assert_eq!(jobs.active_count().unwrap(), 0);
        // Explicit retry resets attempts and requeues.
        let retried = jobs.retry(first.id, 2_040).unwrap();
        assert_eq!((retried.state.as_str(), retried.attempts), ("queued", 0));
        assert_eq!(
            jobs.retry(first.id, 2_040),
            Err(cm_core::ServiceError::Conflict)
        );
        let third = jobs.claim("daemon-c", 2_050, 100).unwrap().unwrap();
        assert!(
            jobs.finish(third.id, third.lease_generation, 2_060, stale)
                .unwrap()
        );
        assert_eq!(jobs.get(first.id).unwrap().unwrap().state, "succeeded");
    }

    #[test]
    fn backoff_deadline_and_cancel() {
        let mut db = LibraryDb::in_memory().unwrap();
        let mut jobs = db.daemon_jobs();
        let (a, _) = jobs.submit(&job("a", "{}")).unwrap();
        let claimed = jobs.claim("d", 1_000, 100).unwrap().unwrap();
        assert!(
            jobs.finish(
                a.id,
                claimed.lease_generation,
                1_010,
                Finish::Retry {
                    error: "busy".into(),
                    next_attempt_at_ms: 5_000
                }
            )
            .unwrap()
        );
        // retry_wait is active but not runnable before its backoff.
        assert_eq!(jobs.active_count().unwrap(), 1);
        assert!(jobs.claim("d", 4_999, 100).unwrap().is_none());
        assert_eq!(jobs.next_wakeup().unwrap(), Some(5_000));
        assert_eq!(jobs.request_cancel(a.id, 4_999).unwrap().state, "cancelled");

        let mut expiring = job("b", "{}");
        expiring.deadline_at_ms = Some(1_500);
        let (b, _) = jobs.submit(&expiring).unwrap();
        assert!(jobs.claim("d", 1_600, 100).unwrap().is_none());
        let b = jobs.get(b.id).unwrap().unwrap();
        assert_eq!(
            (b.state.as_str(), b.error.as_deref()),
            ("failed", Some("deadline exceeded"))
        );

        let (c, _) = jobs.submit(&job("c", "{}")).unwrap();
        let running = jobs.claim("d", 2_000, 100).unwrap().unwrap();
        assert_eq!(running.id, c.id);
        assert_eq!(jobs.request_cancel(c.id, 2_001).unwrap().state, "running");
        assert!(
            jobs.heartbeat(
                c.id,
                running.lease_generation,
                2_002,
                100,
                Some((1, Some(2), None))
            )
            .unwrap()
            .cancel_requested
        );
        assert!(
            jobs.finish(c.id, running.lease_generation, 2_003, Finish::Cancelled)
                .unwrap()
        );
        assert_eq!(jobs.active_count().unwrap(), 0);
    }
}
