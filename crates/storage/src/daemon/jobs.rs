//! Durable job table with leases and fencing generations (#25).
//!
//! Every state transition of a running job is conditional on `(state='running',
//! lease_generation)`, so a stale runner (lost lease, previous daemon) cannot commit.
//! Times are caller-supplied Unix milliseconds so policies are deterministic in tests.
use crate::error::service_error;
use cm_core::{ServiceError, policy::GrantId};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

pub const ACTIVE_STATES: &str = "('queued','running','retry_wait')";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub id: Uuid,
    pub kind: String,
    pub idempotency_key: String,
    pub payload_json: String,
    pub owner_grant: Option<GrantId>,
    /// One of queued|running|retry_wait|succeeded|failed|cancelled.
    pub state: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub progress_done: u64,
    pub progress_total: Option<u64>,
    pub progress_message: Option<String>,
    pub error: Option<String>,
    pub result_json: Option<String>,
    pub detected_committed: bool,
    pub cancel_requested: bool,
    pub lease_owner: Option<String>,
    pub lease_generation: u64,
    pub lease_expires_at_ms: Option<i64>,
    pub next_attempt_at_ms: Option<i64>,
    pub deadline_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    pub kind: &'a str,
    pub idempotency_key: &'a str,
    pub payload_json: &'a str,
    pub owner_grant: Option<GrantId>,
    pub max_attempts: u32,
    pub deadline_at_ms: Option<i64>,
    pub now_ms: i64,
}

/// Outcome of a fenced finish. `Retry` is only valid while attempts remain; the daemon policy
/// decides that before calling.
#[derive(Debug, Clone)]
pub enum Finish {
    Succeeded {
        result_json: String,
        detected_committed: bool,
    },
    Retry {
        error: String,
        next_attempt_at_ms: i64,
    },
    Failed {
        error: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    /// False when the lease was lost (generation/state changed): stop and do not commit.
    pub held: bool,
    pub cancel_requested: bool,
}

pub struct SqliteJobs<'a> {
    pub(crate) connection: &'a mut Connection,
}

const COLUMNS: &str = "SELECT id, kind, idempotency_key, payload_json, owner_grant, state, attempts, max_attempts, progress_done, progress_total, progress_message, error, result_json, detected_committed, cancel_requested, lease_owner, lease_generation, lease_expires_at_ms, next_attempt_at_ms, deadline_at_ms, created_at_ms, updated_at_ms FROM daemon_jobs";

impl SqliteJobs<'_> {
    /// Returns `(row, created)`. Same `(kind, key)` returns the existing job in any state;
    /// a different payload for the same key is a `Conflict`, never a second job.
    pub fn submit(&mut self, job: &NewJob<'_>) -> Result<(JobRow, bool), ServiceError> {
        if !(1..=20).contains(&job.max_attempts) {
            return Err(ServiceError::InvalidInput(
                "max_attempts must be 1..=20".into(),
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let existing = tx
            .query_row(
                &format!("{COLUMNS} WHERE kind = ?1 AND idempotency_key = ?2"),
                params![job.kind, job.idempotency_key],
                decode,
            )
            .optional()
            .map_err(service_error)?;
        if let Some(row) = existing {
            let same: bool = tx
                .query_row(
                    "SELECT json(?1) = json(?2)",
                    params![row.payload_json, job.payload_json],
                    |r| r.get(0),
                )
                .map_err(service_error)?;
            return if same {
                Ok((row, false))
            } else {
                Err(ServiceError::Conflict)
            };
        }
        let id = Uuid::now_v7();
        tx.execute(
            "INSERT INTO daemon_jobs(id, kind, idempotency_key, payload_json, owner_grant, state, max_attempts, deadline_at_ms, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, json(?4), ?5, 'queued', ?6, ?7, ?8, ?8)",
            params![
                id.to_string(),
                job.kind,
                job.idempotency_key,
                job.payload_json,
                job.owner_grant.map(|g| g.0.to_string()),
                job.max_attempts,
                job.deadline_at_ms,
                job.now_ms,
            ],
        )
        .map_err(service_error)?;
        let row = get(&tx, id)?.ok_or(ServiceError::Unavailable)?;
        tx.commit().map_err(service_error)?;
        Ok((row, true))
    }
    pub fn get(&self, id: Uuid) -> Result<Option<JobRow>, ServiceError> {
        get(self.connection, id)
    }
    /// Newest first, at most 500. `owner` restricts to one grant's jobs.
    pub fn list(
        &self,
        owner: Option<GrantId>,
        active_only: bool,
    ) -> Result<Vec<JobRow>, ServiceError> {
        let active = if active_only {
            format!(" AND state IN {ACTIVE_STATES}")
        } else {
            String::new()
        };
        self.connection
            .prepare(&format!(
                "{COLUMNS} WHERE (?1 IS NULL OR owner_grant = ?1){active} ORDER BY created_at_ms DESC, id DESC LIMIT 500"
            ))
            .map_err(service_error)?
            .query_map([owner.map(|g| g.0.to_string())], decode)
            .map_err(service_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(service_error)
    }
    pub fn active_count(&self) -> Result<u32, ServiceError> {
        self.connection
            .query_row(
                &format!("SELECT count(*) FROM daemon_jobs WHERE state IN {ACTIVE_STATES}"),
                [],
                |r| r.get(0),
            )
            .map_err(service_error)
    }
    /// Earliest pending retry time, so the runner can sleep until then.
    pub fn next_wakeup(&self) -> Result<Option<i64>, ServiceError> {
        self.connection
            .query_row(
                "SELECT min(coalesce(next_attempt_at_ms, 0)) FROM daemon_jobs WHERE state IN ('queued','retry_wait')",
                [],
                |r| r.get(0),
            )
            .map_err(service_error)
    }
    /// Fails due-but-expired jobs, then atomically leases the next runnable job:
    /// attempts+1, generation+1, lease until `now + lease_ms`.
    pub fn claim(
        &mut self,
        owner: &str,
        now_ms: i64,
        lease_ms: i64,
    ) -> Result<Option<JobRow>, ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        tx.execute(
            "UPDATE daemon_jobs SET state = 'failed', error = 'deadline exceeded', next_attempt_at_ms = NULL, updated_at_ms = ?1
             WHERE state IN ('queued','retry_wait') AND deadline_at_ms IS NOT NULL AND deadline_at_ms <= ?1",
            [now_ms],
        )
        .map_err(service_error)?;
        let id: Option<String> = tx
            .query_row(
                "SELECT id FROM daemon_jobs
                 WHERE state = 'queued' OR (state = 'retry_wait' AND coalesce(next_attempt_at_ms, 0) <= ?1)
                 ORDER BY coalesce(next_attempt_at_ms, created_at_ms), created_at_ms, id LIMIT 1",
                [now_ms],
                |r| r.get(0),
            )
            .optional()
            .map_err(service_error)?;
        let Some(id) = id else {
            tx.commit().map_err(service_error)?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE daemon_jobs SET state = 'running', attempts = attempts + 1, lease_owner = ?2,
                 lease_generation = lease_generation + 1, lease_expires_at_ms = ?3, next_attempt_at_ms = NULL,
                 progress_done = 0, progress_total = NULL, progress_message = NULL, updated_at_ms = ?4
             WHERE id = ?1",
            params![id, owner, now_ms + lease_ms, now_ms],
        )
        .map_err(service_error)?;
        let row = get(&tx, parse_id(&id)?)?;
        tx.commit().map_err(service_error)?;
        Ok(row)
    }
    /// Renews the lease (and optionally records progress) iff this generation still holds it.
    pub fn heartbeat(
        &mut self,
        id: Uuid,
        generation: u64,
        now_ms: i64,
        lease_ms: i64,
        progress: Option<(u64, Option<u64>, Option<&str>)>,
    ) -> Result<Heartbeat, ServiceError> {
        let (done, total, message, has_progress) = match progress {
            Some((d, t, m)) => (d as i64, t.map(|t| t as i64), m, true),
            None => (0, None, None, false),
        };
        let updated = self
            .connection
            .execute(
                "UPDATE daemon_jobs SET lease_expires_at_ms = ?3, updated_at_ms = ?4,
                     progress_done = CASE WHEN ?5 THEN ?6 ELSE progress_done END,
                     progress_total = CASE WHEN ?5 THEN ?7 ELSE progress_total END,
                     progress_message = CASE WHEN ?5 THEN ?8 ELSE progress_message END
                 WHERE id = ?1 AND state = 'running' AND lease_generation = ?2",
                params![
                    id.to_string(),
                    generation as i64,
                    now_ms + lease_ms,
                    now_ms,
                    has_progress,
                    done,
                    total,
                    message
                ],
            )
            .map_err(service_error)?;
        let cancel_requested = self.get(id)?.is_some_and(|row| row.cancel_requested);
        Ok(Heartbeat {
            held: updated == 1,
            cancel_requested,
        })
    }
    /// Fenced terminal/backoff transition. `Ok(false)`: the lease is no longer held and
    /// nothing was written (a newer attempt or recovery owns the job).
    pub fn finish(
        &mut self,
        id: Uuid,
        generation: u64,
        now_ms: i64,
        outcome: Finish,
    ) -> Result<bool, ServiceError> {
        let (state, error, result, detected, next): (
            &str,
            Option<String>,
            Option<String>,
            bool,
            Option<i64>,
        ) = match outcome {
            Finish::Succeeded {
                result_json,
                detected_committed,
            } => (
                "succeeded",
                None,
                Some(result_json),
                detected_committed,
                None,
            ),
            Finish::Retry {
                error,
                next_attempt_at_ms,
            } => (
                "retry_wait",
                Some(error),
                None,
                false,
                Some(next_attempt_at_ms),
            ),
            Finish::Failed { error } => ("failed", Some(error), None, false, None),
            Finish::Cancelled => ("cancelled", Some("cancelled".into()), None, false, None),
        };
        self.connection
            .execute(
                "UPDATE daemon_jobs SET state = ?3, error = ?4, result_json = ?5, detected_committed = ?6,
                     next_attempt_at_ms = ?7, lease_owner = NULL, lease_expires_at_ms = NULL, updated_at_ms = ?8
                 WHERE id = ?1 AND state = 'running' AND lease_generation = ?2",
                params![
                    id.to_string(),
                    generation as i64,
                    state,
                    error,
                    result,
                    detected,
                    next,
                    now_ms
                ],
            )
            .map(|n| n == 1)
            .map_err(service_error)
    }
    /// queued/retry_wait -> cancelled; running -> cancel_requested (runner finishes it).
    /// Terminal jobs are returned unchanged.
    pub fn request_cancel(&mut self, id: Uuid, now_ms: i64) -> Result<JobRow, ServiceError> {
        let id_text = id.to_string();
        self.connection
            .execute(
                "UPDATE daemon_jobs SET
                     state = CASE WHEN state IN ('queued','retry_wait') THEN 'cancelled' ELSE state END,
                     error = CASE WHEN state IN ('queued','retry_wait') THEN 'cancelled' ELSE error END,
                     next_attempt_at_ms = CASE WHEN state IN ('queued','retry_wait') THEN NULL ELSE next_attempt_at_ms END,
                     cancel_requested = CASE WHEN state = 'running' THEN 1 ELSE cancel_requested END,
                     updated_at_ms = ?2
                 WHERE id = ?1 AND state IN ('queued','retry_wait','running')",
                params![id_text, now_ms],
            )
            .map_err(service_error)?;
        self.get(id)?.ok_or(ServiceError::NotFound)
    }
    /// Explicit user retry of a failed/cancelled job: attempts reset, deadline cleared.
    pub fn retry(&mut self, id: Uuid, now_ms: i64) -> Result<JobRow, ServiceError> {
        let changed = self
            .connection
            .execute(
                "UPDATE daemon_jobs SET state = 'queued', attempts = 0, error = NULL, result_json = NULL,
                     detected_committed = 0, cancel_requested = 0, next_attempt_at_ms = NULL,
                     deadline_at_ms = NULL, progress_done = 0, progress_total = NULL,
                     progress_message = NULL, updated_at_ms = ?2
                 WHERE id = ?1 AND state IN ('failed','cancelled')",
                params![id.to_string(), now_ms],
            )
            .map_err(service_error)?;
        let row = self.get(id)?.ok_or(ServiceError::NotFound)?;
        if changed == 0 {
            return Err(ServiceError::Conflict);
        }
        Ok(row)
    }
    /// Reclaims leases that cannot be live: any `running` row whose owner is not `owner`
    /// (the exclusive daemon lock proves that owner is gone) or whose lease expired.
    /// Attempt limit reached -> failed with reason; cancel requested -> cancelled;
    /// otherwise retry_wait due now. Returns the recovered rows.
    pub fn recover(&mut self, owner: &str, now_ms: i64) -> Result<Vec<JobRow>, ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let ids: Vec<String> = tx
            .prepare(
                "SELECT id FROM daemon_jobs WHERE state = 'running' AND (lease_owner IS NOT ?1 OR lease_expires_at_ms <= ?2)",
            )
            .map_err(service_error)?
            .query_map(params![owner, now_ms], |r| r.get(0))
            .map_err(service_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(service_error)?;
        for id in &ids {
            tx.execute(
                "UPDATE daemon_jobs SET
                     state = CASE WHEN cancel_requested = 1 THEN 'cancelled'
                                  WHEN attempts >= max_attempts THEN 'failed'
                                  ELSE 'retry_wait' END,
                     error = CASE WHEN cancel_requested = 1 THEN 'cancelled'
                                  WHEN attempts >= max_attempts THEN 'lease expired; attempt limit reached'
                                  ELSE 'lease expired (previous run did not finish)' END,
                     next_attempt_at_ms = CASE WHEN cancel_requested = 0 AND attempts < max_attempts THEN ?2 ELSE NULL END,
                     lease_owner = NULL, lease_expires_at_ms = NULL, updated_at_ms = ?2
                 WHERE id = ?1 AND state = 'running'",
                params![id, now_ms],
            )
            .map_err(service_error)?;
        }
        let rows = ids
            .iter()
            .map(|id| get(&tx, parse_id(id)?)?.ok_or(ServiceError::Unavailable))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().map_err(service_error)?;
        Ok(rows)
    }
}

fn parse_id(id: &str) -> Result<Uuid, ServiceError> {
    Uuid::parse_str(id).map_err(|_| ServiceError::Unavailable)
}
fn get(connection: &Connection, id: Uuid) -> Result<Option<JobRow>, ServiceError> {
    connection
        .query_row(
            &format!("{COLUMNS} WHERE id = ?1"),
            [id.to_string()],
            decode,
        )
        .optional()
        .map_err(service_error)
}
fn decode(row: &Row<'_>) -> rusqlite::Result<JobRow> {
    let uuid = |index: usize, value: String| {
        Uuid::parse_str(&value).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
    };
    let unsigned = |index: usize, value: i64| {
        u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
    };
    let owner: Option<String> = row.get(4)?;
    let total: Option<i64> = row.get(9)?;
    Ok(JobRow {
        id: uuid(0, row.get(0)?)?,
        kind: row.get(1)?,
        idempotency_key: row.get(2)?,
        payload_json: row.get(3)?,
        owner_grant: owner.map(|o| uuid(4, o).map(GrantId)).transpose()?,
        state: row.get(5)?,
        attempts: row.get(6)?,
        max_attempts: row.get(7)?,
        progress_done: unsigned(8, row.get(8)?)?,
        progress_total: total.map(|t| unsigned(9, t)).transpose()?,
        progress_message: row.get(10)?,
        error: row.get(11)?,
        result_json: row.get(12)?,
        detected_committed: row.get(13)?,
        cancel_requested: row.get(14)?,
        lease_owner: row.get(15)?,
        lease_generation: unsigned(16, row.get(16)?)?,
        lease_expires_at_ms: row.get(17)?,
        next_attempt_at_ms: row.get(18)?,
        deadline_at_ms: row.get(19)?,
        created_at_ms: row.get(20)?,
        updated_at_ms: row.get(21)?,
    })
}
