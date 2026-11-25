//! IPC clients for the #25 daemon. Transport-neutral over a `Connector`; the daemon crate
//! supplies the verified local socket connector (and lazy start) for its executables.
//!
//! `IpcLibraryClient` implements the existing `LibraryClient` so views are unchanged:
//! `submit` only enqueues; connecting, (re)starting the daemon and writing happen on the
//! `ipc-client` thread, and completions run on the `ipc-reader` thread.
mod library;
#[cfg(unix)]
pub mod local;
/// #26 OS secret stores for token plaintext (std-only, shared by daemon/CLI/MCP).
#[cfg(unix)]
pub mod secrets;
#[cfg(unix)]
mod sha256;
#[cfg(all(test, unix))]
mod tests;
pub use library::{ClientTimeouts, IpcLibraryClient};

use crate::ipc::{
    ClientFrame, Command, FrameError, HandshakeRejection, Hello, MAX_HANDSHAKE_FRAME,
    MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, Reply, ServerFrame, Welcome, WireError, read_frame,
    write_frame,
};
use std::{
    io::{self, Read, Write},
    time::Duration,
};

/// Default limit for the whole Hello/Welcome exchange on a fresh connection.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A connected, bidirectional byte stream that can be split into reader/writer halves.
pub trait Stream: Read + Write + Send + 'static {
    fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>>;
    /// Unblocks a concurrent reader on the other half.
    fn shutdown_stream(&self);
    /// Read/write timeouts of the underlying socket (`None` = block). Socket options are
    /// shared by every clone of the stream. Streams without timeouts may ignore this.
    fn set_io_timeouts(&self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        let _ = (read, write);
        Ok(())
    }
}
#[cfg(unix)]
impl Stream for std::os::unix::net::UnixStream {
    fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn shutdown_stream(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
    fn set_io_timeouts(&self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(read.map(nonzero))?;
        self.set_write_timeout(write.map(nonzero))
    }
}
fn nonzero(timeout: Duration) -> Duration {
    timeout.max(Duration::from_millis(1))
}

/// Produces an authenticated-ready stream plus the `Hello` to present. May block (e.g.
/// start the daemon and wait); it is never called on a UI thread by `IpcLibraryClient`.
pub trait Connector: Send + Sync + 'static {
    fn connect(&self) -> io::Result<(Box<dyn Stream>, Hello)>;
}

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Frame(FrameError),
    Rejected(HandshakeRejection),
    Protocol(&'static str),
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "daemon connection: {e}"),
            Self::Frame(e) => e.fmt(f),
            Self::Rejected(r) => write!(f, "daemon rejected the connection: {r:?}"),
            Self::Protocol(e) => write!(f, "daemon protocol error: {e}"),
        }
    }
}
impl std::error::Error for ClientError {}
impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<FrameError> for ClientError {
    fn from(e: FrameError) -> Self {
        Self::Frame(e)
    }
}

impl ClientError {
    /// The daemon went away right after accepting (e.g. it was in the middle of its idle
    /// exit): worth exactly one fresh connect, unlike a rejection, a malformed reply or a
    /// timeout. Only consulted for failures *after* the socket connected, so any transport
    /// error on that fresh connection counts: platforms disagree on how a just-closed Unix
    /// socket fails (EOF, EPIPE, ECONNRESET, and on macOS also ENOTCONN, EINVAL from
    /// setsockopt or EPROTOTYPE from a racing send).
    fn closed_during_handshake(&self) -> bool {
        let io = |e: &io::Error| {
            !matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            )
        };
        match self {
            Self::Protocol(reason) => *reason == CLOSED_DURING_HANDSHAKE,
            Self::Io(e) | Self::Frame(FrameError::Io(e)) => io(e),
            _ => false,
        }
    }
}
const CLOSED_DURING_HANDSHAKE: &str = "daemon closed during handshake";

/// Performs the Hello/Welcome exchange on a fresh stream within `HANDSHAKE_TIMEOUT`.
/// The stream's timeouts are cleared again afterwards.
pub fn handshake(stream: &mut dyn Stream, hello: Hello) -> Result<Welcome, ClientError> {
    handshake_within(stream, hello, HANDSHAKE_TIMEOUT)
}

/// `handshake` with an explicit limit for each read/write of the exchange; a daemon that
/// accepts but never answers fails with a timeout error instead of blocking forever.
pub fn handshake_within(
    stream: &mut dyn Stream,
    hello: Hello,
    timeout: Duration,
) -> Result<Welcome, ClientError> {
    // On macOS this fails with EINVAL when the daemon already closed the connection; the
    // resulting I/O error is retried once by `connect_and_handshake`.
    stream.set_io_timeouts(Some(timeout), Some(timeout))?;
    let result = (|| {
        write_frame(stream, &ClientFrame::Hello(hello), MAX_HANDSHAKE_FRAME)?;
        match read_frame::<_, ServerFrame>(stream, MAX_RESPONSE_FRAME)? {
            Some(ServerFrame::Welcome(welcome)) => Ok(welcome),
            Some(ServerFrame::Rejected(reason)) => Err(ClientError::Rejected(reason)),
            Some(_) => Err(ClientError::Protocol("expected welcome")),
            None => Err(ClientError::Protocol(CLOSED_DURING_HANDSHAKE)),
        }
    })();
    let cleared = stream.set_io_timeouts(None, None);
    let welcome = result?;
    cleared?;
    Ok(welcome)
}

/// Connects and handshakes, retrying exactly once when the daemon closes the fresh
/// connection during the handshake (the idle-exit race: it accepted just before exiting).
/// The connector then finds no socket and starts a new daemon if it may.
pub fn connect_and_handshake(
    connector: &dyn Connector,
    timeout: Duration,
) -> Result<(Box<dyn Stream>, Welcome), ClientError> {
    // `Ok(Err(_))` is a failure after the connection was established (retryable once when it
    // looks like the daemon went away); `Err(_)` is a connect failure (never retried here:
    // the connector already decides whether to start a daemon).
    type Connected = Result<(Box<dyn Stream>, Welcome), ClientError>;
    let attempt = || -> Result<Connected, ClientError> {
        let (mut stream, hello) = connector.connect()?;
        Ok(handshake_within(stream.as_mut(), hello, timeout).map(|welcome| (stream, welcome)))
    };
    match attempt()? {
        Err(e) if e.closed_during_handshake() => attempt()?,
        other => other,
    }
}

/// Blocking request/response session for CLI-style callers and tests. One request at a time;
/// PDF progress frames are collected into `progress`.
pub struct Session {
    stream: Box<dyn Stream>,
    next: u64,
    pub welcome: Welcome,
    pub progress: Vec<crate::attachments::PdfReport>,
}
impl Session {
    pub fn open(connector: &dyn Connector) -> Result<Self, ClientError> {
        let (stream, welcome) = connect_and_handshake(connector, HANDSHAKE_TIMEOUT)?;
        Ok(Self {
            stream,
            next: 1,
            welcome,
            progress: Vec::new(),
        })
    }
    pub fn from_stream(mut stream: Box<dyn Stream>, hello: Hello) -> Result<Self, ClientError> {
        let welcome = handshake(stream.as_mut(), hello)?;
        Ok(Self {
            stream,
            next: 1,
            welcome,
            progress: Vec::new(),
        })
    }
    pub fn call(&mut self, command: Command) -> Result<Result<Reply, WireError>, ClientError> {
        let id = self.next;
        self.next += 1;
        write_frame(
            self.stream.as_mut(),
            &ClientFrame::Request { id, command },
            MAX_REQUEST_FRAME,
        )?;
        self.wait(id)
    }
    /// Sends a cancel for an earlier id without waiting (pair with `wait`).
    pub fn send(&mut self, frame: &ClientFrame) -> Result<(), ClientError> {
        write_frame(self.stream.as_mut(), frame, MAX_REQUEST_FRAME)?;
        Ok(())
    }
    pub fn next_id(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }
    pub fn wait(&mut self, id: u64) -> Result<Result<Reply, WireError>, ClientError> {
        loop {
            match read_frame::<_, ServerFrame>(self.stream.as_mut(), MAX_RESPONSE_FRAME)? {
                Some(ServerFrame::Response { id: got, result }) if got == id => return Ok(result),
                Some(ServerFrame::PdfProgress { report, .. }) => self.progress.push(report),
                Some(ServerFrame::Response { .. }) => {}
                Some(_) => return Err(ClientError::Protocol("unexpected frame")),
                None => return Err(ClientError::Protocol("daemon closed the connection")),
            }
        }
    }
}
