//! Connection to the library's papersd over the authenticated local IPC (#25).
//!
//! The CLI never opens the database: every read and write is a daemon `Command`. When nothing
//! answers on the socket, `papersd` is started from the directory of this executable (never a
//! PATH search), as the GUI does, and exits on its own once idle.
use crate::{
    credentials::{CLIENT_KIND, CliCredential},
    output::{CliError, Kind},
};
use cm_protocol::{
    client::{
        ClientError, Stream, handshake_within,
        local::{DaemonPaths, Launch, LocalConnector},
    },
    ipc::{
        CapabilitySet, ClientFrame, Command, Denial, FrameError, HandshakeRejection,
        MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, Reply, ServerFrame, Welcome, WireError, read_frame,
        write_frame,
    },
    library::ServiceError,
};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

pub const HANDSHAKE: Duration = Duration::from_secs(10);
/// Library CRUD, queries, organization, DOI resolve (same limit as the GUI client).
pub const INTERACTIVE: Duration = Duration::from_secs(5 * 60);
/// Import, export and PDF work.
pub const LONG: Duration = Duration::from_secs(60 * 60);
/// Status and other daemon-thread answers.
pub const CONTROL: Duration = Duration::from_secs(30);
/// How long a lazily started daemon may take to accept.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Which library and how to reach (or start) its daemon.
#[derive(Debug, Clone)]
pub struct Target {
    pub paths: DaemonPaths,
    pub uid: u32,
    /// Start papersd when nothing answers.
    pub start: bool,
    /// `--idle-timeout-ms` for a daemon this CLI starts.
    pub idle_timeout_ms: Option<u64>,
    pub credential: CliCredential,
}
impl Target {
    pub fn new(
        library: &Path,
        runtime_dir: Option<&Path>,
        start: bool,
        idle_timeout_ms: Option<u64>,
        credential: CliCredential,
    ) -> Result<Self, CliError> {
        let uid = euid();
        let library = absolute(library)?;
        let runtime_dir = runtime_dir.map(absolute).transpose()?;
        let paths = DaemonPaths::new(&library, runtime_dir.as_deref(), uid)
            .map_err(|e| CliError::new(Kind::InvalidInput, format!("library/runtime path: {e}")))?;
        Ok(Self {
            paths,
            uid,
            start,
            idle_timeout_ms,
            credential,
        })
    }
    /// Library paths, launch and credential for an embedded MCP server (`papers mcp`). The MCP
    /// server requests its own session ceiling, so nothing is requested here.
    pub fn mcp_connector(&self) -> Result<LocalConnector, CliError> {
        self.connector(CapabilitySet::new(), self.start)
    }
    fn connector(&self, requested: CapabilitySet, start: bool) -> Result<LocalConnector, CliError> {
        let mut connector = LocalConnector::new(self.paths.clone(), self.uid, CLIENT_KIND);
        connector.requested = Some(requested);
        connector.credential = self.credential.source(self.uid)?;
        if start {
            let mut extra_args = Vec::new();
            if let Some(ms) = self.idle_timeout_ms {
                extra_args.push("--idle-timeout-ms".into());
                extra_args.push(ms.to_string().into());
            }
            connector.launch = Some(Launch {
                papersd: papersd_path()?,
                extra_args,
                timeout: LAUNCH_TIMEOUT,
            });
        }
        Ok(connector)
    }
}

/// `papersd` next to this executable (`<prefix>/bin`, `.app/Contents/MacOS`, `target/<profile>`).
/// Symlinks are resolved first: on macOS `current_exe()` returns the path that was executed,
/// so `papers` linked onto PATH (#34/#31) must still find the bundle's own `papersd`.
pub fn papersd_path() -> Result<PathBuf, CliError> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| {
            CliError::new(
                Kind::DaemonUnavailable,
                format!("cannot locate papers: {e}"),
            )
        })?;
    let papersd = exe.with_file_name("papersd");
    if !papersd.is_file() {
        return Err(CliError::new(
            Kind::DaemonUnavailable,
            format!(
                "papersd not found next to this executable ({}); install it alongside papers",
                papersd.display()
            ),
        ));
    }
    Ok(papersd)
}

pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Joins a relative path onto the current directory (no symlink resolution).
pub fn absolute(path: &Path) -> Result<PathBuf, CliError> {
    std::path::absolute(path)
        .map_err(|e| CliError::new(Kind::InvalidInput, format!("{}: {e}", path.display())))
}

/// Request/response transport used by every subcommand (a fake implements it in unit tests).
pub trait Daemon {
    fn call(&mut self, command: Command, timeout: Duration) -> Result<Reply, CliError>;
}

/// One authenticated session. Requests are sequential; PDF progress frames are skipped.
pub struct Connection {
    stream: Box<dyn Stream>,
    next: u64,
    pub welcome: Welcome,
    /// PID of the process serving the socket (`SO_PEERCRED`, Linux only).
    pub peer_pid: Option<u32>,
}

/// Why a handshake failed: a daemon that vanished right after accepting earns one retry.
enum HandshakeFailure {
    Closed(CliError),
    Failed(CliError),
}
impl HandshakeFailure {
    fn into_error(self) -> CliError {
        match self {
            Self::Closed(e) | Self::Failed(e) => e,
        }
    }
}

impl Connection {
    /// Connects (starting papersd if allowed) and authenticates with `requested` capabilities.
    pub fn open(target: &Target, requested: CapabilitySet) -> Result<Self, CliError> {
        let connector = target.connector(requested, target.start)?;
        let attempt = || {
            let stream = connector
                .connect_or_start()
                .map_err(|e| HandshakeFailure::Failed(connect_error(e)))?;
            Self::handshake(&connector, stream)
        };
        match attempt() {
            // The daemon accepted just before its idle exit: one fresh connect (and start).
            Err(HandshakeFailure::Closed(_)) => attempt().map_err(HandshakeFailure::into_error),
            other => other.map_err(HandshakeFailure::into_error),
        }
    }
    /// Connects only if a daemon is already serving the library. `Ok(None)` = not running
    /// (no socket or connection refused); any other failure is an error.
    pub fn probe(target: &Target, requested: CapabilitySet) -> Result<Option<Self>, CliError> {
        let connector = target.connector(requested, false)?;
        match connector.connect_or_start() {
            Err(e) if not_running(&e) => Ok(None),
            Err(e) => Err(connect_error(e)),
            Ok(stream) => Self::handshake(&connector, stream)
                .map(Some)
                .map_err(HandshakeFailure::into_error),
        }
    }
    fn handshake(
        connector: &LocalConnector,
        stream: std::os::unix::net::UnixStream,
    ) -> Result<Self, HandshakeFailure> {
        let peer_pid = peer_pid(&stream);
        let hello = connector.hello().map_err(|e| {
            HandshakeFailure::Failed(CliError::new(
                Kind::PermissionDenied,
                format!("cannot read the library's credential: {e}"),
            ))
        })?;
        let mut stream: Box<dyn Stream> = Box::new(stream);
        let welcome =
            handshake_within(stream.as_mut(), hello, HANDSHAKE).map_err(handshake_failure)?;
        Ok(Self {
            stream,
            next: 1,
            welcome,
            peer_pid,
        })
    }
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    rustix::net::sockopt::socket_peercred(stream)
        .ok()
        .map(|cred| cred.pid.as_raw_nonzero().get() as u32)
}
#[cfg(not(target_os = "linux"))]
fn peer_pid(_stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    None
}

impl Daemon for Connection {
    fn call(&mut self, command: Command, timeout: Duration) -> Result<Reply, CliError> {
        let id = self.next;
        self.next += 1;
        self.stream
            .set_io_timeouts(Some(timeout), Some(CONTROL.max(Duration::from_secs(1))))
            .map_err(|e| client_error(ClientError::Io(e)))?;
        write_frame(
            self.stream.as_mut(),
            &ClientFrame::Request { id, command },
            MAX_REQUEST_FRAME,
        )
        .map_err(|e| client_error(ClientError::Frame(e)))?;
        loop {
            match read_frame::<_, ServerFrame>(self.stream.as_mut(), MAX_RESPONSE_FRAME) {
                Ok(Some(ServerFrame::Response { id: got, result })) if got == id => {
                    return result.map_err(wire_error);
                }
                Ok(Some(ServerFrame::Response { .. } | ServerFrame::PdfProgress { .. })) => {}
                Ok(Some(_)) => {
                    return Err(CliError::internal(
                        "daemon protocol error: unexpected frame",
                    ));
                }
                Ok(None) => {
                    return Err(CliError::new(
                        Kind::DaemonUnavailable,
                        "daemon closed the connection; result unknown, check with `papers show`/`papers list`",
                    ));
                }
                Err(FrameError::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    let _ = write_frame(
                        self.stream.as_mut(),
                        &ClientFrame::Cancel { id },
                        MAX_REQUEST_FRAME,
                    );
                    return Err(CliError::new(
                        Kind::DaemonUnavailable,
                        format!(
                            "daemon did not answer within {} s; result unknown, check with `papers show`/`papers list`",
                            timeout.as_secs()
                        ),
                    ));
                }
                Err(e) => return Err(client_error(ClientError::Frame(e))),
            }
        }
    }
}

fn not_running(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn connect_error(e: io::Error) -> CliError {
    if e.kind() == io::ErrorKind::PermissionDenied {
        // Unsafe runtime directory/socket ownership: refuse rather than talk to it.
        return CliError::new(
            Kind::PermissionDenied,
            format!("refusing the daemon endpoint: {e}"),
        );
    }
    CliError::new(Kind::DaemonUnavailable, format!("papersd unavailable: {e}"))
}

/// A failure after the socket connected during Hello/Welcome. As in
/// `cm_protocol::client::connect_and_handshake`, any transport error except a timeout means the
/// daemon went away (idle-exit race; platforms disagree on the errno) and earns one retry.
fn handshake_failure(e: ClientError) -> HandshakeFailure {
    let closed = match &e {
        ClientError::Protocol(reason) => reason.contains("closed during handshake"),
        ClientError::Io(e) | ClientError::Frame(FrameError::Io(e)) => !matches!(
            e.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ),
        _ => false,
    };
    let error = client_error(e);
    if closed {
        HandshakeFailure::Closed(error)
    } else {
        HandshakeFailure::Failed(error)
    }
}

pub fn client_error(e: ClientError) -> CliError {
    match e {
        ClientError::Rejected(HandshakeRejection::Unauthenticated) => CliError::new(
            Kind::PermissionDenied,
            "daemon rejected the credential (unauthenticated, revoked or expired)",
        ),
        ClientError::Rejected(HandshakeRejection::PeerRejected) => {
            CliError::new(Kind::PermissionDenied, "daemon rejected this OS user")
        }
        ClientError::Rejected(HandshakeRejection::VersionMismatch { supported }) => {
            CliError::internal(format!(
                "protocol version mismatch: daemon supports {supported}; install matching papers/papersd"
            ))
        }
        ClientError::Rejected(r) => CliError::new(
            Kind::DaemonUnavailable,
            format!("daemon rejected the connection: {r:?}"),
        ),
        e => CliError::new(Kind::DaemonUnavailable, e.to_string()),
    }
}

/// A service error computed locally with the shared core policy (e.g. the #33 PDF proposal).
pub fn wire_error_from_service(e: ServiceError) -> CliError {
    wire_error(WireError::Service(e))
}

pub fn wire_error(e: WireError) -> CliError {
    match e {
        WireError::Service(ServiceError::NotFound) => {
            CliError::new(Kind::NotFound, "record not found")
        }
        WireError::Service(ServiceError::Conflict) => CliError::new(
            Kind::Conflict,
            "conflicts with existing data or changed state; preview again",
        ),
        WireError::Service(ServiceError::InvalidInput(reason)) => {
            CliError::new(Kind::InvalidInput, reason)
        }
        WireError::Service(ServiceError::Unavailable) | WireError::Unavailable => {
            CliError::new(Kind::DaemonUnavailable, "service unavailable")
        }
        WireError::Busy => CliError::new(Kind::DaemonUnavailable, "daemon busy; retry"),
        WireError::Denied(denial) => CliError::new(
            Kind::PermissionDenied,
            match denial {
                Denial::Forbidden(op) => {
                    format!("permission denied: this session's grant does not allow {op:?}")
                }
                other => format!("permission denied: {other}"),
            },
        ),
        WireError::TooLarge => CliError::new(Kind::InvalidInput, "request or response too large"),
        WireError::Cancelled => CliError::internal("request cancelled"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_protocol::ipc::Operation;

    #[test]
    fn daemon_errors_map_to_semantic_kinds() {
        assert_eq!(
            wire_error(WireError::Denied(Denial::Forbidden(
                Operation::ImportCommit
            )))
            .kind,
            Kind::PermissionDenied
        );
        assert_eq!(
            wire_error(WireError::Denied(Denial::Revoked)).kind,
            Kind::PermissionDenied
        );
        assert_eq!(
            wire_error(ServiceError::NotFound.into()).kind,
            Kind::NotFound
        );
        assert_eq!(
            wire_error(ServiceError::Conflict.into()).kind,
            Kind::Conflict
        );
        assert_eq!(
            wire_error(ServiceError::InvalidInput("x".into()).into()).kind,
            Kind::InvalidInput
        );
        assert_eq!(
            wire_error(WireError::Unavailable).kind,
            Kind::DaemonUnavailable
        );
        assert_eq!(
            client_error(ClientError::Rejected(HandshakeRejection::Unauthenticated)).kind,
            Kind::PermissionDenied
        );
        // Handshake transport errors (macOS: ENOTCONN/EINVAL too) earn the one retry; timeouts
        // and rejections do not.
        let closed = |e| matches!(handshake_failure(e), HandshakeFailure::Closed(_));
        for kind in [
            io::ErrorKind::NotConnected,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::BrokenPipe,
        ] {
            assert!(closed(ClientError::Io(io::Error::from(kind))));
        }
        assert!(closed(ClientError::Protocol(
            "daemon closed during handshake"
        )));
        assert!(!closed(ClientError::Io(io::Error::from(
            io::ErrorKind::TimedOut
        ))));
        assert!(!closed(ClientError::Rejected(
            HandshakeRejection::Unauthenticated
        )));
        assert_eq!(
            connect_error(io::Error::from(io::ErrorKind::ConnectionRefused)).kind,
            Kind::DaemonUnavailable
        );
    }
}
