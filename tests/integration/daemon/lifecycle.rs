//! #25 lifecycle with the real `papersd` executable: one winner among concurrent starts,
//! lazy start by a client, liveness while connected / while jobs (incl. retry_wait) are
//! active, idle exit, and SIGKILL recovery of the lock, socket and job lease.
mod support;
use cm_protocol::{
    client::Session,
    ipc::{Command, Reply},
    jobs::{JobCommand, JobId, JobReply, JobState, JobStatus, JobSubmit, kinds},
};
use serde_json::json;
use std::{process::Child, time::Duration};
use support::*;

fn pid(session: &mut Session) -> u32 {
    match ok(session.call(Command::Status)) {
        Reply::Status(s) => s.pid,
        other => panic!("{other:?}"),
    }
}
fn exited(child: &mut Child, within: Duration) -> Option<i32> {
    let mut code = None;
    wait_until(within, || match child.try_wait().unwrap() {
        Some(status) => {
            code = Some(status.code().unwrap_or(-1));
            true
        }
        None => false,
    });
    code
}
fn submit(session: &mut Session, key: &str, payload: serde_json::Value) -> JobStatus {
    match ok(session.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kinds::DIAGNOSTIC_WAIT.into(),
        idempotency_key: key.into(),
        payload,
        max_attempts: Some(5),
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

#[test]
fn concurrent_starts_leave_exactly_one_writer() {
    let env = Env::new();
    let mut children: Vec<Child> = (0..4).map(|_| env.spawn_papersd(60_000)).collect();
    // Losers exit with EX_TEMPFAIL; exactly one keeps serving.
    let mut codes = Vec::new();
    let mut running = Vec::new();
    for (index, child) in children.iter_mut().enumerate() {
        match exited(child, Duration::from_secs(5)) {
            Some(code) => codes.push(code),
            None => running.push(index),
        }
    }
    assert_eq!((codes.len(), running.len()), (3, 1), "{codes:?}");
    assert!(
        codes
            .iter()
            .all(|c| *c == cm_daemon::lifecycle::EXIT_ALREADY_RUNNING)
    );
    let winner = &mut children[running[0]];
    let winner_pid = winner.id();
    assert!(wait_until(Duration::from_secs(10), || env
        .paths()
        .socket
        .exists()));
    let mut a = env.desktop();
    let mut b = env.desktop();
    assert_eq!(pid(&mut a), winner_pid);
    assert_eq!(pid(&mut b), winner_pid);
    winner.kill().unwrap();
    winner.wait().unwrap();
}

#[test]
fn lazy_start_stays_alive_while_connected_and_exits_when_idle() {
    let env = Env::new();
    // No daemon yet: the client starts one (absolute path, never PATH) and waits for it.
    let connector = env.launching_connector(300);
    let mut session = Session::open(&connector).unwrap();
    let daemon_pid = pid(&mut session);
    assert_ne!(daemon_pid, std::process::id());
    let alive = || std::path::Path::new(&format!("/proc/{daemon_pid}")).exists();
    // A held connection keeps it alive well past the idle timeout (GUI lifetime).
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(pid(&mut session), daemon_pid);
    drop(session);
    // No client, no job: idle exit and endpoint cleanup.
    assert!(wait_until(Duration::from_secs(10), || !env
        .paths()
        .socket
        .exists()));
    if cfg!(target_os = "linux") {
        assert!(wait_until(Duration::from_secs(10), || !alive()
            || std::fs::read_to_string(format!("/proc/{daemon_pid}/stat"))
                .is_ok_and(|s| s.contains(") Z "))));
    }
    // The next request starts a fresh daemon on demand.
    let mut again = Session::open(&connector).unwrap();
    assert_ne!(pid(&mut again), daemon_pid);
}

#[test]
fn active_and_retry_waiting_jobs_block_idle_exit_until_done() {
    let env = Env::new();
    let connector = env.launching_connector(200);
    let mut session = Session::open(&connector).unwrap();
    // Fails its first attempt -> retry_wait (1 s default backoff) -> runs 1 s -> succeeds.
    let job = submit(
        &mut session,
        "busy",
        json!({"millis": 1000, "fail_attempts": 1}),
    );
    assert!(wait_until(Duration::from_secs(10), || get(
        &mut session,
        job.id
    )
    .state
        == JobState::RetryWait));
    drop(session);
    // Idle timeout is 200 ms, but the retry_wait/running job keeps the daemon serving.
    std::thread::sleep(Duration::from_millis(700));
    assert!(env.paths().socket.exists());
    // The job finishes, then the daemon exits on its own.
    assert!(wait_until(Duration::from_secs(20), || !env
        .paths()
        .socket
        .exists()));
    let mut later = Session::open(&connector).unwrap();
    let done = get(&mut later, job.id);
    assert_eq!(
        (done.state, done.attempts),
        (JobState::Succeeded, 2),
        "{done:?}"
    );
}

#[test]
fn sigkill_releases_lock_and_socket_and_the_job_lease_is_recovered() {
    let env = Env::new();
    let mut daemon = env.spawn_papersd(60_000);
    assert!(wait_until(Duration::from_secs(10), || env
        .paths()
        .socket
        .exists()));
    let mut session = env.desktop();
    let job = submit(&mut session, "killed", json!({"millis": 60000}));
    assert!(wait_until(Duration::from_secs(10), || get(
        &mut session,
        job.id
    )
    .state
        == JobState::Running));
    let before = get(&mut session, job.id);
    drop(session);
    daemon.kill().unwrap(); // SIGKILL: no cleanup code runs.
    daemon.wait().unwrap();
    // The stale socket file is still there, and the lock file too; neither blocks restart.
    assert!(env.paths().socket.exists());
    assert!(env.paths().lock.exists());
    let mut restarted = env.spawn_papersd(60_000);
    let mut session = None;
    assert!(wait_until(Duration::from_secs(10), || {
        session = Session::open(&env.connector("desktop")).ok();
        session.is_some()
    }));
    let mut session = session.unwrap();
    assert_eq!(pid(&mut session), restarted.id());
    // The dead daemon's lease was reclaimed: a new attempt with a newer fencing generation.
    assert!(wait_until(Duration::from_secs(10), || {
        let s = get(&mut session, job.id);
        s.state == JobState::Running && s.attempts == before.attempts + 1
    }));
    let after = get(&mut session, job.id);
    assert!(after.lease_generation > before.lease_generation);
    // Explicit cancel ends it so the test daemon can exit.
    ok(session.call(Command::Jobs(JobCommand::Cancel(job.id))));
    assert!(wait_until(Duration::from_secs(10), || get(
        &mut session,
        job.id
    )
    .state
        == JobState::Cancelled));
    restarted.kill().unwrap();
    restarted.wait().unwrap();
}
