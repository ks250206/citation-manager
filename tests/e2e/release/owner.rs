//! Release E2E helper (tests/e2e/release), not shipped: the library *owner's* session, i.e.
//! the desktop app's authenticated IPC path (desktop bootstrap credential, starting the
//! installed `papersd` if needed). It stands in for the two GUI actions the release E2E
//! cannot click reliably on a headless display:
//!
//! * `confirm-ai --paper UUID`: the owner accepts every unconfirmed AI annotation of the
//!   paper (the app's "Confirm" button, `AnnotationRequest::ConfirmAi`);
//! * `export-pdf --attachment UUID --out ABS [--replace]`: the app's "Save PDF…" with the
//!   chosen destination (`PdfCommand::Export`, the same request the save dialog sends). The
//!   daemon writes the copy, re-reads and verifies it, and returns the report printed here.
//!
//! `release_e2e_owner --library ABS --runtime-dir ABS --papersd ABS <command> ...`
//! prints one JSON object on stdout and exits 0 when the command succeeded.
#[cfg(unix)]
fn main() {
    use cm_protocol::{
        annotations::{AnnotationReply, AnnotationRequest, Source},
        attachments::PdfReply,
        client::{
            Session,
            local::{DaemonPaths, Launch, LocalConnector},
        },
        ipc::{Command, PdfCommand, Reply},
    };
    use serde_json::{Value, json};
    use std::{path::PathBuf, time::Duration};

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
    let fail = |result: Value| -> ! {
        println!("{result}");
        std::process::exit(1)
    };
    let library = PathBuf::from(required("--library"));
    let runtime = PathBuf::from(required("--runtime-dir"));
    let papersd = PathBuf::from(required("--papersd"));
    let command = args
        .iter()
        .find(|a| matches!(a.as_str(), "confirm-ai" | "export-pdf"))
        .cloned()
        .unwrap_or_else(|| {
            eprintln!("command: confirm-ai | export-pdf");
            std::process::exit(2)
        });

    let uid = cm_native_host::config::euid();
    let paths = DaemonPaths::new(&library, Some(&runtime), uid).expect("paths");
    let mut connector = LocalConnector::new(paths, uid, "release-e2e-owner");
    connector.launch = Some(Launch {
        papersd,
        extra_args: Vec::new(),
        timeout: Duration::from_secs(20),
    });
    let mut session = Session::open(&connector).expect("open library as owner");

    match command.as_str() {
        "confirm-ai" => {
            let paper = serde_json::from_value(Value::String(required("--paper"))).expect("paper");
            let list = match session.call(Command::Annotations(AnnotationRequest::ForPaper(paper)))
            {
                Ok(Ok(Reply::Annotations(AnnotationReply::List(list)))) => list,
                other => fail(json!({ "ok": false, "reply": format!("{other:?}") })),
            };
            let mut confirmed = Vec::new();
            for annotation in list
                .iter()
                .filter(|a| a.source == Source::Ai && a.confirmed_at.is_none())
            {
                match session.call(Command::Annotations(AnnotationRequest::ConfirmAi(
                    annotation.id,
                ))) {
                    Ok(Ok(Reply::Annotations(_))) => confirmed.push(annotation.id.0.to_string()),
                    other => fail(json!({ "ok": false, "reply": format!("{other:?}") })),
                }
            }
            println!(
                "{}",
                json!({ "ok": true, "annotations": list.len(), "confirmed": confirmed })
            );
        }
        _ => {
            let attachment = serde_json::from_value(Value::String(required("--attachment")))
                .expect("attachment");
            let destination = PathBuf::from(required("--out"));
            let reply = session.call(Command::Pdf(PdfCommand::Export {
                attachment,
                destination,
                replace_existing: args.iter().any(|a| a == "--replace"),
                allow_omissions: false,
            }));
            match reply {
                Ok(Ok(Reply::Pdf(PdfReply::Exported(Ok(report))))) => {
                    println!(
                        "{}",
                        json!({ "ok": true, "complete": report.complete(), "report": report })
                    );
                }
                Ok(Ok(Reply::Pdf(PdfReply::Exported(Err(failure))))) => fail(json!({
                    "ok": false, "failure": failure, "message": failure.to_string(),
                })),
                other => fail(json!({ "ok": false, "reply": format!("{other:?}") })),
            }
        }
    }
}
#[cfg(not(unix))]
fn main() {}
