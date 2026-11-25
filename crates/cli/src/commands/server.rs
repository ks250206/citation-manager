//! `server status|start|stop`: the local papersd only. There is no REST listener (#27).
use super::unexpected;
use crate::{
    args::Server,
    connect::{CONTROL, Connection, Daemon, Target},
    credentials::{Need, requested},
    output::{CliError, Kind, Report},
};
use cm_protocol::ipc::{Command as Ipc, DaemonStatus, Reply};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const REST: &str = "not available yet (REST API is issue #27; papersd serves local IPC only)";

pub fn run(target: &Target, read_only: bool, command: &Server) -> Result<Report, CliError> {
    // Status needs only an authenticated live grant: request no capabilities at all.
    let caps = requested(Need::Status, read_only);
    let base = json!({
        "library": target.paths.library,
        "socket": target.paths.socket,
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
        Server::Stop { force } => {
            let Some(mut c) = Connection::probe(target, caps.clone())? else {
                let mut report = stopped(base, "papersd is not running");
                report.data["stopped"] = json!(false);
                return Ok(report);
            };
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
            let pid = rustix::process::Pid::from_raw(status.pid as i32)
                .ok_or_else(|| CliError::internal("daemon reported an invalid PID"))?;
            rustix::process::kill_process(pid, rustix::process::Signal::TERM).map_err(|e| {
                CliError::new(
                    Kind::PermissionDenied,
                    format!("cannot signal papersd: {e}"),
                )
            })?;
            drop(c);
            let started = Instant::now();
            while Connection::probe(target, caps.clone())
                .ok()
                .flatten()
                .is_some()
            {
                if started.elapsed() > Duration::from_secs(10) {
                    return Err(CliError::new(
                        Kind::DaemonUnavailable,
                        "papersd did not stop within 10 s",
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let mut report = stopped(base, "papersd stopped");
            report.data["stopped"] = json!(true);
            report.data["pid"] = json!(status.pid);
            Ok(report)
        }
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
