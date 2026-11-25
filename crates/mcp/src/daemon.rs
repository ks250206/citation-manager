//! Multiplexed, reconnecting papersd IPC client for the MCP server (#25 wire protocol).
//!
//! - One authenticated connection, shared by concurrent tool calls; a reader thread routes
//!   responses to waiting calls by request id.
//! - Lazy start: the `LocalConnector` starts papersd (sibling executable) when nothing answers.
//! - Restart: a lost daemon fails the calls waiting on it; the next call reconnects (and
//!   restarts the daemon). A call whose request could not be written, and read-only calls
//!   whose connection dropped before the reply, are retried once on a fresh connection.
//!   Writes are never re-sent automatically.
//! - Cancellation: an MCP `notifications/cancelled` cancels the call's token; the client sends
//!   `ClientFrame::Cancel{id}` and drops the eventual reply.
//! - Credential changes (a replaced credential file) reconnect with the new secret.
//! - The connection is closed after an idle period so papersd can idle-exit as usual.
use crate::credential::{CLIENT_KIND, CredentialStore};
use cm_protocol::{
    annotations::AnnotationRequest,
    client::{
        ClientError, HANDSHAKE_TIMEOUT, Stream, connect_and_handshake,
        local::{CredentialSource, DaemonPaths, Launch, LocalConnector},
    },
    ipc::{
        CapabilitySet, ClientFrame, Command, Credential, HandshakeRejection, MAX_REQUEST_FRAME,
        MAX_RESPONSE_FRAME, PdfCommand, Reply, ServerFrame, Welcome, WireError, read_frame,
        write_frame,
    },
    library::LibraryRequest,
    organization::OrganizationRequest,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Why a call produced no daemon reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The daemon answered with an error (authorization, validation, busy, ...).
    Wire(WireError),
    /// The connection dropped before the reply (daemon stopped or restarted).
    Lost,
    /// The daemon could not be reached or started.
    Connect(String),
    /// The daemon refused the handshake (e.g. revoked/unknown credential).
    Rejected(HandshakeRejection),
    /// No usable credential.
    Credential(String),
    Timeout(Duration),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub paths: DaemonPaths,
    pub uid: u32,
    pub launch: Option<Launch>,
    /// Narrowing set requested in `Hello` (never widens the grant).
    pub requested: CapabilitySet,
    /// Close an unused connection after this long (`None`: keep it open).
    pub idle_disconnect: Option<Duration>,
}

type Pending = oneshot::Sender<Result<Reply, WireError>>;

struct Connection {
    writer: Mutex<Box<dyn Stream>>,
    pending: Mutex<HashMap<u64, Pending>>,
    next: AtomicU64,
    alive: AtomicBool,
    credential: Credential,
    welcome: Welcome,
    last_used: Mutex<Instant>,
}
impl Connection {
    fn start(
        stream: Box<dyn Stream>,
        welcome: Welcome,
        credential: Credential,
    ) -> Result<Arc<Self>, String> {
        stream
            .set_io_timeouts(None, Some(Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        let mut reader = stream.try_clone_stream().map_err(|e| e.to_string())?;
        let connection = Arc::new(Self {
            writer: Mutex::new(stream),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            credential,
            welcome,
            last_used: Mutex::new(Instant::now()),
        });
        let weak = Arc::downgrade(&connection);
        std::thread::Builder::new()
            .name("mcp-ipc-reader".into())
            .spawn(move || {
                loop {
                    let frame = read_frame::<_, ServerFrame>(reader.as_mut(), MAX_RESPONSE_FRAME);
                    let Some(connection) = weak.upgrade() else {
                        return;
                    };
                    match frame {
                        Ok(Some(ServerFrame::Response { id, result })) => {
                            let waiter = connection.pending().remove(&id);
                            if let Some(waiter) = waiter {
                                let _ = waiter.send(result);
                            }
                        }
                        Ok(Some(ServerFrame::PdfProgress { .. })) => {}
                        Ok(Some(_)) | Ok(None) | Err(_) => {
                            connection.kill();
                            crate::log::debug("daemon connection closed");
                            return;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(connection)
    }
    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    /// Marks the connection dead, unblocks the reader and fails every waiting call (`Lost`).
    fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown_stream();
        self.pending().clear();
    }
    fn write(&self, frame: &ClientFrame) -> std::io::Result<()> {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        write_frame(writer.as_mut(), frame, MAX_REQUEST_FRAME).map_err(|e| match e {
            cm_protocol::ipc::FrameError::Io(e) => e,
            other => std::io::Error::other(other.to_string()),
        })
    }
    fn touch(&self) {
        *self.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }
    fn idle_for(&self) -> Duration {
        self.last_used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed()
    }
}

pub struct DaemonClient {
    config: ClientConfig,
    credential: Arc<dyn CredentialStore>,
    state: tokio::sync::Mutex<Option<Arc<Connection>>>,
}

impl DaemonClient {
    pub fn new(config: ClientConfig, credential: Arc<dyn CredentialStore>) -> Arc<Self> {
        let client = Arc::new(Self {
            config,
            credential,
            state: tokio::sync::Mutex::new(None),
        });
        if let Some(idle) = client.config.idle_disconnect {
            tokio::spawn(reap_idle(Arc::downgrade(&client), idle));
        }
        client
    }

    pub fn library(&self) -> &std::path::Path {
        &self.config.paths.library
    }

    /// Capabilities of the current connection at handshake time (diagnostics only; the
    /// daemon re-checks the live grant on every request).
    pub async fn capabilities(&self) -> Option<CapabilitySet> {
        let state = self.state.lock().await;
        state
            .as_ref()
            .filter(|c| c.alive())
            .map(|c| c.welcome.capabilities.clone())
    }

    async fn connection(&self) -> Result<Arc<Connection>, CallError> {
        let credential = self.credential.load().map_err(CallError::Credential)?;
        let mut state = self.state.lock().await;
        if let Some(existing) = state.as_ref() {
            if existing.alive() && existing.credential == credential {
                return Ok(existing.clone());
            }
            if existing.alive() {
                crate::log::info("credential changed; reconnecting with the new grant");
            }
            existing.kill();
        }
        *state = None;
        let mut connector =
            LocalConnector::new(self.config.paths.clone(), self.config.uid, CLIENT_KIND);
        connector.launch = self.config.launch.clone();
        connector.requested = Some(self.config.requested.clone());
        connector.credential = CredentialSource::Explicit(credential.clone());
        let connected = tokio::task::spawn_blocking(move || {
            connect_and_handshake(&connector, HANDSHAKE_TIMEOUT)
        })
        .await
        .map_err(|e| CallError::Connect(e.to_string()))?;
        let (stream, welcome) = connected.map_err(|e| match e {
            ClientError::Rejected(r) => CallError::Rejected(r),
            other => CallError::Connect(other.to_string()),
        })?;
        crate::log::debug(&format!(
            "connected to papersd (session {}, grant {})",
            welcome.session, welcome.grant
        ));
        let connection =
            Connection::start(stream, welcome, credential).map_err(CallError::Connect)?;
        *state = Some(connection.clone());
        Ok(connection)
    }

    /// Sends `command` and waits for its reply, the cancellation token, or the deadline.
    pub async fn call(
        &self,
        command: Command,
        cancel: &CancellationToken,
    ) -> Result<Reply, CallError> {
        let timeout = timeout_for(&command);
        let retry_lost = is_read_only(&command);
        let mut attempt = 0;
        loop {
            attempt += 1;
            if cancel.is_cancelled() {
                return Err(CallError::Cancelled);
            }
            let connection = tokio::select! {
                c = self.connection() => c?,
                _ = cancel.cancelled() => return Err(CallError::Cancelled),
            };
            connection.touch();
            let id = connection.next.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = oneshot::channel();
            connection.pending().insert(id, tx);
            let frame = ClientFrame::Request {
                id,
                command: command.clone(),
            };
            let writer = connection.clone();
            let written = tokio::task::spawn_blocking(move || writer.write(&frame))
                .await
                .map_err(|e| CallError::Connect(e.to_string()))?;
            if let Err(e) = written {
                connection.pending().remove(&id);
                connection.kill();
                crate::log::debug(&format!("request write failed: {e}"));
                if attempt == 1 {
                    continue;
                }
                return Err(CallError::Lost);
            }
            let outcome = tokio::select! {
                reply = rx => match reply {
                    Ok(Ok(reply)) => Ok(reply),
                    Ok(Err(e)) => Err(CallError::Wire(e)),
                    Err(_) => Err(CallError::Lost),
                },
                _ = cancel.cancelled() => {
                    self.abandon(&connection, id);
                    return Err(CallError::Cancelled);
                }
                _ = tokio::time::sleep(timeout) => {
                    self.abandon(&connection, id);
                    return Err(CallError::Timeout(timeout));
                }
            };
            connection.touch();
            match outcome {
                Err(CallError::Lost) if retry_lost && attempt == 1 => {
                    crate::log::info("daemon connection lost; retrying the read once");
                    continue;
                }
                other => return other,
            }
        }
    }

    /// Stops waiting for `id` and asks the daemon to cancel it (best effort).
    fn abandon(&self, connection: &Arc<Connection>, id: u64) {
        connection.pending().remove(&id);
        let connection = connection.clone();
        tokio::task::spawn_blocking(move || {
            if connection.write(&ClientFrame::Cancel { id }).is_err() {
                connection.kill();
            }
        });
    }
}

async fn reap_idle(client: Weak<DaemonClient>, idle: Duration) {
    let tick = (idle / 4).clamp(Duration::from_millis(50), Duration::from_secs(5));
    loop {
        tokio::time::sleep(tick).await;
        let Some(client) = client.upgrade() else {
            return;
        };
        let Ok(mut state) = client.state.try_lock() else {
            continue;
        };
        let idle_connection = state
            .as_ref()
            .is_some_and(|c| !c.alive() || (c.pending().is_empty() && c.idle_for() >= idle));
        if idle_connection
            && let Some(connection) = state.take()
            && connection.alive()
        {
            crate::log::debug("closing idle daemon connection");
            connection.kill();
        }
    }
}

/// Generous client-side limits; the daemon serializes library work on one worker.
fn timeout_for(command: &Command) -> Duration {
    match command {
        Command::Status | Command::Search(_) | Command::Fulltext(_) | Command::Grants(_) => {
            Duration::from_secs(30)
        }
        Command::Export(_) | Command::Import(_) | Command::Pdf(_) => Duration::from_secs(10 * 60),
        _ => Duration::from_secs(3 * 60),
    }
}

/// Commands without effects, which may be re-sent once after a lost connection.
fn is_read_only(command: &Command) -> bool {
    match command {
        Command::Status | Command::Search(_) | Command::Fulltext(_) | Command::Query(_) => true,
        Command::Library(r) => matches!(r, LibraryRequest::Get(_) | LibraryRequest::List(_)),
        Command::Organization(r) => {
            matches!(
                r,
                OrganizationRequest::List | OrganizationRequest::ForPaper(_)
            )
        }
        Command::Annotations(r) => {
            matches!(
                r,
                AnnotationRequest::List(_) | AnnotationRequest::ForPaper(_)
            )
        }
        Command::Pdf(PdfCommand::List(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_protocol::{
        domain::PaperId,
        exchange::{ExportRequest, Format},
        library::NewPaper,
        search::SearchRequest,
    };

    #[test]
    fn only_effect_free_commands_are_retried_after_a_lost_connection() {
        let paper = PaperId(uuid::Uuid::new_v4());
        assert!(is_read_only(&Command::Search(Box::new(
            SearchRequest::new("x")
        ))));
        assert!(is_read_only(&Command::Library(LibraryRequest::Get(paper))));
        assert!(is_read_only(&Command::Organization(
            OrganizationRequest::ForPaper(paper)
        )));
        assert!(!is_read_only(&Command::Library(LibraryRequest::Create(
            Box::new(NewPaper::titled("x"))
        ))));
        assert!(!is_read_only(&Command::Resolve {
            doi: "10.1/x".into()
        }));
        assert!(!is_read_only(&Command::Export(ExportRequest {
            selection: cm_protocol::exchange::ExportSelection::All,
            format: Format::Bibtex,
            path: "/tmp/x".into(),
        })));
    }
}
