//! Daemon-only tables (#25): grants and durable jobs. Only the lock-holding daemon
//! composition root should call these; GUI/CLI/MCP never open storage directly.
mod api_settings;
mod grants;
mod jobs;
pub use api_settings::SqliteApiSettings;
pub use grants::{SqliteGrants, StoredGrant};
pub use jobs::{Finish, Heartbeat, JobRow, NewJob, SqliteJobs};

impl crate::LibraryDb {
    pub fn daemon_grants(&self) -> SqliteGrants<'_> {
        SqliteGrants {
            connection: &self.connection,
        }
    }
    /// #26 local API listener settings (singleton row).
    pub fn api_settings(&self) -> SqliteApiSettings<'_> {
        SqliteApiSettings {
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
    use cm_core::policy::{ClientKind, Grant, GrantId, Scope, presets};

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
                elevated_until_ms: None,
            },
            token_sha256: [7; 32],
            issued_by: None,
            created_at_ms: 5,
            client_kind: ClientKind::Desktop,
            scopes: None,
            last_used_at_ms: None,
            replaces: None,
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
    fn token_metadata_scope_change_elevation_and_touch_round_trip() {
        let db = LibraryDb::in_memory().unwrap();
        let read: cm_core::policy::ScopeSet = [Scope::PapersRead].into();
        let mut stored = StoredGrant {
            grant: Grant {
                id: GrantId(uuid::Uuid::now_v7()),
                label: "mcp profile".into(),
                capabilities: cm_core::policy::scopes::capabilities(&read),
                expires_at_ms: Some(100),
                revoked: false,
                elevated_until_ms: None,
            },
            token_sha256: [3; 32],
            issued_by: None,
            created_at_ms: 1,
            client_kind: ClientKind::Mcp,
            scopes: Some(read),
            last_used_at_ms: None,
            replaces: None,
        };
        db.daemon_grants().insert(&stored).unwrap();
        assert_eq!(db.daemon_grants().all().unwrap(), vec![stored.clone()]);
        let write: cm_core::policy::ScopeSet = [Scope::PapersRead, Scope::PapersWrite].into();
        let caps = cm_core::policy::scopes::capabilities(&write);
        assert!(
            db.daemon_grants()
                .set_access(stored.grant.id, &caps, Some(&write), Some(50))
                .unwrap()
        );
        assert!(
            db.daemon_grants()
                .set_elevation(stored.grant.id, Some(70))
                .unwrap()
        );
        db.daemon_grants().touch(stored.grant.id, 9).unwrap();
        db.daemon_grants().touch(stored.grant.id, 4).unwrap();
        stored.grant.capabilities = caps;
        stored.scopes = Some(write);
        stored.grant.expires_at_ms = Some(50);
        stored.grant.elevated_until_ms = Some(70);
        stored.last_used_at_ms = Some(9);
        assert_eq!(
            db.daemon_grants().by_id(stored.grant.id).unwrap(),
            Some(stored.clone())
        );
        // Revoked grants cannot be widened or elevated afterwards.
        db.daemon_grants().revoke(stored.grant.id, 10).unwrap();
        assert!(
            !db.daemon_grants()
                .set_access(stored.grant.id, &presets::desktop(), None, None)
                .unwrap()
        );
        assert!(
            !db.daemon_grants()
                .set_elevation(stored.grant.id, Some(99))
                .unwrap()
        );
    }

    #[test]
    fn api_settings_default_off_round_trip_and_constraints() {
        use cm_core::policy::api::{ApiSettings, BindAddress};
        let db = LibraryDb::in_memory().unwrap();
        assert_eq!(db.api_settings().load().unwrap(), ApiSettings::default());
        let settings = ApiSettings {
            enabled: true,
            bind: BindAddress::LoopbackV6,
            port: 40000,
            export_dirs: vec!["/tmp/exports".into()],
        };
        db.api_settings().save(&settings, 5).unwrap();
        assert_eq!(db.api_settings().load().unwrap(), settings);
        // The table itself refuses non-loopback addresses and privileged ports.
        for sql in [
            "UPDATE api_settings SET bind_address = '0.0.0.0'",
            "UPDATE api_settings SET port = 80",
            "INSERT INTO api_settings(id) VALUES (2)",
        ] {
            assert!(db.connection.execute(sql, []).is_err(), "{sql}");
        }
        let invalid = ApiSettings {
            port: 22,
            ..ApiSettings::default()
        };
        assert!(db.api_settings().save(&invalid, 6).is_err());
        assert_eq!(db.api_settings().load().unwrap(), settings);
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

    fn grant(db: &LibraryDb, hash: u8) -> GrantId {
        let stored = StoredGrant {
            grant: Grant {
                id: GrantId(uuid::Uuid::now_v7()),
                label: format!("grant {hash}"),
                capabilities: presets::desktop(),
                expires_at_ms: None,
                revoked: false,
                elevated_until_ms: None,
            },
            token_sha256: [hash; 32],
            issued_by: None,
            created_at_ms: 1,
            client_kind: ClientKind::Unspecified,
            scopes: None,
            last_used_at_ms: None,
            replaces: None,
        };
        db.daemon_grants().insert(&stored).unwrap();
        stored.grant.id
    }

    #[test]
    fn idempotency_is_scoped_to_the_owner_grant() {
        let mut db = LibraryDb::in_memory().unwrap();
        let (a, b) = (grant(&db, 1), grant(&db, 2));
        let mut jobs = db.daemon_jobs();
        let owned = |owner, payload| NewJob {
            owner_grant: Some(owner),
            ..job("shared-key", payload)
        };
        let (first, created) = jobs.submit(&owned(a, r#"{"a":1}"#)).unwrap();
        assert!(created);
        // Another grant with the same key (even another payload) gets its own job.
        let (other, created) = jobs.submit(&owned(b, r#"{"b":2}"#)).unwrap();
        assert!(created);
        assert_ne!(other.id, first.id);
        assert_eq!(other.owner_grant, Some(b));
        // The same owner still deduplicates / conflicts as before.
        assert_eq!(jobs.submit(&owned(a, r#"{"a":1}"#)).unwrap().0.id, first.id);
        assert_eq!(
            jobs.submit(&owned(a, r#"{"a":9}"#)),
            Err(cm_core::ServiceError::Conflict)
        );
        // Daemon-internal (ownerless) jobs form their own scope.
        let (internal, created) = jobs.submit(&job("shared-key", "{}")).unwrap();
        assert!(created);
        assert_eq!(
            jobs.submit(&job("shared-key", "{}")).unwrap().0.id,
            internal.id
        );
        assert_eq!(jobs.active_count().unwrap(), 3);
    }

    #[test]
    fn revoking_an_owner_fails_its_waiting_jobs_and_flags_running_ones() {
        let mut db = LibraryDb::in_memory().unwrap();
        let (a, b) = (grant(&db, 1), grant(&db, 2));
        let mut jobs = db.daemon_jobs();
        let owned = |owner, key| NewJob {
            owner_grant: Some(owner),
            ..job(key, "{}")
        };
        let (running, _) = jobs.submit(&owned(a, "run")).unwrap();
        let claimed = jobs.claim("d", 1_000, 100).unwrap().unwrap();
        assert_eq!(claimed.id, running.id);
        let (queued, _) = jobs.submit(&owned(a, "queued")).unwrap();
        let (unrelated, _) = jobs.submit(&owned(b, "queued")).unwrap();
        let affected = jobs
            .fail_owner_active(a, "owner grant revoked", 1_010)
            .unwrap();
        assert_eq!(affected.len(), 2);
        let queued = jobs.get(queued.id).unwrap().unwrap();
        assert_eq!(
            (queued.state.as_str(), queued.error.as_deref()),
            ("failed", Some("owner grant revoked"))
        );
        let running = jobs.get(running.id).unwrap().unwrap();
        assert_eq!(running.state, "running");
        assert!(running.cancel_requested);
        assert_eq!(jobs.get(unrelated.id).unwrap().unwrap().state, "queued");
    }
}
