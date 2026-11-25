//! Durable job runner (#25). One runner thread executes one job at a time on its own
//! connection; job-table control operations use the shared control connection.
//!
//! Policy:
//! - queued/running/retry_wait are active and keep the daemon alive.
//! - Each claim increments attempts and the fencing generation and leases the job.
//!   A background heartbeat renews the lease every `lease/3` while the handler runs and
//!   persists the latest progress with it. Live progress and cancel signals are in memory
//!   between beats: every committed write to library.sqlite invalidates open #33 import
//!   previews (conservative data_version check), so job bookkeeping writes stay sparse.
//! - Retryable failure: backoff `base * 2^(attempt-1)` capped at `max_backoff`
//!   (retry_wait); attempt limit reached -> failed "attempt limit N reached: <reason>";
//!   a retry that would start after the deadline -> failed "deadline exceeded ...".
//! - Startup recovery reclaims every running lease of a previous daemon (the exclusive
//!   library lock proves it is gone).
//! - Re-execution is possible after a crash (at-least-once). Handlers make effects
//!   idempotent and report `AlreadyCommitted` when they find their earlier result.
//! - Jobs never outlive their owner's authority: the owner grant must be live and still hold
//!   the kind's capabilities when the job is claimed and at every heartbeat; otherwise the job
//!   fails ("owner grant revoked"/"expired"/...). Revoking a grant fails its queued/retry_wait
//!   jobs at once and stops its running job (`JobEngine::owner_revoked`).
//! - A running attempt stops (cooperatively, at its next progress report or heartbeat) and
//!   fails when the job's `deadline_at_ms` passes or it exceeds the kind's
//!   `JobHandler::max_run`.
mod capture;
mod handlers;
pub use capture::{CapturePdfFetchHandler, sweep_stale_downloads};
pub use handlers::{AttachmentImportHandler, DiagnosticWaitHandler};

use crate::{ipc::auth::now_ms, lifecycle::Activity};
use cm_core::{
    ServiceError,
    metadata_resolve::Cancellation,
    policy::{self, Capability, CapabilitySet, Denial, GrantId, Operation},
};
use cm_protocol::{
    ipc::WireError,
    jobs::{JobId, JobProgress, JobState, JobStatus, JobSubmit},
};
use cm_storage::{
    LibraryDb, LibraryDirectory,
    daemon::{Finish, JobRow, NewJob},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// Default per-attempt run-time limit (`JobHandler::max_run`).
pub const DEFAULT_MAX_RUN: Duration = Duration::from_secs(60 * 60);
/// Error recorded when a job's owner grant was revoked (queued, waiting or running).
pub const OWNER_REVOKED: &str = "owner grant revoked";

#[derive(Debug, Clone)]
pub struct JobPolicy {
    pub lease: Duration,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    pub default_max_attempts: u32,
    /// Upper bound on how long the runner sleeps before re-checking the table.
    pub poll: Duration,
}
impl Default for JobPolicy {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(30),
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            default_max_attempts: 3,
            poll: Duration::from_millis(500),
        }
    }
}
impl JobPolicy {
    pub fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1u32
            .checked_shl(attempt.saturating_sub(1).min(20))
            .unwrap_or(u32::MAX);
        self.base_backoff
            .saturating_mul(factor)
            .min(self.max_backoff)
    }
}

pub enum Outcome {
    Succeeded(Value),
    /// The effect was already durably committed by an earlier attempt; no new write.
    AlreadyCommitted(Value),
    Retryable(String),
    Fatal(String),
    Cancelled,
}

pub struct JobContext<'a> {
    pub job: JobId,
    pub attempt: u32,
    pub payload: &'a Value,
    pub idempotency_key: &'a str,
    pub db: &'a mut LibraryDb,
    pub directory: &'a LibraryDirectory,
    pub cancel: &'a Cancellation,
    progress: &'a dyn Fn(JobProgress) -> bool,
}
impl<'a> JobContext<'a> {
    /// Records progress and renews the lease. `false`: stop (cancelled or lease lost).
    pub fn progress(&self, progress: JobProgress) -> bool {
        (self.progress)(progress) && !self.cancel.is_cancelled()
    }
    /// The same reporter detached from `self`, so a handler can hold it while it borrows `db`.
    pub fn reporter<'b>(&self) -> impl Fn(JobProgress) -> bool + 'b
    where
        'a: 'b,
    {
        let (progress, cancel) = (self.progress, self.cancel);
        move |p| progress(p) && !cancel.is_cancelled()
    }
}

pub trait JobHandler: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    /// Capabilities the submitting grant must hold in addition to `JobsControl`.
    fn required(&self) -> &'static [Capability];
    fn validate(&self, payload: &Value) -> Result<(), ServiceError>;
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome;
    /// Run-time limit of one attempt. Cancellation is cooperative: the handler observes it
    /// through `JobContext::progress`/`cancel`. Exceeding it fails the job (no retry).
    fn max_run(&self) -> Duration {
        DEFAULT_MAX_RUN
    }
    /// Whether clients may create this kind through `JobCommand::Submit`. Kinds whose
    /// payload must come from a daemon-validated operation (e.g. #22 capture PDF fetch,
    /// bound to a reviewed preview) return false and are created via `submit_internal`.
    fn ipc_submittable(&self) -> bool {
        true
    }
}

/// Current authority of a job's owner grant. The runner consults it when it claims a job and
/// on every heartbeat, so revoked/expired/narrowed grants stop their queued and running work.
pub trait OwnerAuthority: Send + Sync + 'static {
    /// The owner grant's current capabilities, or why it can no longer act.
    fn owner_capabilities(&self, owner: GrantId, now_ms: i64) -> Result<CapabilitySet, Denial>;
}

/// Fallback authority read from the stored grant rows (used by `JobEngine::start`).
struct StoredOwners(Arc<Mutex<LibraryDb>>);
impl OwnerAuthority for StoredOwners {
    fn owner_capabilities(&self, owner: GrantId, now_ms: i64) -> Result<CapabilitySet, Denial> {
        let stored = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .by_id(owner)
            .map_err(|_| Denial::Unauthenticated)?;
        policy::check_live(stored.as_ref().map(|s| &s.grant), now_ms)
            .map(|g| g.capabilities.clone())
    }
}

/// Why the owner may no longer run a job of a kind needing `required`; `None` = allowed.
/// Ownerless jobs are daemon-internal and run with daemon authority.
fn owner_denial(
    owners: &dyn OwnerAuthority,
    owner: Option<GrantId>,
    required: &[Capability],
    now_ms: i64,
) -> Option<String> {
    let owner = owner?;
    match owners.owner_capabilities(owner, now_ms) {
        Ok(held) if required.iter().all(|c| held.contains(c)) => None,
        Ok(_) => Some("owner grant no longer holds the capabilities this job kind requires".into()),
        Err(Denial::Revoked) => Some(OWNER_REVOKED.into()),
        Err(Denial::Expired) => Some("owner grant expired".into()),
        Err(_) => Some("owner grant no longer exists".into()),
    }
}

pub fn default_handlers() -> Vec<Arc<dyn JobHandler>> {
    vec![
        Arc::new(AttachmentImportHandler),
        Arc::new(DiagnosticWaitHandler),
        Arc::new(crate::fulltext::SearchRebuildHandler),
    ]
}

/// #18: submit one ownerless `fulltext.extract {scope: pending}` job when PDF attachments
/// await extraction and no extraction job is active. The idempotency key names the pending
/// set and the work done/attempted on it (`Unfinished::key`), so a set whose job failed
/// without progress is not resubmitted in a loop (explicit retry remains).
fn ensure_extraction(control: &Mutex<LibraryDb>, handlers: &Handlers) -> Option<JobRow> {
    use cm_protocol::jobs::kinds::FULLTEXT_EXTRACT;
    if !handlers.contains_key(FULLTEXT_EXTRACT) {
        return None;
    }
    let mut db = control.lock().unwrap_or_else(|e| e.into_inner());
    let key = db.fulltext_reader().unfinished().ok()?.key()?;
    let mut jobs = db.daemon_jobs();
    if jobs
        .list(None, true)
        .ok()?
        .iter()
        .any(|j| j.kind == FULLTEXT_EXTRACT)
    {
        return None;
    }
    jobs.submit(&NewJob {
        kind: FULLTEXT_EXTRACT,
        idempotency_key: &key,
        payload_json: AUTO_EXTRACT_PAYLOAD,
        owner_grant: None,
        max_attempts: 3,
        deadline_at_ms: None,
        now_ms: now_ms(),
    })
    .ok()
    .map(|(row, _)| row)
}
const AUTO_EXTRACT_PAYLOAD: &str = "{\"scope\":{\"kind\":\"pending\"},\"force\":false}";

type Handlers = Arc<HashMap<&'static str, Arc<dyn JobHandler>>>;

/// In-memory state of the running job, shared by the engine (readers) and the runner.
#[derive(Default)]
struct Live {
    progress: Mutex<HashMap<uuid::Uuid, JobProgress>>,
    cancel: Mutex<std::collections::HashSet<uuid::Uuid>>,
}
impl Live {
    fn overlay(&self, mut status: JobStatus) -> JobStatus {
        if status.state == JobState::Running
            && let Some(p) = self
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&status.id.0)
        {
            status.progress = p.clone();
        }
        status
    }
}

pub struct JobEngine {
    control: Arc<Mutex<LibraryDb>>,
    handlers: Handlers,
    policy: JobPolicy,
    wake: mpsc::Sender<()>,
    stop: Arc<AtomicBool>,
    runner: Mutex<Option<thread::JoinHandle<()>>>,
    live: Arc<Live>,
}

impl JobEngine {
    /// Recovers stale leases synchronously, then starts the runner. Owner grants are checked
    /// against the stored grant rows; the daemon uses `start_with_owners` with its live
    /// grant registry instead.
    pub fn start(
        directory: LibraryDirectory,
        control: Arc<Mutex<LibraryDb>>,
        handlers: Vec<Arc<dyn JobHandler>>,
        policy: JobPolicy,
        activity: Arc<Activity>,
    ) -> Result<Self, ServiceError> {
        let owners = Arc::new(StoredOwners(control.clone()));
        Self::start_with_owners(directory, control, handlers, policy, activity, owners)
    }
    /// As `start`, with `owners` as the authority on job owner grants.
    pub fn start_with_owners(
        directory: LibraryDirectory,
        control: Arc<Mutex<LibraryDb>>,
        handlers: Vec<Arc<dyn JobHandler>>,
        policy: JobPolicy,
        activity: Arc<Activity>,
        owners: Arc<dyn OwnerAuthority>,
    ) -> Result<Self, ServiceError> {
        let owner = format!("papersd-{}-{}", std::process::id(), uuid::Uuid::now_v7());
        let handlers: Handlers = Arc::new(handlers.into_iter().map(|h| (h.kind(), h)).collect());
        {
            let mut db = control.lock().unwrap_or_else(|e| e.into_inner());
            let mut jobs = db.daemon_jobs();
            jobs.recover(&owner, now_ms())?;
            activity.set_active_jobs(jobs.active_count()? as usize);
        }
        let work = LibraryDb::open(&directory)
            .map_err(|e| ServiceError::InvalidInput(format!("cannot open job connection: {e}")))?;
        let (wake, woken) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(Live::default());
        let runner = Runner {
            live: live.clone(),
            owner,
            control: control.clone(),
            handlers: handlers.clone(),
            policy: policy.clone(),
            directory,
            work,
            activity,
            stop: stop.clone(),
            owners,
        };
        let runner = thread::Builder::new()
            .name("job-runner".into())
            .spawn(move || runner.run(woken))
            .map_err(|_| ServiceError::Unavailable)?;
        Ok(Self {
            control,
            handlers,
            policy,
            wake,
            stop,
            runner: Mutex::new(Some(runner)),
            live,
        })
    }
    fn db(&self) -> std::sync::MutexGuard<'_, LibraryDb> {
        self.control.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// `owner`: submitting grant. `held`: its effective capabilities. Client path
    /// (`JobCommand::Submit`): kinds that are not `ipc_submittable` are refused.
    pub fn submit(
        &self,
        owner: GrantId,
        held: &CapabilitySet,
        submit: JobSubmit,
    ) -> Result<JobStatus, WireError> {
        self.submit_as(owner, held, submit, false)
    }
    /// Daemon-internal submission for a payload the daemon itself derived from an
    /// authorized operation (same capability, idempotency and validation rules).
    pub fn submit_internal(
        &self,
        owner: GrantId,
        held: &CapabilitySet,
        submit: JobSubmit,
    ) -> Result<JobStatus, WireError> {
        self.submit_as(owner, held, submit, true)
    }
    fn submit_as(
        &self,
        owner: GrantId,
        held: &CapabilitySet,
        submit: JobSubmit,
        internal: bool,
    ) -> Result<JobStatus, WireError> {
        let handler = self
            .handlers
            .get(submit.kind.as_str())
            .ok_or_else(|| invalid("unknown job kind"))?;
        if !internal && !handler.ipc_submittable() {
            return Err(WireError::Denied(Denial::Forbidden(Operation::JobSubmit)));
        }
        if !handler.required().iter().all(|c| held.contains(c)) {
            return Err(WireError::Denied(Denial::Forbidden(Operation::JobSubmit)));
        }
        if submit.idempotency_key.is_empty()
            || submit.idempotency_key.len() > 512
            || submit.idempotency_key.chars().any(char::is_control)
        {
            return Err(invalid("idempotency_key must be 1..=512 printable bytes"));
        }
        let max_attempts = submit
            .max_attempts
            .unwrap_or(self.policy.default_max_attempts);
        if !(1..=20).contains(&max_attempts) {
            return Err(invalid("max_attempts must be 1..=20"));
        }
        handler.validate(&submit.payload)?;
        let payload = serde_json::to_string(&submit.payload)
            .map_err(|e| WireError::Service(ServiceError::InvalidInput(e.to_string())))?;
        let (row, _) = self.db().daemon_jobs().submit(&NewJob {
            kind: handler.kind(),
            idempotency_key: &submit.idempotency_key,
            payload_json: &payload,
            owner_grant: Some(owner),
            max_attempts,
            deadline_at_ms: submit.deadline_at_ms,
            now_ms: now_ms(),
        })?;
        let _ = self.wake.send(());
        status(row)
    }
    /// `owner = None` sees every job (GrantAdmin); otherwise only that grant's jobs.
    pub fn get(&self, id: JobId, owner: Option<GrantId>) -> Result<JobStatus, WireError> {
        let row = self
            .db()
            .daemon_jobs()
            .get(id.0)?
            .filter(|row| owner.is_none() || row.owner_grant == owner)
            .ok_or(WireError::Service(ServiceError::NotFound))?;
        status(row).map(|s| self.live.overlay(s))
    }
    pub fn list(
        &self,
        owner: Option<GrantId>,
        active_only: bool,
    ) -> Result<Vec<JobStatus>, WireError> {
        self.db()
            .daemon_jobs()
            .list(owner, active_only)?
            .into_iter()
            .map(|row| status(row).map(|s| self.live.overlay(s)))
            .collect()
    }
    /// Cancel only stops or prevents effects, so it needs `JobsControl` (and job visibility)
    /// but not the kind's capabilities.
    pub fn cancel(&self, id: JobId, owner: Option<GrantId>) -> Result<JobStatus, WireError> {
        self.get(id, owner)?;
        let row = self.db().daemon_jobs().request_cancel(id.0, now_ms())?;
        if row.state == "running" {
            self.live
                .cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.0);
        }
        let _ = self.wake.send(());
        status(row)
    }
    /// Explicit retry re-runs the job's effects, so the retrying session's effective
    /// capabilities (`held`) must cover the kind's requirement, exactly as for submit.
    pub fn retry(
        &self,
        id: JobId,
        owner: Option<GrantId>,
        held: &CapabilitySet,
    ) -> Result<JobStatus, WireError> {
        let current = self.get(id, owner)?;
        let handler = self
            .handlers
            .get(current.kind.as_str())
            .ok_or_else(|| invalid("unknown job kind"))?;
        if !handler.required().iter().all(|c| held.contains(c)) {
            return Err(WireError::Denied(Denial::Forbidden(Operation::JobRetry)));
        }
        let row = self.db().daemon_jobs().retry(id.0, now_ms())?;
        let _ = self.wake.send(());
        status(row)
    }
    /// Submits automatic extraction for pending PDF attachments (no-op without an extractor).
    pub fn ensure_extraction(&self) {
        if ensure_extraction(&self.control, &self.handlers).is_some() {
            let _ = self.wake.send(());
        }
    }
    /// Daemon-initiated (ownerless) job, e.g. `search.rebuild` after detected index damage.
    /// Visible to GrantAdmin sessions; idempotent on `(kind, key)`.
    pub fn submit_system(
        &self,
        kind: &str,
        key: &str,
        payload: Value,
    ) -> Result<JobStatus, WireError> {
        let handler = self
            .handlers
            .get(kind)
            .ok_or_else(|| invalid("unknown job kind"))?;
        handler.validate(&payload)?;
        let payload = payload.to_string();
        let (row, _) = self.db().daemon_jobs().submit(&NewJob {
            kind: handler.kind(),
            idempotency_key: key,
            payload_json: &payload,
            owner_grant: None,
            max_attempts: self.policy.default_max_attempts,
            deadline_at_ms: None,
            now_ms: now_ms(),
        })?;
        let _ = self.wake.send(());
        status(row)
    }
    /// The grant was revoked: its queued/retry_wait jobs fail with `OWNER_REVOKED` now and
    /// its running job is told to stop (the runner records the same reason). Returns how many
    /// active jobs were affected.
    pub fn owner_revoked(&self, owner: GrantId) -> Result<usize, WireError> {
        let rows = self
            .db()
            .daemon_jobs()
            .fail_owner_active(owner, OWNER_REVOKED, now_ms())?;
        {
            let mut cancel = self.live.cancel.lock().unwrap_or_else(|e| e.into_inner());
            for row in rows.iter().filter(|r| r.state == "running") {
                cancel.insert(row.id);
            }
        }
        let _ = self.wake.send(());
        Ok(rows.len())
    }
    /// Stops after the current job (its lease is recovered on next start if unfinished).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        let runner = self.runner.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(runner) = runner {
            let _ = runner.join();
        }
    }
}
impl Drop for JobEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
    }
}

fn invalid(reason: &str) -> WireError {
    WireError::Service(ServiceError::InvalidInput(reason.into()))
}

pub fn status(row: JobRow) -> Result<JobStatus, WireError> {
    Ok(JobStatus {
        id: JobId(row.id),
        state: JobState::parse(&row.state).ok_or(WireError::Unavailable)?,
        kind: row.kind,
        idempotency_key: row.idempotency_key,
        attempts: row.attempts,
        max_attempts: row.max_attempts,
        progress: JobProgress {
            done: row.progress_done,
            total: row.progress_total,
            message: row.progress_message,
        },
        error: row.error,
        result: row
            .result_json
            .as_deref()
            .and_then(|r| serde_json::from_str(r).ok()),
        detected_committed: row.detected_committed,
        cancel_requested: row.cancel_requested,
        lease_generation: row.lease_generation,
        next_attempt_at_ms: row.next_attempt_at_ms,
        deadline_at_ms: row.deadline_at_ms,
        created_at_ms: row.created_at_ms,
        updated_at_ms: row.updated_at_ms,
    })
}

struct Runner {
    live: Arc<Live>,
    owner: String,
    control: Arc<Mutex<LibraryDb>>,
    handlers: Handlers,
    policy: JobPolicy,
    directory: LibraryDirectory,
    work: LibraryDb,
    activity: Arc<Activity>,
    stop: Arc<AtomicBool>,
    owners: Arc<dyn OwnerAuthority>,
}
impl Runner {
    fn db(&self) -> std::sync::MutexGuard<'_, LibraryDb> {
        self.control.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn refresh_active(&self) {
        if let Ok(count) = self.db().daemon_jobs().active_count() {
            self.activity.set_active_jobs(count as usize);
        }
    }
    fn run(mut self, woken: mpsc::Receiver<()>) {
        let lease_ms = self.policy.lease.as_millis() as i64;
        while !self.stop.load(Ordering::SeqCst) {
            let claimed = self
                .db()
                .daemon_jobs()
                .claim(&self.owner, now_ms(), lease_ms);
            self.refresh_active();
            match claimed {
                Ok(Some(row)) => {
                    let (id, kind) = (row.id, row.kind.clone());
                    self.execute(row);
                    // New attachments (an attachment import or a #22 capture PDF fetch)
                    // leave pending work. So does an extraction run that
                    // ended on its budget, deferred an attachment, or ran while further PDFs
                    // were attached (`ensure_extraction` is a no-op while it is active): when
                    // it ends, the still-unfinished set gets a job. The idempotency key makes
                    // this a no-op when the run made no progress; a cancelled run is not
                    // resubmitted (the user stopped it).
                    let resume = kind == cm_protocol::jobs::kinds::ATTACHMENT_IMPORT
                        || kind == cm_protocol::jobs::kinds::CAPTURE_PDF_FETCH
                        || (kind == cm_protocol::jobs::kinds::FULLTEXT_EXTRACT
                            && self
                                .db()
                                .daemon_jobs()
                                .get(id)
                                .ok()
                                .flatten()
                                .is_some_and(|j| j.state != "cancelled"));
                    if resume {
                        ensure_extraction(&self.control, &self.handlers);
                    }
                    self.refresh_active();
                }
                Ok(None) | Err(_) => {
                    let wait = self
                        .db()
                        .daemon_jobs()
                        .next_wakeup()
                        .ok()
                        .flatten()
                        .map(|at| Duration::from_millis((at - now_ms()).max(1) as u64))
                        .unwrap_or(self.policy.poll)
                        .min(self.policy.poll);
                    let _ = woken.recv_timeout(wait);
                    while woken.try_recv().is_ok() {}
                }
            }
        }
    }
    fn execute(&mut self, row: JobRow) {
        let lease_ms = self.policy.lease.as_millis() as i64;
        let generation = row.lease_generation;
        let id = row.id;
        let Some(handler) = self.handlers.get(row.kind.as_str()).cloned() else {
            self.finish(
                &row,
                Finish::Failed {
                    error: "unknown job kind".into(),
                },
            );
            return;
        };
        let payload: Value = match serde_json::from_str(&row.payload_json) {
            Ok(v) => v,
            Err(_) => {
                self.finish(
                    &row,
                    Finish::Failed {
                        error: "unreadable payload".into(),
                    },
                );
                return;
            }
        };
        if let Some(reason) = owner_denial(
            self.owners.as_ref(),
            row.owner_grant,
            handler.required(),
            now_ms(),
        ) {
            self.finish(&row, Finish::Failed { error: reason });
            return;
        }
        // Wall-clock end of this attempt: the job deadline or the kind's run-time limit.
        let started = Instant::now();
        let max_run = handler.max_run();
        let deadline_at_ms = row.deadline_at_ms;
        let stop_reason: Mutex<Option<String>> = Mutex::new(None);
        let stop_with = |reason: String| {
            stop_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert(reason);
        };
        let out_of_time = || -> Option<String> {
            if deadline_at_ms.is_some_and(|at| now_ms() >= at) {
                Some("deadline exceeded while running".into())
            } else if started.elapsed() >= max_run {
                Some(format!(
                    "run time limit of {} ms exceeded",
                    max_run.as_millis()
                ))
            } else {
                None
            }
        };
        let owners = self.owners.clone();
        let owner = row.owner_grant;
        let required = handler.required();
        let cancel = Cancellation::default();
        let lost = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        let control = self.control.clone();
        let live = self.live.clone();
        let cancel_signalled = || {
            live.cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&id)
        };
        // Lease renewal + sparse progress persistence; also notices DB-side cancel requests,
        // owner revocation/expiry and the attempt's deadline.
        let beat = || -> bool {
            let stop =
                owner_denial(owners.as_ref(), owner, required, now_ms()).or_else(out_of_time);
            if let Some(reason) = stop {
                stop_with(reason);
                cancel.cancel();
                return false;
            }
            let progress = live
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id)
                .cloned();
            let beat = control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .daemon_jobs()
                .heartbeat(
                    id,
                    generation,
                    now_ms(),
                    lease_ms,
                    progress
                        .as_ref()
                        .map(|p| (p.done, p.total, p.message.as_deref())),
                );
            match beat {
                Ok(b) if !b.held => {
                    lost.store(true, Ordering::SeqCst);
                    cancel.cancel();
                    false
                }
                Ok(b) if b.cancel_requested => {
                    cancel.cancel();
                    false
                }
                // A transient DB error does not stop the job; the next beat retries.
                Ok(_) | Err(_) => true,
            }
        };
        let interval = (self.policy.lease / 3).max(Duration::from_millis(10));
        let outcome = thread::scope(|scope| {
            let heartbeat = scope.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    thread::park_timeout(interval);
                    if done.load(Ordering::SeqCst) || !beat() {
                        break;
                    }
                }
            });
            let progress = |p: JobProgress| -> bool {
                live.progress
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, p);
                if cancel_signalled() {
                    cancel.cancel();
                }
                if let Some(reason) = out_of_time() {
                    stop_with(reason);
                    cancel.cancel();
                }
                !cancel.is_cancelled() && !lost.load(Ordering::SeqCst)
            };
            let mut ctx = JobContext {
                job: JobId(id),
                attempt: row.attempts,
                payload: &payload,
                idempotency_key: &row.idempotency_key,
                db: &mut self.work,
                directory: &self.directory,
                cancel: &cancel,
                progress: &progress,
            };
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.run(&mut ctx)))
                    .unwrap_or_else(|_| Outcome::Fatal("job handler panicked".into()));
            done.store(true, Ordering::SeqCst);
            heartbeat.thread().unpark();
            outcome
        });
        self.live
            .progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        self.live
            .cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        if lost.load(Ordering::SeqCst) {
            // Another owner (or recovery) holds the job: never commit a stale generation.
            return;
        }
        let cancel_requested = cancel.is_cancelled();
        let now = now_ms();
        // A committed effect is reported truthfully. Otherwise a lost owner authority or an
        // exhausted run time is the recorded reason (never retried).
        let stopped = if matches!(
            outcome,
            Outcome::Succeeded(_) | Outcome::AlreadyCommitted(_)
        ) {
            None
        } else {
            owner_denial(
                self.owners.as_ref(),
                row.owner_grant,
                handler.required(),
                now,
            )
            .or_else(|| stop_reason.lock().unwrap_or_else(|e| e.into_inner()).take())
        };
        let finish = match (outcome, stopped) {
            (_, Some(error)) => Finish::Failed { error },
            (outcome, None) => self.finish_for(&row, outcome, cancel_requested, now),
        };
        self.finish(&row, finish);
    }
    fn finish_for(
        &self,
        row: &JobRow,
        outcome: Outcome,
        cancel_requested: bool,
        now: i64,
    ) -> Finish {
        match outcome {
            Outcome::Succeeded(v) => Finish::Succeeded {
                result_json: v.to_string(),
                detected_committed: false,
            },
            Outcome::AlreadyCommitted(v) => Finish::Succeeded {
                result_json: v.to_string(),
                detected_committed: true,
            },
            Outcome::Cancelled => Finish::Cancelled,
            Outcome::Retryable(_) if cancel_requested => Finish::Cancelled,
            Outcome::Fatal(error) => Finish::Failed { error },
            Outcome::Retryable(error) if row.attempts >= row.max_attempts => Finish::Failed {
                error: format!("attempt limit {} reached: {error}", row.max_attempts),
            },
            Outcome::Retryable(error) => {
                let next = now + self.policy.backoff(row.attempts).as_millis() as i64;
                if row.deadline_at_ms.is_some_and(|deadline| next > deadline) {
                    Finish::Failed {
                        error: format!("deadline exceeded before retry: {error}"),
                    }
                } else {
                    Finish::Retry {
                        error,
                        next_attempt_at_ms: next,
                    }
                }
            }
        }
    }
    fn finish(&self, row: &JobRow, finish: Finish) {
        // A false return means the lease moved on; the newer owner decides the outcome.
        let _ = self
            .db()
            .daemon_jobs()
            .finish(row.id, row.lease_generation, now_ms(), finish);
    }
}
