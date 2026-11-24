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
mod handlers;
pub use handlers::{AttachmentImportHandler, DiagnosticWaitHandler};

use crate::{ipc::auth::now_ms, lifecycle::Activity};
use cm_core::{
    ServiceError,
    metadata_resolve::Cancellation,
    policy::{Capability, CapabilitySet, GrantId},
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
    time::Duration,
};

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
impl JobContext<'_> {
    /// Records progress and renews the lease. `false`: stop (cancelled or lease lost).
    pub fn progress(&self, progress: JobProgress) -> bool {
        (self.progress)(progress) && !self.cancel.is_cancelled()
    }
}

pub trait JobHandler: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    /// Capabilities the submitting grant must hold in addition to `JobsControl`.
    fn required(&self) -> &'static [Capability];
    fn validate(&self, payload: &Value) -> Result<(), ServiceError>;
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome;
}

pub fn default_handlers() -> Vec<Arc<dyn JobHandler>> {
    vec![
        Arc::new(AttachmentImportHandler),
        Arc::new(DiagnosticWaitHandler),
    ]
}

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
    /// Recovers stale leases synchronously, then starts the runner.
    pub fn start(
        directory: LibraryDirectory,
        control: Arc<Mutex<LibraryDb>>,
        handlers: Vec<Arc<dyn JobHandler>>,
        policy: JobPolicy,
        activity: Arc<Activity>,
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
    /// `owner`: submitting grant. `held`: its effective capabilities.
    pub fn submit(
        &self,
        owner: GrantId,
        held: &CapabilitySet,
        submit: JobSubmit,
    ) -> Result<JobStatus, WireError> {
        let handler = self
            .handlers
            .get(submit.kind.as_str())
            .ok_or_else(|| invalid("unknown job kind"))?;
        if !handler.required().iter().all(|c| held.contains(c)) {
            return Err(WireError::Denied(cm_core::policy::Denial::Forbidden(
                cm_core::policy::Operation::JobSubmit,
            )));
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
    pub fn retry(&self, id: JobId, owner: Option<GrantId>) -> Result<JobStatus, WireError> {
        self.get(id, owner)?;
        let row = self.db().daemon_jobs().retry(id.0, now_ms())?;
        let _ = self.wake.send(());
        status(row)
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
                    self.execute(row);
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
        // Lease renewal + sparse progress persistence; also notices DB-side cancel requests.
        let beat = || -> bool {
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
        let finish = match outcome {
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
        };
        self.finish(&row, finish);
    }
    fn finish(&self, row: &JobRow, finish: Finish) {
        // A false return means the lease moved on; the newer owner decides the outcome.
        let _ = self
            .db()
            .daemon_jobs()
            .finish(row.id, row.lease_generation, now_ms(), finish);
    }
}
