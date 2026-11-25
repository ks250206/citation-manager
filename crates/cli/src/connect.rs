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
pub fn papersd_path() -> Result<PathBuf, CliError> {
    let exe = std::env::current_exe().map_err(|e| {
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
}
impl Connection {
    /// Connects (starting papersd if allowed) and authenticates with `requested` capabilities.
    pub fn open(target: &Target, requested: CapabilitySet) -> Result<Self, CliError> {
        let connector = target.connector(requested, target.start)?;
        Self::with(&connector)
    }
    /// Connects only if a daemon is already serving the library. `Ok(None)` = not running.
    pub fn probe(target: &Target, requested: CapabilitySet) -> Result<Option<Self>, CliError> {
        let connector = target.connector(requested, false)?;
        match connector.connect_or_start() {
            Err(e) if not_running(&e) => Ok(None),
            Err(e) => Err(connect_error(e)),
            Ok(stream) => Self::handshake(&connector, Box::new(stream)).map(Some),
        }
    }
    fn with(connector: &LocalConnector) -> Result<Self, CliError> {
        let attempt = || {
            let stream = connector.connect_or_start().map_err(connect_error)?;
            Self::handshake(connector, Box::new(stream))
        };
        match attempt() {
            // The daemon accepted just before its idle exit: one fresh connect (and start).
            Err(e) if e.message.contains(CLOSED) => attempt(),
            other => other,
        }
    }
    fn handshake(
        connector: &LocalConnector,
        mut stream: Box<dyn Stream>,
    ) -> Result<Self, CliError> {
        let hello = connector.hello().map_err(|e| {
            CliError::new(
                Kind::PermissionDenied,
                format!("cannot read the library's credential: {e}"),
            )
        })?;
        let welcome = handshake_within(stream.as_mut(), hello, HANDSHAKE).map_err(client_error)?;
        Ok(Self {
            stream,
            next: 1,
            welcome,
        })
    }
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
                        "daemon closed the connection",
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
                        format!("daemon did not answer within {} s", timeout.as_secs()),
                    ));
                }
                Err(e) => return Err(client_error(ClientError::Frame(e))),
            }
        }
    }
}

const CLOSED: &str = "closed during handshake";

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

pub fn client_error(e: ClientError) -> CliError {
    let closed = match &e {
        ClientError::Protocol(reason) => reason.contains("closed during handshake"),
        ClientError::Io(e) | ClientError::Frame(FrameError::Io(e)) => matches!(
            e.kind(),
            io::ErrorKind::UnexpectedEof
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
        ),
        _ => false,
    };
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
        e if closed => CliError::new(Kind::DaemonUnavailable, format!("daemon {CLOSED}: {e}")),
        e => CliError::new(Kind::DaemonUnavailable, e.to_string()),
    }
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
        assert_eq!(
            connect_error(io::Error::from(io::ErrorKind::ConnectionRefused)).kind,
            Kind::DaemonUnavailable
        );
    }
}
