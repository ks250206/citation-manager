//! Daemon access for the host: the user's library `papersd`, started on demand from the
//! executable's own directory exactly like the GUI (absolute sibling path, never PATH), and
//! authenticated with the daemon-issued capture grant only.
use crate::message::{ErrorCode, HostError, Request};
use cm_protocol::{
    capture::{CaptureCommand, CaptureReply},
    client::{
        ClientError, Connector, Session, Stream,
        local::{CredentialSource, DaemonPaths, Launch, LocalConnector},
    },
    ipc::{
        Capability, CapabilitySet, Command, Credential, Denial, HandshakeRejection, Hello, Reply,
        WireError,
    },
    library::ServiceError,
    organization::OrganizationRequest,
};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

pub const CLIENT_KIND: &str = "native-host";
/// How long a daemon start may take before the request fails with `daemon_unavailable`.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for one daemon reply (a preview may complement metadata over the network).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// The `native_host` preset (#22). Requested in `Hello` so even a mistakenly broader stored
/// grant is narrowed to it for this session; the daemon grant remains the authority.
pub fn capture_capabilities() -> CapabilitySet {
    [
        Capability::CapturePreview,
        Capability::CaptureCommit,
        Capability::DestinationRead,
    ]
    .into_iter()
    .collect()
}

/// `papersd` next to `exe` (`<prefix>/bin`, `.app/Contents/MacOS`).
pub fn sibling_papersd(exe: &Path) -> PathBuf {
    exe.with_file_name("papersd")
}

pub fn launch(exe: &Path) -> Launch {
    Launch {
        papersd: sibling_papersd(exe),
        extra_args: Vec::new(),
        timeout: START_TIMEOUT,
    }
}

/// Socket whose post-handshake reads/writes stay bounded: `Session` clears handshake
/// timeouts; this stream maps "no timeout" to the host's request limit instead.
struct BoundedStream {
    inner: UnixStream,
    limit: Duration,
}
impl Read for BoundedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}
impl Write for BoundedStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
impl Stream for BoundedStream {
    fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(Self {
            inner: self.inner.try_clone()?,
            limit: self.limit,
        }))
    }
    fn shutdown_stream(&self) {
        let _ = self.inner.shutdown(std::net::Shutdown::Both);
    }
    fn set_io_timeouts(&self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        let bound =
            |t: Option<Duration>| Some(t.unwrap_or(self.limit).max(Duration::from_millis(1)));
        self.inner.set_read_timeout(bound(read))?;
        self.inner.set_write_timeout(bound(write))
    }
}

/// `LocalConnector` (lazy start, endpoint checks) with bounded request I/O.
pub struct HostConnector {
    pub local: LocalConnector,
    pub limit: Duration,
}
impl HostConnector {
    pub fn new(paths: DaemonPaths, uid: u32, credential: CredentialSource, exe: &Path) -> Self {
        let mut local = LocalConnector::new(paths, uid, CLIENT_KIND);
        local.launch = Some(launch(exe));
        local.credential = credential;
        Self {
            local,
            limit: REQUEST_TIMEOUT,
        }
    }
    /// The browser-facing connector: capture grant, narrowed to the native host preset.
    pub fn capture(paths: DaemonPaths, uid: u32, credential: Credential, exe: &Path) -> Self {
        let mut connector = Self::new(paths, uid, CredentialSource::Explicit(credential), exe);
        connector.local.requested = Some(capture_capabilities());
        connector
    }
}
impl Connector for HostConnector {
    fn connect(&self) -> io::Result<(Box<dyn Stream>, Hello)> {
        let inner = self.local.connect_or_start()?;
        let stream = BoundedStream {
            inner,
            limit: self.limit,
        };
        stream.set_io_timeouts(None, None)?;
        Ok((Box::new(stream), self.local.hello()?))
    }
}

/// The only commands the browser can cause. Hello never reaches this function.
pub fn command(request: Request) -> Option<Command> {
    Some(match request {
        Request::Hello => return None,
        Request::Destinations => Command::Organization(OrganizationRequest::List),
        Request::Preview(capture) => Command::Capture(CaptureCommand::Preview(capture)),
        Request::Commit(commit) => Command::Capture(CaptureCommand::Commit(commit)),
        Request::Cancel(preview) => Command::Capture(CaptureCommand::Cancel(preview)),
        Request::PdfJob(job) => Command::Capture(CaptureCommand::PdfJob(job)),
        Request::RetryPdf(job) => Command::Capture(CaptureCommand::RetryPdf(job)),
    })
}

/// One request on a fresh session (so the daemon's idle clock starts as soon as we are done).
pub fn call(connector: &dyn Connector, command: Command) -> Result<Value, HostError> {
    let mut session = Session::open(connector).map_err(client_error)?;
    let reply = session.call(command).map_err(client_error)?;
    drop(session);
    reply_value(reply.map_err(wire_error)?)
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, HostError> {
    serde_json::to_value(value).map_err(|_| HostError::new(ErrorCode::Internal, "reply encoding"))
}

pub fn reply_value(reply: Reply) -> Result<Value, HostError> {
    match reply {
        Reply::Capture(CaptureReply::Preview(preview)) => to_value(&preview),
        Reply::Capture(CaptureReply::Committed(committed)) => to_value(&committed),
        Reply::Capture(CaptureReply::Cancelled) => Ok(json!({ "cancelled": true })),
        Reply::Capture(CaptureReply::PdfJob(job)) => to_value(&job),
        Reply::Organization(organization) => to_value(&organization),
        _ => Err(HostError::new(
            ErrorCode::Internal,
            "unexpected daemon reply",
        )),
    }
}

pub fn wire_error(error: WireError) -> HostError {
    match error {
        WireError::Service(ServiceError::Unavailable) | WireError::Unavailable => {
            HostError::new(ErrorCode::DaemonUnavailable, "library service unavailable")
        }
        WireError::Service(ServiceError::Conflict) => HostError::new(
            ErrorCode::Conflict,
            "the library changed or the preview expired; preview again",
        ),
        WireError::Service(ServiceError::NotFound) => {
            HostError::new(ErrorCode::NotFound, "not found")
        }
        WireError::Service(ServiceError::InvalidInput(message)) => {
            HostError::new(ErrorCode::InvalidInput, message)
        }
        WireError::Denied(Denial::Forbidden(operation)) => HostError::new(
            ErrorCode::Denied,
            "not permitted for the browser capture grant",
        )
        .with_detail(json!({ "operation": operation })),
        WireError::Denied(denial) => HostError::new(
            ErrorCode::Unauthorized,
            format!("browser capture grant {denial}; pair again"),
        ),
        WireError::Busy => HostError::new(ErrorCode::Busy, "library service busy"),
        WireError::TooLarge => HostError::new(ErrorCode::TooLarge, "request too large"),
        WireError::Cancelled => HostError::new(ErrorCode::Cancelled, "cancelled"),
    }
}

pub fn client_error(error: ClientError) -> HostError {
    match error {
        ClientError::Rejected(HandshakeRejection::Unauthenticated) => HostError::new(
            ErrorCode::Unauthorized,
            "the browser capture grant is unknown, revoked or expired; pair again",
        ),
        ClientError::Rejected(HandshakeRejection::VersionMismatch { supported }) => HostError::new(
            ErrorCode::DaemonRejected,
            "daemon protocol version mismatch",
        )
        .with_detail(json!({ "daemon_supported": supported })),
        ClientError::Rejected(other) => HostError::new(
            ErrorCode::DaemonRejected,
            format!("daemon rejected the host: {other:?}"),
        ),
        ClientError::Frame(cm_protocol::ipc::FrameError::TooLarge(_)) => {
            HostError::new(ErrorCode::TooLarge, "request over the daemon frame limit")
        }
        other => HostError::new(ErrorCode::DaemonUnavailable, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::parse;
    use cm_protocol::ipc::Operation;

    #[test]
    fn requests_map_to_capture_and_destination_operations_only() {
        let job = "0192f0b2-0000-7000-8000-000000000001";
        let allowed = capture_capabilities();
        for body in [
            json!({"v":1,"type":"destinations"}),
            json!({"v":1,"type":"capture.preview","capture":{"page_url":"https://e.org"}}),
            json!({"v":1,"type":"capture.commit","preview_id":"p",
                   "decisions":[{"candidate_index":0,"action":"Create"}]}),
            json!({"v":1,"type":"capture.cancel","preview_id":"p"}),
            json!({"v":1,"type":"capture.pdf_job","job":job}),
            json!({"v":1,"type":"capture.retry_pdf","job":job}),
        ] {
            let request = parse(&serde_json::to_vec(&body).unwrap()).unwrap().request;
            let operation = command(request).unwrap().operation();
            assert!(
                matches!(
                    operation,
                    Operation::OrganizationList
                        | Operation::CaptureSubmit
                        | Operation::CaptureSave
                        | Operation::CaptureCancel
                        | Operation::CapturePdfStatus
                        | Operation::CapturePdfRetry
                ),
                "{operation:?}"
            );
        }
        assert!(command(Request::Hello).is_none());
        assert_eq!(allowed.len(), 3);
    }

    #[test]
    fn daemon_errors_map_to_stable_codes() {
        assert_eq!(wire_error(WireError::Busy).code, ErrorCode::Busy);
        assert_eq!(
            wire_error(WireError::Denied(Denial::Revoked)).code,
            ErrorCode::Unauthorized
        );
        let denied = wire_error(WireError::Denied(Denial::Forbidden(
            Operation::ImportCommit,
        )));
        assert_eq!(denied.code, ErrorCode::Denied);
        assert_eq!(denied.detail, Some(json!({"operation": "import_commit"})));
        assert_eq!(
            wire_error(WireError::Service(ServiceError::Conflict)).code,
            ErrorCode::Conflict
        );
        assert!(wire_error(WireError::Unavailable).retryable);
        assert_eq!(
            client_error(ClientError::Rejected(HandshakeRejection::Unauthenticated)).code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            client_error(ClientError::Io(io::ErrorKind::NotFound.into())).code,
            ErrorCode::DaemonUnavailable
        );
        assert_eq!(
            sibling_papersd(Path::new("/A.app/Contents/MacOS/papers-native-host")),
            PathBuf::from("/A.app/Contents/MacOS/papersd")
        );
    }
}
