//! Durable job contract (#25). GUI, CLI, REST and MCP read the same `JobStatus`.
//!
//! Recovery is at-least-once execution with idempotent effects: a handler may run again
//! after a crash, and must detect an already committed result instead of re-registering.
//! This is deliberately NOT called exactly-once.
use crate::domain::PaperId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    /// Waiting for backoff before the next attempt. Active: blocks daemon idle exit.
    RetryWait,
    Succeeded,
    /// Terminal with a reason (attempt limit, deadline, fatal error). Explicitly retryable.
    Failed,
    /// Terminal by user request. Explicitly retryable.
    Cancelled,
}
impl JobState {
    pub const ACTIVE: [JobState; 3] = [Self::Queued, Self::Running, Self::RetryWait];
    /// Active jobs keep the daemon alive; only zero active jobs permits idle exit.
    pub fn is_active(self) -> bool {
        Self::ACTIVE.contains(&self)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Running,
            Self::RetryWait,
            Self::Succeeded,
            Self::Failed,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JobProgress {
    pub done: u64,
    pub total: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    pub id: JobId,
    pub kind: String,
    pub idempotency_key: String,
    pub state: JobState,
    /// Attempts started (claims). Reset by an explicit retry.
    pub attempts: u32,
    pub max_attempts: u32,
    pub progress: JobProgress,
    /// Last failure reason (also kept while retry_wait).
    pub error: Option<String>,
    pub result: Option<serde_json::Value>,
    /// The result was found already committed by an earlier (crashed/lost) attempt.
    pub detected_committed: bool,
    pub cancel_requested: bool,
    /// Fencing generation: incremented per claim; stale generations cannot commit.
    pub lease_generation: u64,
    pub next_attempt_at_ms: Option<i64>,
    pub deadline_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Submission is idempotent on `(kind, idempotency_key)`: resubmitting returns the
/// existing job (any state) instead of creating a duplicate; a different payload conflicts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSubmit {
    pub kind: String,
    pub idempotency_key: String,
    pub payload: serde_json::Value,
    /// 1..=20; default 3.
    pub max_attempts: Option<u32>,
    /// Absolute Unix ms. Past deadline the job fails instead of being (re)tried.
    pub deadline_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum JobCommand {
    Submit(JobSubmit),
    Get(JobId),
    List {
        active_only: bool,
    },
    /// queued/retry_wait -> cancelled now; running -> cancel_requested, then cancelled.
    Cancel(JobId),
    /// Explicit retry of failed/cancelled: attempts reset, state queued.
    Retry(JobId),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobReply {
    Job(Box<JobStatus>),
    Jobs(Vec<JobStatus>),
}

/// Built-in job kinds. Later issues (e.g. #18 extraction) add kinds and handlers.
pub mod kinds {
    use super::*;
    /// Copy a local file into the managed attachment store and link it to a paper.
    /// Idempotent on (paper, sha256): a rerun detects the existing link.
    pub const ATTACHMENT_IMPORT: &str = "attachment.import";
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct AttachmentImport {
        pub paper: PaperId,
        pub path: PathBuf,
        pub original_name: Option<String>,
        pub media_type: Option<String>,
    }
    /// Diagnostic job without library effects: waits (cancellable, with progress) and can
    /// fail its first attempts on purpose. Used to exercise lifecycle/retry/lease recovery.
    pub const DIAGNOSTIC_WAIT: &str = "diagnostic.wait";
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct DiagnosticWait {
        /// 0..=60000.
        pub millis: u64,
        /// Attempts numbered <= this fail with a retryable error.
        #[serde(default)]
        pub fail_attempts: u32,
    }
}
