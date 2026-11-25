//! Citation Manager MCP server (#29): a thin rmcp stdio adapter over papersd IPC (#25).
//!
//! Read-only by default. The server never opens the library itself and never uses REST: each
//! tool is a composition of authenticated IPC commands that papersd authorizes against the
//! live grant of the presented credential (default-deny, revocation applies to live sessions).
//!
//! Entry points (the contract `papers mcp` from #28 uses):
//! - [`main`]: `papers-mcp [ARGS]` ≡ `papers mcp [ARGS]` — parses ARGS, serves stdio or runs a
//!   grant subcommand, returns the process exit code.
//! - [`run_stdio_with`]: serve stdio with a [`StdioConfig`] the embedding CLI already resolved
//!   (library/runtime paths or a `LocalConnector`, credential source); read-only session by
//!   default.
//! - [`run_stdio`]: thin wrapper, configured from the environment only (`PAPERS_LIBRARY`, ...).
//!
//! stdout carries MCP protocol frames only; every diagnostic goes to stderr ([`log`]).
pub mod credential;
pub mod daemon;
pub mod log;
mod schema;
pub mod server;
pub mod tools;
pub mod transport;

use cm_protocol::client::local::{CredentialSource, LocalConnector};
use cm_protocol::{
    client::local::{DaemonPaths, Launch},
    ipc::{Credential, GrantId},
};
use credential::{CredentialStore, FileCredential, Profile};
use rmcp::ServiceExt;
use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub const ENV_LIBRARY: &str = "PAPERS_LIBRARY";
pub const ENV_RUNTIME_DIR: &str = "PAPERS_RUNTIME_DIR";
/// How long papersd may take to start on demand.
pub const START_TIMEOUT: Duration = Duration::from_secs(20);
/// Default: close the daemon connection after this long without calls.
pub const IDLE_DISCONNECT: Duration = Duration::from_secs(60);

pub const USAGE: &str = "\
papers-mcp: Citation Manager MCP server over stdio (also `papers mcp`).

Serve (started by an AI client; stdout is MCP protocol only, logs go to stderr):
  papers-mcp [serve] --library ABS [--runtime-dir ABS] [--credential-file ABS]
                     [--read-only] [--papersd ABS] [--daemon-idle-timeout-ms N]
                     [--idle-disconnect-ms N]
      Tools are read-only unless the credential's grant was approved for writes.
      --read-only narrows the session to reads even with a write grant.

Grants (run by the library owner in a terminal; uses the owner's desktop credential):
  papers-mcp grant  --library ABS [--write] [--export] [--ttl 8h] [--credential-file ABS] ...
      Issue an MCP grant (read-only unless --write; --export allows export_citations)
      and store its secret in the credential file (0600). Replaces and revokes the grant
      the file held before; a running server switches on its next call. Expiry: write or
      export grants 8h by default (max 90d), read-only 90d (max 365d); --ttl Nm|Nh|Nd.
  papers-mcp revoke --library ABS [--grant UUID | --credential-file ABS] ...
      Revoke a grant (default: the credential file's, which is then removed).
  papers-mcp grants --library ABS ...
      List MCP grants as JSON.

Options and environment:
  --library ABS          library directory ($PAPERS_LIBRARY)
  --runtime-dir ABS      private socket directory ($PAPERS_RUNTIME_DIR; default per user)
  --credential-file ABS  MCP credential file ($PAPERS_MCP_CREDENTIAL_FILE;
                         default <library>/daemon/mcp.credential). Secrets are never
                         accepted on the command line or in environment values.
  --papersd ABS          papersd to start on demand (default: next to this executable)
  PAPERS_MCP_LOG=off|error|warn|info|debug   stderr verbosity (default info)
";

/// Why the server could not run. `Display` never contains secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StdioError {
    /// The MCP server could not be started (configuration or runtime failure).
    Unavailable(String),
    /// Invalid arguments or environment.
    Usage(String),
}
impl fmt::Display for StdioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "MCP server unavailable: {reason}"),
            Self::Usage(reason) => write!(f, "{reason}"),
        }
    }
}
impl std::error::Error for StdioError {}

/// Where an embedded server gets its MCP secret. Never the owner's desktop credential.
#[derive(Clone)]
pub enum McpCredential {
    /// `$PAPERS_MCP_CREDENTIAL_FILE`, else `<library>/daemon/mcp.credential` (0600 file).
    DefaultFile,
    /// An explicit private credential file (absolute path).
    File(PathBuf),
    /// A secret the embedding process already resolved (kept in memory only).
    Fixed(Credential),
    /// Any other source (#26 profiles/keychain).
    Store(Arc<dyn CredentialStore>),
}

/// Configuration of [`run_stdio_with`]: an embedding CLI (`papers mcp`, #28) passes the
/// library/connection it already resolved. Defaults: papersd next to the current executable,
/// [`McpCredential::DefaultFile`], and a **read-only session** (only the MCP read-only preset
/// is requested; set `read_only = false` to let an owner-approved write/export grant act).
#[derive(Clone)]
pub struct StdioConfig {
    pub paths: DaemonPaths,
    pub uid: u32,
    /// `None`: never start papersd (connect to a running one only).
    pub launch: Option<Launch>,
    pub credential: McpCredential,
    /// Request only the read-only capabilities even if the grant allows more.
    pub read_only: bool,
    /// Close an unused daemon connection after this long (`None`: keep it).
    pub idle_disconnect: Option<Duration>,
}
impl StdioConfig {
    /// `library` (and `runtime_dir`, if given) must be absolute.
    pub fn new(library: &Path, runtime_dir: Option<&Path>) -> Result<Self, StdioError> {
        let uid = euid();
        let paths = DaemonPaths::new(library, runtime_dir, uid)
            .map_err(|e| StdioError::Usage(format!("library/runtime directory: {e}")))?;
        Ok(Self {
            paths,
            uid,
            launch: sibling_launch(None, None),
            credential: McpCredential::DefaultFile,
            read_only: true,
            idle_disconnect: Some(IDLE_DISCONNECT),
        })
    }
    /// Reuses a connector's library paths, UID and launch settings. Its credential is used
    /// only when it is an explicit (issued) one; a `DesktopBootstrap` (owner) source is never
    /// presented by the MCP server — the default MCP credential file is used instead.
    pub fn from_connector(connector: &LocalConnector) -> Self {
        Self {
            paths: connector.paths.clone(),
            uid: connector.uid,
            launch: connector.launch.clone(),
            credential: match &connector.credential {
                CredentialSource::Explicit(c) => McpCredential::Fixed(c.clone()),
                CredentialSource::DesktopBootstrap => McpCredential::DefaultFile,
            },
            read_only: true,
            idle_disconnect: Some(IDLE_DISCONNECT),
        }
    }
}

/// A secret held in memory (from [`McpCredential::Fixed`]).
struct FixedCredential(Credential);
impl CredentialStore for FixedCredential {
    fn load(&self) -> Result<Credential, String> {
        Ok(self.0.clone())
    }
    fn describe(&self) -> String {
        "credential supplied by the embedding process".into()
    }
}

fn sibling_launch(papersd: Option<PathBuf>, daemon_idle_ms: Option<u64>) -> Option<Launch> {
    let papersd = papersd.or_else(|| {
        std::env::current_exe()
            .ok()
            .map(|exe| exe.with_file_name("papersd"))
    });
    let launch = papersd.filter(|p| p.is_file()).map(|papersd| Launch {
        papersd,
        extra_args: daemon_idle_ms
            .map(|ms| vec!["--idle-timeout-ms".into(), ms.to_string().into()])
            .unwrap_or_default(),
        timeout: START_TIMEOUT,
    });
    if launch.is_none() {
        log::warn(
            "papersd was not found next to this executable; only an already running library \
             service can be used",
        );
    }
    launch
}

pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[derive(Default)]
struct Options {
    command: String,
    library: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    credential_file: Option<PathBuf>,
    papersd: Option<PathBuf>,
    daemon_idle_ms: Option<u64>,
    idle_disconnect_ms: Option<u64>,
    read_only: bool,
    write: bool,
    export: bool,
    grant: Option<GrantId>,
    ttl_ms: Option<i64>,
}

fn absolute(flag: &str, value: Option<OsString>) -> Result<PathBuf, StdioError> {
    let path =
        PathBuf::from(value.ok_or_else(|| StdioError::Usage(format!("{flag} needs a value")))?);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(StdioError::Usage(format!(
            "{flag} must be an absolute path"
        )))
    }
}

fn number(flag: &str, value: Option<OsString>) -> Result<u64, StdioError> {
    value
        .and_then(|v| v.into_string().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| StdioError::Usage(format!("{flag} needs a non-negative integer")))
}

fn parse(
    args: Vec<OsString>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Options, StdioError> {
    let mut o = Options::default();
    let mut args = args.into_iter().peekable();
    o.command = match args.peek().and_then(|a| a.to_str()) {
        Some(c @ ("serve" | "grant" | "revoke" | "grants" | "help" | "--help" | "-h")) => {
            let c = c.to_owned();
            args.next();
            c
        }
        _ => "serve".into(),
    };
    let allowed: &[&str] = match o.command.as_str() {
        "serve" => &["--read-only", "--idle-disconnect-ms"],
        "grant" => &["--write", "--export", "--ttl"],
        "revoke" => &["--grant"],
        _ => &[],
    };
    while let Some(flag) = args.next() {
        let flag = flag
            .into_string()
            .map_err(|_| StdioError::Usage("arguments must be UTF-8".into()))?;
        let common = [
            "--library",
            "--runtime-dir",
            "--credential-file",
            "--papersd",
            "--daemon-idle-timeout-ms",
        ];
        if matches!(flag.as_str(), "--credential" | "--token" | "--secret") {
            return Err(StdioError::Usage(
                "secrets are never accepted on the command line; use --credential-file".into(),
            ));
        }
        if !common.contains(&flag.as_str()) && !allowed.contains(&flag.as_str()) {
            return Err(StdioError::Usage(format!(
                "unknown option {flag} for {}",
                o.command
            )));
        }
        match flag.as_str() {
            "--library" => o.library = Some(absolute(&flag, args.next())?),
            "--runtime-dir" => o.runtime_dir = Some(absolute(&flag, args.next())?),
            "--credential-file" => o.credential_file = Some(absolute(&flag, args.next())?),
            "--papersd" => o.papersd = Some(absolute(&flag, args.next())?),
            "--daemon-idle-timeout-ms" => o.daemon_idle_ms = Some(number(&flag, args.next())?),
            "--idle-disconnect-ms" => o.idle_disconnect_ms = Some(number(&flag, args.next())?),
            "--read-only" => o.read_only = true,
            "--write" => o.write = true,
            "--export" => o.export = true,
            "--ttl" => {
                let raw = args
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .ok_or_else(|| StdioError::Usage("--ttl needs a duration (e.g. 8h)".into()))?;
                o.ttl_ms = Some(credential::parse_ttl(&raw).map_err(StdioError::Usage)?);
            }
            "--grant" => {
                let raw = args
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .ok_or_else(|| StdioError::Usage("--grant needs a UUID".into()))?;
                o.grant =
                    Some(GrantId(uuid::Uuid::parse_str(&raw).map_err(|_| {
                        StdioError::Usage("--grant needs a UUID".into())
                    })?));
            }
            _ => unreachable!("checked above"),
        }
    }
    if o.library.is_none()
        && let Some(value) = env(ENV_LIBRARY).filter(|v| !v.is_empty())
    {
        o.library = Some(absolute(ENV_LIBRARY, Some(value))?);
    }
    if o.runtime_dir.is_none()
        && let Some(value) = env(ENV_RUNTIME_DIR).filter(|v| !v.is_empty())
    {
        o.runtime_dir = Some(absolute(ENV_RUNTIME_DIR, Some(value))?);
    }
    Ok(o)
}

struct Resolved {
    paths: DaemonPaths,
    uid: u32,
    launch: Option<Launch>,
    file: FileCredential,
}

fn resolve(o: &Options, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Resolved, StdioError> {
    let library = o
        .library
        .clone()
        .ok_or_else(|| StdioError::Usage(format!("--library (or ${ENV_LIBRARY}) is required")))?;
    let uid = euid();
    let paths = DaemonPaths::new(&library, o.runtime_dir.as_deref(), uid)
        .map_err(|e| StdioError::Usage(format!("library/runtime directory: {e}")))?;
    let launch = sibling_launch(o.papersd.clone(), o.daemon_idle_ms);
    let file = FileCredential::resolve(
        o.credential_file.clone(),
        env(credential::ENV_CREDENTIAL_FILE),
        &paths,
        uid,
    )
    .map_err(StdioError::Usage)?;
    Ok(Resolved {
        paths,
        uid,
        launch,
        file,
    })
}

/// `papers-mcp ARGS` / `papers mcp ARGS`. Returns the process exit code
/// (0 ok, 1 failure, 2 usage).
pub fn main(args: Vec<OsString>) -> u8 {
    log::init_from_env();
    let env = |key: &str| std::env::var_os(key);
    let outcome = parse(args, &env).and_then(|o| run(o, &env));
    match outcome {
        Ok(()) => 0,
        Err(StdioError::Usage(reason)) if reason.is_empty() => {
            eprint!("{USAGE}");
            0
        }
        Err(StdioError::Usage(reason)) => {
            eprintln!("papers-mcp: {reason}\n\n{USAGE}");
            2
        }
        Err(e) => {
            log::error(&e.to_string());
            1
        }
    }
}

fn run(o: Options, env: &dyn Fn(&str) -> Option<OsString>) -> Result<(), StdioError> {
    if matches!(o.command.as_str(), "help" | "--help" | "-h") {
        return Err(StdioError::Usage(String::new()));
    }
    let r = resolve(&o, env)?;
    let failed = StdioError::Unavailable;
    match o.command.as_str() {
        "grant" => {
            let profile = Profile {
                write: o.write,
                export: o.export,
            };
            let grant = credential::issue(&r.paths, r.uid, r.launch, profile, o.ttl_ms, &r.file)
                .map_err(failed)?;
            print_json(&serde_json::json!({
                "grant_id": grant.id.0,
                "label": grant.label,
                "capabilities": grant.capabilities,
                "expires_at_ms": grant.expires_at_ms,
                "credential_file": r.file.path,
            }));
            Ok(())
        }
        "revoke" => {
            let revoked =
                credential::revoke(&r.paths, r.uid, r.launch, o.grant, &r.file).map_err(failed)?;
            print_json(&serde_json::json!({"revoked": revoked.0}));
            Ok(())
        }
        "grants" => {
            let grants = credential::list(&r.paths, r.uid, r.launch).map_err(failed)?;
            print_json(&serde_json::json!({"grants": grants}));
            Ok(())
        }
        // `papers-mcp` serve: the grant decides (read-only unless the owner approved more);
        // `--read-only` additionally narrows the session.
        _ => run_stdio_with(StdioConfig {
            paths: r.paths,
            uid: r.uid,
            launch: r.launch,
            credential: McpCredential::Store(Arc::new(r.file)),
            read_only: o.read_only,
            idle_disconnect: match o.idle_disconnect_ms {
                Some(0) => None,
                Some(ms) => Some(Duration::from_millis(ms)),
                None => Some(IDLE_DISCONNECT),
            },
        }),
    }
}

/// Grant subcommands print one JSON document (they are not the stdio server).
fn print_json(value: &serde_json::Value) {
    println!("{value}");
}

/// Thin wrapper over [`run_stdio_with`] configured from the environment only:
/// `$PAPERS_LIBRARY` (required), `$PAPERS_RUNTIME_DIR`, `$PAPERS_MCP_CREDENTIAL_FILE`;
/// papersd next to this executable; read-only session ([`StdioConfig::new`] defaults).
/// Returns when stdin closes.
pub fn run_stdio() -> Result<(), StdioError> {
    log::init_from_env();
    let env = |key: &str| std::env::var_os(key);
    let options = parse(Vec::new(), &env)?;
    let library = options
        .library
        .ok_or_else(|| StdioError::Usage(format!("${ENV_LIBRARY} is required")))?;
    run_stdio_with(StdioConfig::new(&library, options.runtime_dir.as_deref())?)
}

/// Serves MCP over this process's stdin/stdout until stdin closes. stdout carries protocol
/// frames only; diagnostics go to stderr. Blocks the calling thread (owns its runtime).
pub fn run_stdio_with(config: StdioConfig) -> Result<(), StdioError> {
    let credential: Arc<dyn CredentialStore> = match config.credential {
        McpCredential::DefaultFile | McpCredential::File(_) => {
            let explicit = match &config.credential {
                McpCredential::File(path) => Some(path.clone()),
                _ => None,
            };
            Arc::new(
                FileCredential::resolve(
                    explicit,
                    std::env::var_os(credential::ENV_CREDENTIAL_FILE),
                    &config.paths,
                    config.uid,
                )
                .map_err(StdioError::Usage)?,
            )
        }
        McpCredential::Fixed(secret) => Arc::new(FixedCredential(secret)),
        McpCredential::Store(store) => store,
    };
    serve(ServeConfig {
        paths: config.paths,
        uid: config.uid,
        launch: config.launch,
        credential,
        read_only: config.read_only,
        idle_disconnect: config.idle_disconnect,
    })
}

struct ServeConfig {
    paths: DaemonPaths,
    uid: u32,
    launch: Option<Launch>,
    credential: Arc<dyn CredentialStore>,
    read_only: bool,
    idle_disconnect: Option<Duration>,
}

fn serve(config: ServeConfig) -> Result<(), StdioError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("papers-mcp")
        .enable_all()
        .build()
        .map_err(|e| StdioError::Unavailable(format!("async runtime: {e}")))?;
    log::info(&format!(
        "serving MCP on stdio for library {} ({}; credential {})",
        config.paths.library.display(),
        if config.read_only {
            "read-only session"
        } else {
            "read-only unless the grant allows writes"
        },
        config.credential.describe()
    ));
    let result = runtime.block_on(serve_async(config, tokio::io::stdin(), tokio::io::stdout()));
    // stdin may still be blocked in a reader thread; do not wait for it.
    runtime.shutdown_timeout(Duration::from_millis(200));
    result
}

/// The server over arbitrary byte streams (used by `serve` with stdin/stdout).
async fn serve_async<R, W>(config: ServeConfig, input: R, output: W) -> Result<(), StdioError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let client = daemon::DaemonClient::new(
        daemon::ClientConfig {
            paths: config.paths,
            uid: config.uid,
            launch: config.launch,
            requested: credential::session_ceiling(config.read_only),
            idle_disconnect: config.idle_disconnect,
        },
        config.credential,
    );
    let (out, writer) = transport::writer_task(output);
    let transport = transport::LineTransport::new(tokio::io::BufReader::new(input), out);
    let outcome = match server::McpServer::new(client).serve(transport).await {
        Ok(running) => match running.waiting().await {
            Ok(reason) => {
                log::info(&format!("stdin closed; exiting ({reason:?})"));
                Ok(())
            }
            Err(e) => Err(StdioError::Unavailable(e.to_string())),
        },
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => {
            log::info("stdin closed before initialization; exiting");
            Ok(())
        }
        Err(e) => Err(StdioError::Unavailable(e.to_string())),
    };
    // Everything queued for stdout is written before returning.
    let _ = writer.await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }
    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn arguments_default_to_serving_and_reject_secrets_on_argv() {
        let o = parse(os(&["--library", "/l", "--read-only"]), &no_env).unwrap();
        assert_eq!(o.command, "serve");
        assert!(o.read_only);
        let o = parse(os(&["grant", "--library", "/l", "--write"]), &no_env).unwrap();
        assert_eq!(
            (o.command.as_str(), o.write, o.export),
            ("grant", true, false)
        );
        let o = parse(os(&["grant", "--library", "/l", "--ttl", "2h"]), &no_env).unwrap();
        assert_eq!(o.ttl_ms, Some(2 * credential::HOUR_MS));
        for bad in [
            &["--credential", "abc"][..],
            &["--token", "abc"],
            &["--library", "relative"],
            &["--write"],
            &["grant", "--read-only"],
            &["revoke", "--grant", "nope"],
            &["grant", "--ttl", "forever"],
            &["--ttl", "2h"],
        ] {
            assert!(parse(os(bad), &no_env).is_err(), "{bad:?}");
        }
        let env = |k: &str| (k == ENV_LIBRARY).then(|| OsString::from("/env/lib"));
        let o = parse(Vec::new(), &env).unwrap();
        assert_eq!(o.library, Some(PathBuf::from("/env/lib")));
        let rel = |k: &str| (k == ENV_LIBRARY).then(|| OsString::from("rel"));
        assert!(parse(Vec::new(), &rel).is_err());
    }

    #[test]
    fn a_missing_library_is_a_usage_error_not_a_fake_server() {
        let o = parse(Vec::new(), &no_env).unwrap();
        assert!(matches!(resolve(&o, &no_env), Err(StdioError::Usage(_))));
        assert!(
            StdioError::Unavailable("x".into())
                .to_string()
                .contains("unavailable")
        );
    }

    #[test]
    fn embedding_config_is_read_only_and_never_presents_the_owner_credential() {
        let library = Path::new("/tmp/cm-mcp-embed/lib");
        let runtime = Path::new("/tmp/cm-mcp-embed/run");
        let config = StdioConfig::new(library, Some(runtime)).unwrap();
        assert!(config.read_only);
        assert!(matches!(config.credential, McpCredential::DefaultFile));
        assert!(StdioConfig::new(Path::new("relative"), None).is_err());
        let owner = LocalConnector::new(config.paths.clone(), euid(), "cli");
        let from_owner = StdioConfig::from_connector(&owner);
        assert!(from_owner.read_only);
        assert!(matches!(from_owner.credential, McpCredential::DefaultFile));
        assert_eq!(from_owner.paths, config.paths);
        let mut issued = owner.clone();
        issued.credential = CredentialSource::Explicit(Credential("s".into()));
        assert!(matches!(
            StdioConfig::from_connector(&issued).credential,
            McpCredential::Fixed(Credential(ref s)) if s == "s"
        ));
    }
}
