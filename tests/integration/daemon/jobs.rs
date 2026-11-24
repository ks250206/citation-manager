//! #25 durable jobs: idempotent submission, committed-result detection without double
//! registration, attempt limit -> failed with reason, explicit retry, cancel, deadline.
mod support;
use cm_daemon::{
    ipc::start,
    jobs::{AttachmentImportHandler, JobContext, JobHandler, Outcome, default_handlers},
};
use cm_protocol::{
    client::Session,
    domain::PaperId,
    ipc::{Capability, Command, Reply, WireError},
    jobs::{JobCommand, JobId, JobReply, JobState, JobStatus, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest, NewPaper, ServiceError},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use support::*;

fn submit(
    session: &mut Session,
    kind: &str,
    key: &str,
    payload: Value,
    max: Option<u32>,
) -> JobStatus {
    match ok(session.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kind.into(),
        idempotency_key: key.into(),
        payload,
        max_attempts: max,
        deadline_at_ms: None,
    })))) {
        Reply::Jobs(JobReply::Job(job)) => *job,
        other => panic!("{other:?}"),
    }
}
fn get(session: &mut Session, id: JobId) -> JobStatus {
    match ok(session.call(Command::Jobs(JobCommand::Get(id)))) {
        Reply::Jobs(JobReply::Job(job)) => *job,
        other => panic!("{other:?}"),
    }
}
fn settle(session: &mut Session, id: JobId) -> JobStatus {
    let mut last = None;
    assert!(wait_until(Duration::from_secs(20), || {
        let job = get(session, id);
        let done = !job.state.is_active();
        last = Some(job);
        done
    }));
    last.unwrap()
}
fn paper(session: &mut Session) -> PaperId {
    match ok(
        session.call(Command::Library(LibraryRequest::Create(Box::new(
            NewPaper::titled("Attachment owner"),
        )))),
    ) {
        Reply::Library(LibraryReply::Paper(p)) => p.id,
        other => panic!("{other:?}"),
    }
}
fn attachment_count(session: &mut Session, paper: PaperId) -> usize {
    match ok(session.call(Command::Pdf(cm_protocol::ipc::PdfCommand::List(paper)))) {
        Reply::Pdf(cm_protocol::attachments::PdfReply::Attachments(list)) => list.len(),
        other => panic!("{other:?}"),
    }
}

/// Commits the real attachment effect, then "loses" the result once (as a crash between
/// the library commit and the job-table update would).
struct LoseFirstResult(AtomicU32);
impl JobHandler for LoseFirstResult {
    fn kind(&self) -> &'static str {
        "test.lose-first-result"
    }
    fn required(&self) -> &'static [Capability] {
        &[Capability::LibraryWrite, Capability::LocalFileRead]
    }
    fn validate(&self, payload: &Value) -> Result<(), ServiceError> {
        AttachmentImportHandler.validate(payload)
    }
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome {
        let outcome = AttachmentImportHandler.run(ctx);
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            assert!(matches!(outcome, Outcome::Succeeded(_)));
            return Outcome::Retryable("simulated crash after commit".into());
        }
        outcome
    }
}
/// Fails its first `n` executions across explicit retries.
struct FailRuns(AtomicU32, u32);
impl JobHandler for FailRuns {
    fn kind(&self) -> &'static str {
        "test.fail-runs"
    }
    fn required(&self) -> &'static [Capability] {
        &[]
    }
    fn validate(&self, _: &Value) -> Result<(), ServiceError> {
        Ok(())
    }
    fn run(&self, _: &mut JobContext<'_>) -> Outcome {
        if self.0.fetch_add(1, Ordering::SeqCst) < self.1 {
            Outcome::Retryable("transient".into())
        } else {
            Outcome::Succeeded(json!("ok"))
        }
    }
}

#[test]
fn attachment_jobs_are_idempotent_and_detect_committed_results() {
    let env = Env::new();
    let mut config = env.config();
    config.handlers = default_handlers();
    config
        .handlers
        .push(Arc::new(LoseFirstResult(AtomicU32::new(0))));
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let owner = paper(&mut desktop);
    let source = env.temp.path().canonicalize().unwrap().join("paper.pdf");
    std::fs::write(&source, b"%PDF-1.4 test bytes").unwrap();
    let payload = json!({"paper": owner.0, "path": source, "original_name": "paper.pdf", "media_type": "application/pdf"});

    let first = submit(
        &mut desktop,
        kinds::ATTACHMENT_IMPORT,
        "k1",
        payload.clone(),
        None,
    );
    // Same key: the same job, never a second one. Different payload under the key: conflict.
    assert_eq!(
        submit(
            &mut desktop,
            kinds::ATTACHMENT_IMPORT,
            "k1",
            payload.clone(),
            None
        )
        .id,
        first.id
    );
    let other =
        json!({"paper": owner.0, "path": "/etc/hosts", "original_name": null, "media_type": null});
    assert!(matches!(
        desktop
            .call(Command::Jobs(JobCommand::Submit(JobSubmit {
                kind: kinds::ATTACHMENT_IMPORT.into(),
                idempotency_key: "k1".into(),
                payload: other,
                max_attempts: None,
                deadline_at_ms: None,
            })))
            .unwrap(),
        Err(WireError::Service(ServiceError::Conflict))
    ));
    let done = settle(&mut desktop, first.id);
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert!(!done.detected_committed);
    assert_eq!(attachment_count(&mut desktop, owner), 1);
    // A new job for the same content (another key) detects the committed link.
    let again = submit(
        &mut desktop,
        kinds::ATTACHMENT_IMPORT,
        "k2",
        payload.clone(),
        None,
    );
    let again = settle(&mut desktop, again.id);
    assert_eq!(again.state, JobState::Succeeded);
    assert!(again.detected_committed);
    assert_eq!(attachment_count(&mut desktop, owner), 1);

    // Result lost after commit: the retry detects it instead of registering twice.
    let second_owner = paper_with(&mut desktop, "Second owner");
    let payload =
        json!({"paper": second_owner.0, "path": source, "original_name": null, "media_type": null});
    let lost = submit(&mut desktop, "test.lose-first-result", "k3", payload, None);
    let lost = settle(&mut desktop, lost.id);
    assert_eq!(lost.state, JobState::Succeeded, "{lost:?}");
    assert_eq!(lost.attempts, 2);
    assert!(lost.detected_committed);
    assert_eq!(attachment_count(&mut desktop, second_owner), 1);
}

fn paper_with(session: &mut Session, title: &str) -> PaperId {
    match ok(
        session.call(Command::Library(LibraryRequest::Create(Box::new(
            NewPaper::titled(title),
        )))),
    ) {
        Reply::Library(LibraryReply::Paper(p)) => p.id,
        other => panic!("{other:?}"),
    }
}

#[test]
fn attempt_limit_fails_with_reason_and_explicit_retry_requeues() {
    let env = Env::new();
    let mut config = env.config();
    config
        .handlers
        .push(Arc::new(FailRuns(AtomicU32::new(0), 3)));
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let job = submit(&mut desktop, "test.fail-runs", "limit", json!({}), Some(2));
    // Backoff is observable as retry_wait (an active state) between attempts.
    let mut saw_retry_wait = false;
    let done = {
        let mut last = None;
        assert!(wait_until(Duration::from_secs(20), || {
            let s = get(&mut desktop, job.id);
            saw_retry_wait |= s.state == JobState::RetryWait && s.next_attempt_at_ms.is_some();
            let finished = !s.state.is_active();
            last = Some(s);
            finished
        }));
        last.unwrap()
    };
    assert!(saw_retry_wait);
    assert_eq!(done.state, JobState::Failed);
    assert_eq!(done.attempts, 2);
    assert_eq!(
        done.error.as_deref(),
        Some("attempt limit 2 reached: transient")
    );
    // Explicit retry: attempts reset, runs again (third run fails, fourth succeeds).
    let retried = match ok(desktop.call(Command::Jobs(JobCommand::Retry(job.id)))) {
        Reply::Jobs(JobReply::Job(j)) => *j,
        other => panic!("{other:?}"),
    };
    assert_eq!((retried.state, retried.attempts), (JobState::Queued, 0));
    let done = settle(&mut desktop, job.id);
    assert_eq!((done.state, done.attempts), (JobState::Succeeded, 2));
    // Succeeded jobs cannot be "retried" into a duplicate run.
    assert!(matches!(
        desktop
            .call(Command::Jobs(JobCommand::Retry(job.id)))
            .unwrap(),
        Err(WireError::Service(ServiceError::Conflict))
    ));
}

#[test]
fn running_jobs_cancel_and_deadlines_fail() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    let job = submit(
        &mut desktop,
        kinds::DIAGNOSTIC_WAIT,
        "long",
        json!({"millis": 20000}),
        None,
    );
    assert!(wait_until(Duration::from_secs(10), || {
        let s = get(&mut desktop, job.id);
        s.state == JobState::Running && s.progress.total == Some(20000)
    }));
    let requested = match ok(desktop.call(Command::Jobs(JobCommand::Cancel(job.id)))) {
        Reply::Jobs(JobReply::Job(j)) => *j,
        other => panic!("{other:?}"),
    };
    assert!(requested.cancel_requested);
    let done = settle(&mut desktop, job.id);
    assert_eq!(done.state, JobState::Cancelled);

    let past = match ok(desktop.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kinds::DIAGNOSTIC_WAIT.into(),
        idempotency_key: "late".into(),
        payload: json!({"millis": 0}),
        max_attempts: None,
        deadline_at_ms: Some(1),
    })))) {
        Reply::Jobs(JobReply::Job(j)) => *j,
        other => panic!("{other:?}"),
    };
    let done = settle(&mut desktop, past.id);
    assert_eq!(
        (done.state, done.error.as_deref()),
        (JobState::Failed, Some("deadline exceeded"))
    );
    assert!(matches!(
        desktop
            .call(Command::Jobs(JobCommand::Submit(JobSubmit {
                kind: "no.such.kind".into(),
                idempotency_key: "x".into(),
                payload: json!({}),
                max_attempts: None,
                deadline_at_ms: None,
            })))
            .unwrap(),
        Err(WireError::Service(ServiceError::InvalidInput(_)))
    ));
    match ok(desktop.call(Command::Jobs(JobCommand::List { active_only: true }))) {
        Reply::Jobs(JobReply::Jobs(list)) => assert!(list.is_empty()),
        other => panic!("{other:?}"),
    }
}
