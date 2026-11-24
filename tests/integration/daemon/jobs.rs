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
    ipc::{Capability, Command, Denial, GrantCommand, GrantReply, Operation, Reply, WireError},
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

fn caps(list: &[Capability]) -> cm_protocol::ipc::CapabilitySet {
    list.iter().copied().collect()
}
fn job_reply(result: Result<Reply, WireError>) -> Result<JobStatus, WireError> {
    result.map(|reply| match reply {
        Reply::Jobs(JobReply::Job(job)) => *job,
        other => panic!("{other:?}"),
    })
}
fn wait_job(session: &mut Session, id: JobId) -> JobStatus {
    match ok(session.call(Command::Jobs(JobCommand::Get(id)))) {
        Reply::Jobs(JobReply::Job(job)) => *job,
        other => panic!("{other:?}"),
    }
}

#[test]
fn retry_requires_the_kind_capabilities_of_the_retrying_session() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    let source = env.temp.path().canonicalize().unwrap().join("x.pdf");
    std::fs::write(&source, b"%PDF-1.4").unwrap();
    // No such paper: the attachment job fails (fatally) and becomes explicitly retryable.
    let payload = json!({"paper": uuid::Uuid::now_v7(), "path": source, "original_name": null, "media_type": null});
    let job = submit(&mut desktop, kinds::ATTACHMENT_IMPORT, "a", payload, None);
    let failed = settle(&mut desktop, job.id);
    assert_eq!(failed.state, JobState::Failed);
    // Same grant, but the session narrowed itself to job control only.
    let mut connector = env.connector("cli");
    connector.requested = Some(caps(&[Capability::JobsRead, Capability::JobsControl]));
    let mut narrowed = Session::open(&connector).unwrap();
    assert_eq!(wait_job(&mut narrowed, job.id).id, job.id);
    assert_eq!(
        job_reply(
            narrowed
                .call(Command::Jobs(JobCommand::Retry(job.id)))
                .unwrap()
        )
        .unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::JobRetry))
    );
    assert_eq!(get(&mut desktop, job.id).state, JobState::Failed);
    // The full session may retry it.
    let retried = job_reply(
        desktop
            .call(Command::Jobs(JobCommand::Retry(job.id)))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(retried.state, JobState::Queued);
    assert_eq!(settle(&mut desktop, job.id).state, JobState::Failed);
}

#[test]
fn jobs_of_a_revoked_or_expired_grant_fail_instead_of_running() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    let control = caps(&[Capability::JobsRead, Capability::JobsControl]);
    let credential = issue(&mut desktop, "worker", control.clone());
    let mut worker = env.session("cli", credential);
    let grant = worker.welcome.grant;
    let running = submit(
        &mut worker,
        kinds::DIAGNOSTIC_WAIT,
        "long",
        json!({"millis": 20000}),
        None,
    );
    assert!(wait_until(Duration::from_secs(10), || {
        get(&mut worker, running.id).state == JobState::Running
    }));
    // One runner: this one waits in the queue behind the running job.
    let queued = submit(
        &mut worker,
        kinds::DIAGNOSTIC_WAIT,
        "queued",
        json!({"millis": 0}),
        None,
    );
    assert_eq!(queued.state, JobState::Queued);
    assert!(matches!(
        ok(desktop.call(Command::Grants(GrantCommand::Revoke(grant)))),
        Reply::Grants(GrantReply::Revoked(g)) if g == grant
    ));
    let queued = get(&mut desktop, queued.id);
    assert_eq!(
        (queued.state, queued.error.as_deref(), queued.attempts),
        (JobState::Failed, Some("owner grant revoked"), 0)
    );
    let running = settle(&mut desktop, running.id);
    assert_eq!(
        (running.state, running.error.as_deref()),
        (JobState::Failed, Some("owner grant revoked"))
    );

    // Expiry: a queued job of a grant that expired before it was claimed never runs.
    let blocker = submit(
        &mut desktop,
        kinds::DIAGNOSTIC_WAIT,
        "blocker",
        json!({"millis": 20000}),
        None,
    );
    assert!(wait_until(Duration::from_secs(10), || {
        get(&mut desktop, blocker.id).state == JobState::Running
    }));
    let soon = cm_daemon::ipc::auth::now_ms() + 400;
    let expiring = match ok(desktop.call(Command::Grants(GrantCommand::Issue {
        label: "short".into(),
        capabilities: control,
        expires_at_ms: Some(soon),
    }))) {
        Reply::Grants(GrantReply::Issued { credential, .. }) => credential,
        other => panic!("{other:?}"),
    };
    let mut short = env.session("cli", expiring);
    let late = submit(
        &mut short,
        kinds::DIAGNOSTIC_WAIT,
        "late",
        json!({"millis": 0}),
        None,
    );
    std::thread::sleep(Duration::from_millis(500));
    ok(desktop.call(Command::Jobs(JobCommand::Cancel(blocker.id))));
    let late = settle(&mut desktop, late.id);
    assert_eq!(
        (late.state, late.error.as_deref()),
        (JobState::Failed, Some("owner grant expired"))
    );
    assert!(late.result.is_none());
}

#[test]
fn idempotency_keys_are_scoped_to_the_submitting_grant() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    let control = caps(&[Capability::JobsRead, Capability::JobsControl]);
    let mut a = env.session("cli", issue(&mut desktop, "a", control.clone()));
    let mut b = env.session("cli", issue(&mut desktop, "b", control));
    let first = submit(
        &mut a,
        kinds::DIAGNOSTIC_WAIT,
        "same",
        json!({"millis": 0}),
        None,
    );
    // Another grant, same key, even another payload: its own job, no Conflict, no leak.
    let second = submit(
        &mut b,
        kinds::DIAGNOSTIC_WAIT,
        "same",
        json!({"millis": 1}),
        None,
    );
    assert_ne!(first.id, second.id);
    assert_eq!(second.state, JobState::Queued);
    assert_eq!(second.attempts, 0);
    // Each grant still deduplicates its own key.
    assert_eq!(
        submit(
            &mut b,
            kinds::DIAGNOSTIC_WAIT,
            "same",
            json!({"millis": 1}),
            None
        )
        .id,
        second.id
    );
    assert_eq!(settle(&mut a, first.id).state, JobState::Succeeded);
    assert_eq!(settle(&mut b, second.id).state, JobState::Succeeded);
    match ok(b.call(Command::Jobs(JobCommand::List { active_only: false }))) {
        Reply::Jobs(JobReply::Jobs(list)) => {
            assert_eq!(list.iter().map(|j| j.id).collect::<Vec<_>>(), [second.id]);
        }
        other => panic!("{other:?}"),
    }
}

/// Reports progress forever; only the run-time limit stops it.
struct Endless;
impl JobHandler for Endless {
    fn kind(&self) -> &'static str {
        "test.endless"
    }
    fn required(&self) -> &'static [Capability] {
        &[]
    }
    fn validate(&self, _: &Value) -> Result<(), ServiceError> {
        Ok(())
    }
    fn max_run(&self) -> Duration {
        Duration::from_millis(300)
    }
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome {
        while ctx.progress(cm_protocol::jobs::JobProgress::default()) {
            std::thread::sleep(Duration::from_millis(20));
        }
        Outcome::Cancelled
    }
}

#[test]
fn running_jobs_stop_at_their_deadline_or_run_time_limit() {
    let env = Env::new();
    let mut config = env.config();
    config.handlers.push(Arc::new(Endless));
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let endless = submit(&mut desktop, "test.endless", "e", json!({}), Some(5));
    let endless = settle(&mut desktop, endless.id);
    assert_eq!(
        (endless.state, endless.error.as_deref(), endless.attempts),
        (
            JobState::Failed,
            Some("run time limit of 300 ms exceeded"),
            1
        )
    );
    let deadline = cm_daemon::ipc::auth::now_ms() + 500;
    let late = job_reply(
        desktop
            .call(Command::Jobs(JobCommand::Submit(JobSubmit {
                kind: kinds::DIAGNOSTIC_WAIT.into(),
                idempotency_key: "deadline".into(),
                payload: json!({"millis": 20000}),
                max_attempts: None,
                deadline_at_ms: Some(deadline),
            })))
            .unwrap(),
    )
    .unwrap();
    let late = settle(&mut desktop, late.id);
    assert_eq!(
        (late.state, late.error.as_deref()),
        (JobState::Failed, Some("deadline exceeded while running"))
    );
    assert!(late.updated_at_ms < deadline + 5_000);
}
