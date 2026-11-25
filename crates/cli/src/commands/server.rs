//! `server status|start|stop`: the local papersd only. There is no REST listener (#27).
use super::unexpected;
use crate::{
    args::Server,
    connect::{CONTROL, Connection, Daemon, Target},
    credentials::{Need, requested},
    output::{CliError, Kind, Report, path_json},
};
use cm_protocol::ipc::{Command as Ipc, DaemonStatus, GrantCommand, Reply};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const REST: &str = "not available yet (REST API is issue #27; papersd serves local IPC only)";

pub fn run(target: &Target, read_only: bool, command: &Server) -> Result<Report, CliError> {
    // Status needs only an authenticated live grant: request no capabilities at all.
    let caps = requested(Need::Status, read_only);
    let base = json!({
        "library": path_json(&target.paths.library),
        "socket": path_json(&target.paths.socket),
        "rest": REST,
    });
    match command {
        Server::Status => match Connection::probe(target, caps)? {
            None => Ok(stopped(
                base,
                "papersd is not running (it starts on demand)",
            )),
            Some(mut c) => running(&mut c, base),
        },
        Server::Start { rest: true } => Err(CliError::new(
            Kind::FeatureUnavailable,
            format!("--rest: {REST}"),
        )),
        Server::Start { rest: false } => {
            let was_running = Connection::probe(target, caps.clone())?.is_some();
            if !was_running && !target.start {
                return Err(CliError::usage("--no-start contradicts server start"));
            }
            let mut c = Connection::open(target, caps)?;
            let mut report = running(&mut c, base)?;
            report.data["started"] = json!(!was_running);
            Ok(report)
        }
        Server::Stop { force } => stop(target, read_only, *force, base),
    }
}

/// Stops papersd with SIGTERM after proving, through the daemon, that this credential holds
/// GrantAdmin (read-only or restricted grants cannot stop the owner's daemon), and that the
/// PID to signal is the process actually serving the authenticated socket (`SO_PEERCRED`).
fn stop(target: &Target, read_only: bool, force: bool, base: Value) -> Result<Report, CliError> {
    if read_only {
        return Err(CliError::new(
            Kind::PermissionDenied,
            "server stop is refused under --read-only",
        ));
    }
    let caps = requested(Need::Admin, read_only);
    let Some(mut c) = Connection::probe(target, caps.clone())? else {
        let mut report = stopped(base, "papersd is not running");
        report.data["stopped"] = json!(false);
        return Ok(report);
    };
    // Daemon-side authorization: only a GrantAdmin session may list grants.
    match c.call(Ipc::Grants(GrantCommand::List), CONTROL) {
        Ok(Reply::Grants(_)) => {}
        Ok(_) => return Err(unexpected()),
        Err(e) if e.kind == Kind::PermissionDenied => {
            return Err(CliError::new(
                Kind::PermissionDenied,
                "server stop needs the owner's administrative grant (GrantAdmin)",
            ));
        }
        Err(e) => return Err(e),
    }
    let status = status(&mut c)?;
    // Our own connection counts as one.
    let others = status.connections.saturating_sub(1);
    if (others > 0 || status.active_jobs > 0) && !force {
        return Err(CliError::new(
            Kind::Conflict,
            format!(
                "papersd has {others} other client(s) and {} active job(s); pass --force to stop it anyway",
                status.active_jobs
            ),
        )
        .with_report(Report::new(status_json(&status, base))));
    }
    verify_peer(c.peer_pid, status.pid)?;
    let pid = rustix::process::Pid::from_raw(status.pid as i32)
        .ok_or_else(|| CliError::internal("daemon reported an invalid PID"))?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).map_err(|e| {
        CliError::new(
            Kind::PermissionDenied,
            format!("cannot signal papersd: {e}"),
        )
    })?;
    drop(c);
    // Stopped only when nothing listens any more (no socket / connection refused). Other probe
    // failures (e.g. a handshake cut short while it exits) mean "not yet known".
    let started = Instant::now();
    let mut last_error = None;
    loop {
        match Connection::probe(target, caps.clone()) {
            Ok(None) => break,
            Ok(Some(_)) => {}
            Err(e) => last_error = Some(e.message),
        }
        if started.elapsed() > Duration::from_secs(10) {
            return Err(CliError::new(
                Kind::DaemonUnavailable,
                format!(
                    "papersd (pid {}) did not stop within 10 s{}",
                    status.pid,
                    last_error.map(|e| format!(": {e}")).unwrap_or_default()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut report = stopped(base, "papersd stopped");
    report.data["stopped"] = json!(true);
    report.data["pid"] = json!(status.pid);
    Ok(report)
}

/// The PID reported by `Status` must be the peer of the authenticated socket. Without peer
/// credentials (non-Linux until #34) the CLI refuses rather than signal an unverified PID.
fn verify_peer(peer: Option<u32>, reported: u32) -> Result<(), CliError> {
    match peer {
        Some(peer) if peer == reported => Ok(()),
        Some(peer) => Err(CliError::new(
            Kind::Conflict,
            format!(
                "socket peer pid {peer} differs from the reported daemon pid {reported}; refusing to signal"
            ),
        )),
        None => Err(CliError::new(
            Kind::FeatureUnavailable,
            "server stop needs socket peer credentials, available on Linux only for now (#34)",
        )),
    }
}

fn status(c: &mut Connection) -> Result<DaemonStatus, CliError> {
    match c.call(Ipc::Status, CONTROL)? {
        Reply::Status(status) => Ok(status),
        _ => Err(unexpected()),
    }
}

fn running(c: &mut Connection, base: Value) -> Result<Report, CliError> {
    let status = status(c)?;
    let line = format!(
        "papersd {} running (pid {}, {} connection(s), {} active job(s))",
        status.daemon_version, status.pid, status.connections, status.active_jobs
    );
    Ok(Report::new(status_json(&status, base)).line(line))
}

fn status_json(status: &DaemonStatus, mut base: Value) -> Value {
    base["running"] = json!(true);
    base["pid"] = json!(status.pid);
    base["daemon_version"] = json!(status.daemon_version);
    base["protocol_version"] = json!(status.protocol_version);
    base["connections"] = json!(status.connections);
    base["active_jobs"] = json!(status.active_jobs);
    base["session_capabilities"] = json!(status.capabilities);
    base
}

fn stopped(mut base: Value, line: &str) -> Report {
    base["running"] = json!(false);
    Report::new(base).line(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_verified_socket_peer_is_signalled() {
        assert!(verify_peer(Some(42), 42).is_ok());
        assert_eq!(verify_peer(Some(41), 42).unwrap_err().kind, Kind::Conflict);
        assert_eq!(
            verify_peer(None, 42).unwrap_err().kind,
            Kind::FeatureUnavailable
        );
    }
}
