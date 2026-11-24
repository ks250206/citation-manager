//! #26 CLI bootstrap: issue/list/revoke tokens and edit API settings without the GUI.
//!
//! `papersd token …` / `papersd api …` connect to (or start) the library's daemon with the
//! owner's desktop bootstrap credential and send the same `Command::Api` the settings screen
//! sends, so the daemon makes the same decision. A new token is printed exactly once, on
//! stdout, and only to a terminal unless `--allow-non-tty` is given (then the caller is
//! responsible for where stdout goes). Everything else goes to stderr. The `papers` CLI
//! (#28) can wrap these commands or send `Command::Api` itself with an owner credential.
use crate::lifecycle::{DaemonPaths, Launch, euid, local_connector};
use cm_protocol::{
    api::{ApiCommand, ApiReply, BindAddress, ClientKind, IssueToken, Scope, ScopeSet, profiles},
    client::Session,
    ipc::{Command, GrantId, Reply, WireError},
};
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
/// Refused by policy (non-terminal stdout, denied by the daemon, invalid request).
pub const EXIT_REFUSED: i32 = 3;

pub const USAGE: &str = "\
Usage:
  papersd token issue  --library DIR --label TEXT --kind api|mcp|cli [--scope NAME]...
                       [--mcp-read-only] [--expires-days N] [--allow-non-tty]
  papersd token list   --library DIR
  papersd token revoke --library DIR --id GRANT
  papersd token elevate --library DIR --id GRANT --minutes N   (1..=15)
  papersd api show     --library DIR
  papersd api set      --library DIR [--enable|--disable] [--bind 127.0.0.1|::1] [--port N]
Common: [--runtime-dir DIR]. Scopes: papers:read papers:write papers:delete files:read
annotations:read annotations:write exports:write jobs:read jobs:write";

/// Whether `papersd`'s first argument selects an admin subcommand.
pub fn is_admin(first: Option<&std::ffi::OsStr>) -> bool {
    matches!(first.and_then(|a| a.to_str()), Some("token" | "api"))
}

#[derive(Default)]
struct Args {
    library: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    label: Option<String>,
    kind: Option<ClientKind>,
    scopes: ScopeSet,
    mcp_read_only: bool,
    expires_days: Option<i64>,
    allow_non_tty: bool,
    id: Option<GrantId>,
    minutes: Option<i64>,
    enable: Option<bool>,
    bind: Option<BindAddress>,
    port: Option<u16>,
}

fn parse(rest: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--library" => a.library = Some(PathBuf::from(value()?)),
            "--runtime-dir" => a.runtime_dir = Some(PathBuf::from(value()?)),
            "--label" => a.label = Some(value()?),
            "--kind" => {
                let v = value()?;
                a.kind = Some(
                    ClientKind::parse(&v)
                        .filter(|k| k.scoped_token())
                        .ok_or(format!("unknown token kind {v}"))?,
                )
            }
            "--scope" => {
                let v = value()?;
                a.scopes
                    .insert(Scope::parse(&v).ok_or(format!("unknown scope {v}"))?);
            }
            "--mcp-read-only" => a.mcp_read_only = true,
            "--expires-days" => {
                a.expires_days = Some(value()?.parse().map_err(|_| "invalid --expires-days")?)
            }
            "--allow-non-tty" => a.allow_non_tty = true,
            "--id" => {
                a.id = Some(GrantId(
                    uuid::Uuid::parse_str(&value()?).map_err(|_| "invalid --id")?,
                ))
            }
            "--minutes" => a.minutes = Some(value()?.parse().map_err(|_| "invalid --minutes")?),
            "--enable" => a.enable = Some(true),
            "--disable" => a.enable = Some(false),
            "--bind" => {
                let v = value()?;
                a.bind = Some(BindAddress::parse(&v).ok_or("--bind must be 127.0.0.1 or ::1")?)
            }
            "--port" => a.port = Some(value()?.parse().map_err(|_| "invalid --port")?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(a)
}

fn session(library: &Path, runtime_dir: Option<&Path>) -> Result<Session, String> {
    if !library.is_absolute() {
        return Err("--library must be an absolute path".into());
    }
    let paths = DaemonPaths::new(library, runtime_dir, euid()).map_err(|e| e.to_string())?;
    let mut connector = local_connector(paths, "cli-bootstrap");
    // Same binary, never a PATH search.
    let papersd = std::env::current_exe().map_err(|e| e.to_string())?;
    connector.launch = Some(Launch {
        papersd,
        extra_args: vec![],
        timeout: Duration::from_secs(20),
    });
    Session::open(&connector).map_err(|e| e.to_string())
}

fn call(session: &mut Session, command: ApiCommand) -> Result<ApiReply, (i32, String)> {
    match session.call(Command::Api(command)) {
        Ok(Ok(Reply::Api(reply))) => Ok(reply),
        Ok(Ok(_)) => Err((EXIT_ERROR, "unexpected reply".into())),
        Ok(Err(WireError::Denied(d))) => Err((EXIT_REFUSED, format!("not permitted: {d}"))),
        Ok(Err(WireError::Service(e))) => Err((EXIT_REFUSED, e.to_string())),
        Ok(Err(e)) => Err((EXIT_ERROR, format!("{e:?}"))),
        Err(e) => Err((EXIT_ERROR, e.to_string())),
    }
}

fn day(ms: Option<i64>) -> String {
    ms.map_or("-".into(), |ms| format!("{}", ms / 86_400_000))
}

/// Runs an admin subcommand. `args` excludes the program name. Returns the exit status.
pub fn run(
    args: &[OsString],
    stdout_is_tty: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let args: Option<Vec<String>> = args.iter().map(|a| a.to_str().map(String::from)).collect();
    let Some(args) = args else {
        let _ = writeln!(err, "arguments must be UTF-8\n{USAGE}");
        return EXIT_USAGE;
    };
    let (Some(group), Some(action)) = (args.first(), args.get(1)) else {
        let _ = writeln!(err, "{USAGE}");
        return EXIT_USAGE;
    };
    let a = match parse(&args[2..]) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "papersd: {e}\n{USAGE}");
            return EXIT_USAGE;
        }
    };
    let Some(library) = a.library.clone() else {
        let _ = writeln!(err, "papersd: --library is required\n{USAGE}");
        return EXIT_USAGE;
    };
    let now = crate::ipc::auth::now_ms();
    let command = match (group.as_str(), action.as_str()) {
        ("token", "issue") => {
            let (Some(label), Some(kind)) = (a.label.clone(), a.kind) else {
                let _ = writeln!(err, "papersd: --label and --kind are required\n{USAGE}");
                return EXIT_USAGE;
            };
            // Decide before anything is issued: a refused print must not leave a live token.
            if !stdout_is_tty && !a.allow_non_tty {
                let _ = writeln!(
                    err,
                    "papersd: refusing to print a new token to a non-terminal stdout; \
                     re-run in a terminal, or pass --allow-non-tty if stdout goes somewhere private"
                );
                return EXIT_REFUSED;
            }
            let mut scopes = a.scopes.clone();
            if a.mcp_read_only {
                scopes.extend(profiles::mcp_read_only());
            }
            ApiCommand::IssueToken(IssueToken {
                label,
                kind,
                scopes,
                expires_at_ms: a
                    .expires_days
                    .map(|d| now.saturating_add(d.saturating_mul(86_400_000))),
            })
        }
        ("token", "list") => ApiCommand::ListTokens,
        ("token", "revoke") => match a.id {
            Some(id) => ApiCommand::Revoke(id),
            None => {
                let _ = writeln!(err, "papersd: --id is required");
                return EXIT_USAGE;
            }
        },
        ("token", "elevate") => match (a.id, a.minutes) {
            (Some(grant), Some(minutes)) => ApiCommand::Elevate {
                grant,
                duration_ms: minutes.saturating_mul(60_000),
            },
            _ => {
                let _ = writeln!(err, "papersd: --id and --minutes are required");
                return EXIT_USAGE;
            }
        },
        ("api", "show") => ApiCommand::Overview,
        ("api", "set") => ApiCommand::Overview, // read-modify-write below
        _ => {
            let _ = writeln!(err, "{USAGE}");
            return EXIT_USAGE;
        }
    };
    let mut session = match session(&library, a.runtime_dir.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(err, "papersd: cannot reach the library daemon: {e}");
            return EXIT_ERROR;
        }
    };
    let mut reply = call(&mut session, command);
    if (group.as_str(), action.as_str()) == ("api", "set")
        && let Ok(ApiReply::Overview(overview)) = &reply
    {
        let mut settings = overview.settings.clone();
        if let Some(enable) = a.enable {
            settings.enabled = enable;
        }
        if let Some(bind) = a.bind {
            settings.bind = bind;
        }
        if let Some(port) = a.port {
            settings.port = port;
        }
        reply = call(&mut session, ApiCommand::SaveSettings(settings));
    }
    match reply {
        Ok(ApiReply::Issued {
            token,
            credential,
            stored_in,
            secret_account,
        }) => {
            // The secret is the only stdout line; never logged anywhere else.
            let _ = writeln!(out, "{}", credential.0);
            let _ = writeln!(
                err,
                "Issued {} token {} \"{}\" scopes [{}] expires {}.\nShown once. Stored in: {} (account {secret_account}).",
                token.kind.name(),
                token.id,
                token.label,
                token
                    .scopes
                    .iter()
                    .map(|s| s.name())
                    .collect::<Vec<_>>()
                    .join(" "),
                token.expires_at_ms.map_or("never".into(), |at| format!(
                    "in {} days",
                    (at - now) / 86_400_000
                )),
                stored_in.map_or("not stored (secret store unavailable)", |b| b.describe()),
            );
            EXIT_OK
        }
        Ok(ApiReply::Tokens(tokens)) => {
            for t in tokens {
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t[{}]\tcreated_day={}\texpires_day={}\tlast_used_day={}",
                    t.id,
                    t.kind.name(),
                    if t.revoked { "revoked" } else { "active" },
                    t.label,
                    t.scopes
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(" "),
                    day(Some(t.created_at_ms)),
                    day(t.expires_at_ms),
                    day(t.last_used_at_ms),
                );
            }
            EXIT_OK
        }
        Ok(ApiReply::Revoked(id)) => {
            let _ = writeln!(
                err,
                "Revoked {id}; live sessions are refused from their next request."
            );
            EXIT_OK
        }
        Ok(ApiReply::Updated(t)) => {
            let _ = writeln!(
                err,
                "Updated {}: elevated until {:?} (epoch ms)",
                t.id, t.elevated_until_ms
            );
            EXIT_OK
        }
        Ok(ApiReply::Overview(o)) => {
            let _ = writeln!(
                out,
                "enabled={} bind={} port={} export_dirs={} secret_store={:?} listener={:?}",
                o.settings.enabled,
                o.settings.bind.as_str(),
                o.settings.port,
                o.settings.export_dirs.len(),
                o.secret_backend,
                o.listener
            );
            EXIT_OK
        }
        Ok(_) => EXIT_OK,
        Err((code, message)) => {
            let _ = writeln!(err, "papersd: {message}");
            code
        }
    }
}
