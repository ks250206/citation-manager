//! Browser smoke verifier (scripts/native-host/browser_smoke.py), not shipped: after the
//! browser and the host have exited, open the library as its owner (desktop bootstrap
//! credential, starting `papersd` if it already idled out) and report whether the captured
//! paper was persisted and whether its PDF job reached a terminal state.
//!
//! `inspect_capture --library ABS --runtime-dir ABS --papersd ABS --paper UUID [--job UUID] [--wait-secs N]`
#[cfg(unix)]
fn main() {
    use cm_protocol::{
        client::{
            Session,
            local::{DaemonPaths, Launch, LocalConnector},
        },
        ipc::{Command, Reply},
        jobs::{JobCommand, JobId, JobReply},
        library::{LibraryReply, LibraryRequest},
    };
    use serde_json::{Value, json};
    use std::{path::PathBuf, time::Duration, time::Instant};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let required = |flag: &str| {
        value(flag).unwrap_or_else(|| {
            eprintln!("missing {flag}");
            std::process::exit(2)
        })
    };
    let library = PathBuf::from(required("--library"));
    let runtime = PathBuf::from(required("--runtime-dir"));
    let papersd = PathBuf::from(required("--papersd"));
    let paper = serde_json::from_value(Value::String(required("--paper"))).expect("paper id");
    let job: Option<JobId> =
        value("--job").map(|j| serde_json::from_value(Value::String(j)).expect("job id"));
    let wait = Duration::from_secs(
        value("--wait-secs")
            .and_then(|s| s.parse().ok())
            .unwrap_or(120),
    );

    let uid = cm_native_host::config::euid();
    let paths = DaemonPaths::new(&library, Some(&runtime), uid).expect("paths");
    let mut connector = LocalConnector::new(paths, uid, "smoke-inspector");
    connector.launch = Some(Launch {
        papersd,
        extra_args: Vec::new(),
        timeout: Duration::from_secs(20),
    });
    let mut session = Session::open(&connector).expect("open library as owner");
    let paper = match session.call(Command::Library(LibraryRequest::Get(paper))) {
        Ok(Ok(Reply::Library(LibraryReply::Paper(p)))) => json!({
            "found": true, "title": p.title, "external_ids": p.external_ids,
            "year": p.bibliography.year, "deleted": p.deleted_at.is_some(),
        }),
        other => json!({ "found": false, "reply": format!("{other:?}") }),
    };
    let job = job.map(|id| {
        let started = Instant::now();
        loop {
            let status = match session.call(Command::Jobs(JobCommand::Get(id))) {
                Ok(Ok(Reply::Jobs(JobReply::Job(job)))) => job,
                other => return json!({ "found": false, "reply": format!("{other:?}") }),
            };
            if !status.state.is_active() || started.elapsed() > wait {
                return json!({
                    "found": true, "kind": status.kind, "state": status.state.as_str(),
                    "terminal": !status.state.is_active(), "attempts": status.attempts,
                    "error": status.error, "waited_ms": started.elapsed().as_millis() as u64,
                });
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    println!("{}", json!({ "paper": paper, "job": job }));
}
#[cfg(not(unix))]
fn main() {}
