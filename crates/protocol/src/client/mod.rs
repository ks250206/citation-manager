//! IPC clients for the #25 daemon. Transport-neutral over a `Connector`; the daemon crate
//! supplies the verified local socket connector (and lazy start) for its executables.
//!
//! `IpcLibraryClient` implements the existing `LibraryClient` so views are unchanged:
//! `submit` only enqueues; connecting, (re)starting the daemon and writing happen on the
//! `ipc-client` thread, and completions run on the `ipc-reader` thread.
mod library;
#[cfg(unix)]
pub mod local;
pub use library::IpcLibraryClient;

use crate::ipc::{
    ClientFrame, Command, FrameError, HandshakeRejection, Hello, MAX_HANDSHAKE_FRAME,
    MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, Reply, ServerFrame, Welcome, WireError, read_frame,
    write_frame,
};
use std::io::{self, Read, Write};

/// A connected, bidirectional byte stream that can be split into reader/writer halves.
pub trait Stream: Read + Write + Send + 'static {
    fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>>;
    /// Unblocks a concurrent reader on the other half.
    fn shutdown_stream(&self);
}
#[cfg(unix)]
impl Stream for std::os::unix::net::UnixStream {
    fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn shutdown_stream(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
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

/// Performs the Hello/Welcome exchange on a fresh stream.
pub fn handshake(stream: &mut dyn Stream, hello: Hello) -> Result<Welcome, ClientError> {
    write_frame(stream, &ClientFrame::Hello(hello), MAX_HANDSHAKE_FRAME)?;
    match read_frame::<_, ServerFrame>(stream, MAX_RESPONSE_FRAME)? {
        Some(ServerFrame::Welcome(welcome)) => Ok(welcome),
        Some(ServerFrame::Rejected(reason)) => Err(ClientError::Rejected(reason)),
        Some(_) => Err(ClientError::Protocol("expected welcome")),
        None => Err(ClientError::Protocol("daemon closed during handshake")),
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
        let (stream, hello) = connector.connect()?;
        Self::from_stream(stream, hello)
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
