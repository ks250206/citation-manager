//! `LibraryClient` over IPC. Exactly-once completion: whoever removes a pending entry
//! (reader on response/EOF, the transport thread on a failed write or an expired deadline)
//! completes it.
//!
//! No request can hang the caller: the handshake has read/write timeouts, request writes a
//! write timeout, and every pending request a deadline (`ClientTimeouts`) after which it
//! completes with `Unavailable`. Failed (re)connects back off instead of re-running a
//! possibly slow daemon start for every queued request.
use super::{ClientError, Connector, Stream, connect_and_handshake};
use crate::{
    attachments::{PdfProgress, PdfReply, PdfRequest},
    exchange::{
        Cancellation, ExportRequest, Exported, ImportReply, ImportRequestUi, MetadataResolution,
    },
    ipc::{
        ClientFrame, Command, MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, PdfCommand, Reply,
        ServerFrame, WireError, read_frame, write_frame,
    },
    library::{Completion, LibraryClient, LibraryRequest, ServiceError},
    library_query::{LibraryPage, LibraryQuery},
    organization::{Organization, OrganizationRequest},
    settings::{Settings, SettingsLoaded},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// Client-side time limits. Every request completes exactly once: with the daemon's answer,
/// or `Unavailable` when its deadline passes, the connection is lost, or (re)connecting fails.
///
/// Deadlines are per command class because the daemon runs library work on one serial
/// worker: a quick query can legitimately wait behind a long import, so only the control
/// commands get a short limit. When a request expires the client sends a best-effort
/// `Cancel` for it; the connection itself is dropped (and re-established by the next
/// request) only when no other request is still waiting on it, so one slow request never
/// aborts another long operation that is still within its own deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientTimeouts {
    /// Each read/write of the Hello/Welcome exchange. Default 10 s.
    pub handshake: Duration,
    /// Socket write limit for request and cancel frames. Default 30 s.
    pub write: Duration,
    /// `Status`, `Jobs`, `Grants`, `Search`, `Fulltext`: answered on the daemon's connection
    /// thread. Default 30 s.
    pub control: Duration,
    /// Library CRUD, queries, organization, lifecycle, settings, DOI resolve: they queue
    /// behind the daemon's single library worker. Default 5 min.
    pub interactive: Duration,
    /// Imports, exports and PDF commands (inspection, rendering, attachment, commit).
    /// Default 60 min. For PDF commands every progress frame restarts it, so it bounds
    /// silence rather than total work.
    pub long: Duration,
    /// Wait before reconnecting after the second consecutive failed connect, doubling up to
    /// `backoff_max`; requests during the wait fail fast with `Unavailable`. A single failure
    /// is retried by the next request at once (a daemon restart is not delayed). 250 ms.
    pub backoff_base: Duration,
    /// Default 10 s.
    pub backoff_max: Duration,
}
impl Default for ClientTimeouts {
    fn default() -> Self {
        Self {
            handshake: super::HANDSHAKE_TIMEOUT,
            write: Duration::from_secs(30),
            control: Duration::from_secs(30),
            interactive: Duration::from_secs(5 * 60),
            long: Duration::from_secs(60 * 60),
            backoff_base: Duration::from_millis(250),
            backoff_max: Duration::from_secs(10),
        }
    }
}
impl ClientTimeouts {
    /// Deadline of one request of this command, measured from when it is sent.
    pub fn for_command(&self, command: &Command) -> Duration {
        match command {
            Command::Status | Command::Jobs(_) | Command::Grants(_) => self.control,
            // #18: bounded by the request's scan budget (<= 10 s) and FTS candidate caps.
            Command::Search(_) | Command::Fulltext(_) => self.control,
            Command::Annotations(_) => self.interactive,
            Command::Library(_)
            | Command::Query(_)
            | Command::Organization(_)
            | Command::Lifecycle { .. }
            | Command::LoadSettings
            | Command::SaveSettings(_)
            | Command::Resolve { .. } => self.interactive,
            Command::Import(_) | Command::Export(_) | Command::Pdf(_) => self.long,
        }
    }
    pub(super) fn backoff(&self, failures: u32) -> Option<Duration> {
        let doublings = failures.checked_sub(2)?.min(16);
        Some(
            self.backoff_base
                .saturating_mul(1u32 << doublings)
                .min(self.backoff_max),
        )
    }
}

type RawCompletion = Box<dyn FnOnce(Result<Reply, WireError>) + Send>;
struct Pending {
    complete: RawCompletion,
    progress: Option<PdfProgress>,
    generation: u64,
    deadline: Instant,
    timeout: Duration,
}
enum Outgoing {
    Request {
        id: u64,
        command: Box<Command>,
        complete: RawCompletion,
        progress: Option<PdfProgress>,
        cancel: Option<Cancellation>,
    },
    Connect,
}
#[derive(Default)]
struct Shared {
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    last_error: Mutex<Option<String>>,
}
impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn take(&self, id: u64) -> Option<Pending> {
        self.lock().remove(&id)
    }
    fn fail_generation(&self, generation: u64) {
        let failed: Vec<Pending> = {
            let mut pending = self.lock();
            let ids: Vec<u64> = pending
                .iter()
                .filter(|(_, p)| p.generation == generation)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id))
                .collect()
        };
        for p in failed {
            (p.complete)(Err(WireError::Unavailable));
        }
    }
    fn take_expired(&self, now: Instant) -> Vec<(u64, Pending)> {
        let mut pending = self.lock();
        let ids: Vec<u64> = pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| pending.remove(&id).map(|p| (id, p)))
            .collect()
    }
    fn has_generation(&self, generation: u64) -> bool {
        self.lock().values().any(|p| p.generation == generation)
    }
    fn set_error(&self, error: Option<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = error;
    }
    fn connect_error(&self) -> WireError {
        let reason = self
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        if reason.contains("rejected") {
            WireError::Service(ServiceError::InvalidInput(reason))
        } else {
            WireError::Unavailable
        }
    }
}

/// Persistent connection to the daemon. Holding it open keeps the daemon alive (GUI
/// lifetime); a lost daemon is reconnected (and, via the connector, restarted) on the
/// next request. In-flight requests at loss complete with `Unavailable`.
#[derive(Clone)]
pub struct IpcLibraryClient {
    sender: mpsc::SyncSender<Outgoing>,
    shared: Arc<Shared>,
}
impl IpcLibraryClient {
    /// Uses `ClientTimeouts::default()`.
    pub fn new(connector: impl Connector) -> std::io::Result<Self> {
        Self::with_timeouts(connector, ClientTimeouts::default())
    }
    pub fn with_timeouts(
        connector: impl Connector,
        timeouts: ClientTimeouts,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Outgoing>(64);
        let shared = Arc::new(Shared::default());
        let transport = Transport {
            connector,
            shared: shared.clone(),
            timeouts,
            connection: None,
            generation: 0,
            failures: 0,
            retry_at: None,
            cancels: Vec::new(),
        };
        thread::Builder::new()
            .name("ipc-client".into())
            .spawn(move || transport.run(receiver))?;
        Ok(Self { sender, shared })
    }
    /// Asks the transport thread to connect now (e.g. at GUI start) without blocking.
    pub fn connect_in_background(&self) {
        let _ = self.sender.try_send(Outgoing::Connect);
    }
    /// Last connection/handshake failure, for diagnostics. Never contains the credential.
    pub fn last_error(&self) -> Option<String> {
        self.shared
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Raw command access for feature modules without a `LibraryClient` method
    /// (jobs, grants, status). Completion is exactly once after `Ok`.
    pub fn call(
        &self,
        command: Command,
        complete: impl FnOnce(Result<Reply, WireError>) + Send + 'static,
    ) -> Result<(), ServiceError> {
        self.send(command, None, None, Box::new(complete))
    }
    fn send(
        &self,
        command: Command,
        cancel: Option<Cancellation>,
        progress: Option<PdfProgress>,
        complete: RawCompletion,
    ) -> Result<(), ServiceError> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.sender
            .try_send(Outgoing::Request {
                id,
                command: Box::new(command),
                complete,
                progress,
                cancel,
            })
            .map_err(|_| ServiceError::Unavailable)
    }
    fn typed<T: Send + 'static>(
        &self,
        command: Command,
        cancel: Option<Cancellation>,
        progress: Option<PdfProgress>,
        complete: Box<dyn FnOnce(Result<T, ServiceError>) + Send>,
        extract: fn(Reply) -> Option<T>,
    ) -> Result<(), ServiceError> {
        self.send(
            command,
            cancel,
            progress,
            Box::new(move |result| {
                complete(match result {
                    Ok(reply) => extract(reply).ok_or(ServiceError::Unavailable),
                    Err(e) => Err(e.into()),
                })
            }),
        )
    }
}

struct Connection {
    writer: Box<dyn Stream>,
    generation: u64,
    alive: Arc<AtomicBool>,
}
impl Connection {
    /// Marks the connection dead and unblocks its reader, which fails its pending requests.
    fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.writer.shutdown_stream();
    }
}

fn connect(
    connector: &dyn Connector,
    shared: &Arc<Shared>,
    generation: u64,
    timeouts: &ClientTimeouts,
) -> Result<Connection, ClientError> {
    let (stream, _) = connect_and_handshake(connector, timeouts.handshake)?;
    // The reader blocks without a timeout (deadlines are enforced by the transport thread);
    // writes are bounded so a daemon that stops reading cannot wedge the transport.
    stream.set_io_timeouts(None, Some(timeouts.write))?;
    let mut reader = stream.try_clone_stream()?;
    let alive = Arc::new(AtomicBool::new(true));
    let reader_alive = alive.clone();
    let reader_shared = shared.clone();
    thread::Builder::new()
        .name("ipc-reader".into())
        .spawn(move || {
            loop {
                match read_frame::<_, ServerFrame>(reader.as_mut(), MAX_RESPONSE_FRAME) {
                    Ok(Some(ServerFrame::Response { id, result })) => {
                        if let Some(p) = reader_shared.take(id) {
                            (p.complete)(result);
                        }
                    }
                    Ok(Some(ServerFrame::PdfProgress { id, report })) => {
                        let mut pending = reader_shared.lock();
                        if let Some(p) = pending.get_mut(&id) {
                            // Progress proves the operation is alive: restart its deadline.
                            p.deadline = Instant::now() + p.timeout;
                            if let Some(progress) = p.progress.as_ref() {
                                progress(report);
                            }
                        }
                    }
                    Ok(Some(_)) | Ok(None) | Err(_) => break,
                }
            }
            reader_alive.store(false, Ordering::SeqCst);
            reader_shared.fail_generation(generation);
        })?;
    Ok(Connection {
        writer: stream,
        generation,
        alive,
    })
}

struct Transport<C: Connector> {
    connector: C,
    shared: Arc<Shared>,
    timeouts: ClientTimeouts,
    connection: Option<Connection>,
    generation: u64,
    /// Consecutive failed connects; reset by a successful handshake.
    failures: u32,
    /// No connect attempt before this instant (backoff).
    retry_at: Option<Instant>,
    cancels: Vec<(u64, Cancellation)>,
}
impl<C: Connector> Transport<C> {
    fn ensure(&mut self) -> bool {
        if self
            .connection
            .as_ref()
            .is_some_and(|c| c.alive.load(Ordering::SeqCst))
        {
            return true;
        }
        if let Some(old) = self.connection.take() {
            old.writer.shutdown_stream();
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        self.generation += 1;
        match connect(
            &self.connector,
            &self.shared,
            self.generation,
            &self.timeouts,
        ) {
            Ok(c) => {
                self.connection = Some(c);
                self.shared.set_error(None);
                self.failures = 0;
                self.retry_at = None;
                true
            }
            Err(e) => {
                self.shared.set_error(Some(e.to_string()));
                self.failures = self.failures.saturating_add(1);
                self.retry_at = self
                    .timeouts
                    .backoff(self.failures)
                    .map(|wait| Instant::now() + wait);
                false
            }
        }
    }
    fn run(mut self, receiver: mpsc::Receiver<Outgoing>) {
        loop {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(Outgoing::Connect) => {
                    self.ensure();
                }
                Ok(Outgoing::Request {
                    id,
                    command,
                    complete,
                    progress,
                    cancel,
                }) => {
                    if self.ensure() {
                        self.write(id, *command, complete, progress, cancel);
                    } else {
                        // Requests queued during the failed connect would each re-run it
                        // (possibly a slow daemon start): fail them now as well.
                        let mut failed = vec![complete];
                        while let Ok(queued) = receiver.try_recv() {
                            if let Outgoing::Request { complete, .. } = queued {
                                failed.push(complete);
                            }
                        }
                        for complete in failed {
                            complete(Err(self.shared.connect_error()));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.expire();
            self.forward_cancels();
        }
        if let Some(c) = self.connection {
            c.writer.shutdown_stream();
        }
    }
    fn write(
        &mut self,
        id: u64,
        command: Command,
        complete: RawCompletion,
        progress: Option<PdfProgress>,
        cancel: Option<Cancellation>,
    ) {
        let timeout = self.timeouts.for_command(&command);
        let c = self.connection.as_mut().expect("connected");
        self.shared.lock().insert(
            id,
            Pending {
                complete,
                progress,
                generation: c.generation,
                deadline: Instant::now() + timeout,
                timeout,
            },
        );
        let frame = ClientFrame::Request { id, command };
        match write_frame(c.writer.as_mut(), &frame, MAX_REQUEST_FRAME) {
            Ok(()) if c.alive.load(Ordering::SeqCst) => {
                if let Some(cancel) = cancel {
                    self.cancels.push((id, cancel));
                }
            }
            Ok(()) => {
                if let Some(p) = self.shared.take(id) {
                    (p.complete)(Err(WireError::Unavailable));
                }
            }
            Err(e) => {
                let error = match e {
                    crate::ipc::FrameError::TooLarge(_) => WireError::TooLarge,
                    // Failed or timed-out write: the stream may hold a partial frame.
                    _ => {
                        c.kill();
                        WireError::Unavailable
                    }
                };
                if let Some(p) = self.shared.take(id) {
                    (p.complete)(Err(error));
                }
            }
        }
    }
    /// Completes requests past their deadline with `Unavailable`.
    fn expire(&mut self) {
        let expired = self.shared.take_expired(Instant::now());
        let current = self.connection.as_ref().map(|c| c.generation);
        if !expired.iter().any(|(_, p)| Some(p.generation) == current) {
            for (_, p) in expired {
                (p.complete)(Err(WireError::Unavailable));
            }
            return;
        }
        for (id, p) in expired {
            if let Some(c) = self
                .connection
                .as_mut()
                .filter(|c| c.generation == p.generation && c.alive.load(Ordering::SeqCst))
                && write_frame(
                    c.writer.as_mut(),
                    &ClientFrame::Cancel { id },
                    MAX_REQUEST_FRAME,
                )
                .is_err()
            {
                c.kill();
            }
            (p.complete)(Err(WireError::Unavailable));
        }
        // Nothing else is waiting on this connection: treat the daemon as unresponsive and
        // drop it; the next request reconnects (and, if needed, restarts the daemon).
        if let Some(c) = self.connection.take() {
            if self.shared.has_generation(c.generation) {
                self.connection = Some(c);
            } else {
                c.kill();
            }
        }
    }
    fn forward_cancels(&mut self) {
        if self.cancels.is_empty() {
            return;
        }
        let pending: Vec<u64> = self.shared.lock().keys().copied().collect();
        self.cancels.retain(|(id, _)| pending.contains(id));
        let fired: Vec<u64> = self
            .cancels
            .iter()
            .filter(|(_, c)| c.is_cancelled())
            .map(|(id, _)| *id)
            .collect();
        self.cancels.retain(|(_, c)| !c.is_cancelled());
        if let Some(c) = self.connection.as_mut() {
            for id in fired {
                let _ = write_frame(
                    c.writer.as_mut(),
                    &ClientFrame::Cancel { id },
                    MAX_REQUEST_FRAME,
                );
            }
        }
    }
}

impl LibraryClient for IpcLibraryClient {
    fn submit(&self, request: LibraryRequest, complete: Completion) -> Result<(), ServiceError> {
        self.typed(
            Command::Library(request),
            None,
            None,
            complete,
            |r| match r {
                Reply::Library(reply) => Some(reply),
                _ => None,
            },
        )
    }
    fn query(
        &self,
        query: LibraryQuery,
        complete: crate::library::Reply<LibraryPage>,
    ) -> Result<(), ServiceError> {
        self.typed(Command::Query(query), None, None, complete, |r| match r {
            Reply::Page(page) => Some(page),
            _ => None,
        })
    }
    fn organization(
        &self,
        request: OrganizationRequest,
        complete: crate::library::Reply<Organization>,
    ) -> Result<(), ServiceError> {
        self.typed(
            Command::Organization(request),
            None,
            None,
            complete,
            |r| match r {
                Reply::Organization(o) => Some(o),
                _ => None,
            },
        )
    }
    fn lifecycle(
        &self,
        paper: crate::domain::PaperId,
        deleted: bool,
        complete: crate::library::Reply<crate::domain::Paper>,
    ) -> Result<(), ServiceError> {
        self.typed(
            Command::Lifecycle { paper, deleted },
            None,
            None,
            complete,
            |r| match r {
                Reply::Paper(p) => Some(*p),
                _ => None,
            },
        )
    }
    fn load_settings(
        &self,
        complete: crate::library::Reply<SettingsLoaded>,
    ) -> Result<(), ServiceError> {
        self.typed(Command::LoadSettings, None, None, complete, |r| match r {
            Reply::Settings(s) => Some(s),
            _ => None,
        })
    }
    fn save_settings(
        &self,
        settings: Settings,
        complete: crate::library::Reply<()>,
    ) -> Result<(), ServiceError> {
        self.typed(
            Command::SaveSettings(settings),
            None,
            None,
            complete,
            |r| match r {
                Reply::Unit => Some(()),
                _ => None,
            },
        )
    }
    fn import(
        &self,
        request: ImportRequestUi,
        complete: crate::library::Reply<ImportReply>,
    ) -> Result<(), ServiceError> {
        self.typed(
            Command::Import(request),
            None,
            None,
            complete,
            |r| match r {
                Reply::Import(i) => Some(i),
                _ => None,
            },
        )
    }
    fn resolve(
        &self,
        doi: String,
        cancel: Cancellation,
        complete: crate::library::Reply<MetadataResolution>,
    ) -> Result<(), ServiceError> {
        self.typed(
            Command::Resolve { doi },
            Some(cancel),
            None,
            complete,
            |r| match r {
                Reply::Resolution(m) => Some(*m),
                _ => None,
            },
        )
    }
    fn export(
        &self,
        request: ExportRequest,
        complete: crate::library::Reply<Exported>,
    ) -> Result<(), ServiceError> {
        self.typed(
            Command::Export(request),
            None,
            None,
            complete,
            |r| match r {
                Reply::Exported(e) => Some(e),
                _ => None,
            },
        )
    }
    fn annotations(
        &self,
        request: crate::annotations::AnnotationRequest,
        complete: crate::library::Reply<crate::annotations::AnnotationReply>,
    ) -> Result<(), ServiceError> {
        self.typed(
            Command::Annotations(request),
            None,
            None,
            complete,
            |r| match r {
                Reply::Annotations(a) => Some(a),
                _ => None,
            },
        )
    }
    fn pdf(
        &self,
        request: PdfRequest,
        progress: PdfProgress,
        complete: crate::library::Reply<PdfReply>,
    ) -> Result<(), ServiceError> {
        let (command, cancel) = PdfCommand::from_request(request);
        self.typed(
            Command::Pdf(command),
            cancel,
            Some(progress),
            complete,
            |r| match r {
                Reply::Pdf(p) => Some(p),
                _ => None,
            },
        )
    }
}
