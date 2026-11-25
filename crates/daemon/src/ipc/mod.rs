//! `papersd` local IPC server (#25): the single writer for one library.
//!
//! Start order: private dirs -> exclusive library lock -> migrations (with pre-migration
//! backup) -> grant registry + owner bootstrap credential -> library worker + job runner ->
//! stale socket cleanup -> bind (0600 socket in a 0700 directory) -> #27 REST listener
//! manager (listens only while the persisted API settings enable it).
//!
//! Per connection: OS peer check, a bounded/timed Hello, credential -> stored grant, then
//! every request is authorized against the *current* grant (default deny).
//!
//! #26: the same per-request decision covers scoped API/MCP/CLI tokens. On top of it the
//! daemon applies the bulk guard (`policy::elevation`), restricts `exports:write` tokens to
//! the allowed export folders, and records denials and non-read operations in the audit log.
pub mod auth;
mod capture;
/// #27 in-process adapters (REST) dispatch through the same `Session::dispatch`.
pub mod gateway;

use crate::{
    desktop_worker::InProcessLibraryClient,
    fulltext::readers::{MAX_SEARCH_READERS, ReaderPool},
    jobs::{CapturePdfFetchHandler, JobEngine, JobHandler, JobPolicy, default_handlers},
    lifecycle::{self, Activity, DaemonPaths, LibraryLock},
};
use auth::{Authenticator, IssueError, now_ms};
use cm_core::{
    ServiceError,
    metadata_resolve::Cancellation,
    policy::{
        self, Capability, CapabilitySet, Denial, GrantId, Operation,
        elevation::{BulkGuard, BulkKind},
    },
    search::extraction::PageTextExtractor,
};
use cm_metadata::fetch_policy::{FetchPolicy, HostResolver, PdfFetcher, SystemResolver};
use cm_protocol::{
    api::{AuditEntry, AuditOutcome},
    client::secrets::{PlatformSecretStore, SecretStore},
    organization::OrganizationRequest,
};
use cm_protocol::{
    attachments::PdfReply,
    exchange::{ImportReply, ImportRequestUi},
    import::{ImportAction, ImportCommit, ImportPreview, PreviewId},
    ipc::{
        ClientFrame, Command, DaemonStatus, GrantCommand, GrantReply, HandshakeRejection,
        MAX_HANDSHAKE_FRAME, MAX_IN_FLIGHT, MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME,
        PROTOCOL_VERSION, PdfCommand, Reply, ServerFrame, Welcome, WireError, read_frame,
        write_frame,
    },
    jobs::{JobCommand, JobReply},
    library::LibraryClient,
    search::{FulltextReply, FulltextRequest, PageTextRequest, SearchReply, SearchRequest},
};
use cm_storage::{LibraryDb, LibraryDirectory, SqlFault, StorageError, fts::SearchReader};
use std::{
    collections::HashMap,
    io,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 64;

#[derive(Clone)]
pub struct DaemonConfig {
    pub library: PathBuf,
    pub runtime_dir: Option<PathBuf>,
    pub settings: Option<PathBuf>,
    /// Exit after this long with no connection and no active job.
    pub idle_timeout: Duration,
    pub jobs: JobPolicy,
    /// Extra/overriding job handlers (registered after the built-in capture handler).
    pub handlers: Vec<Arc<dyn JobHandler>>,
    /// #18 page text extractor. `Some` registers `fulltext.extract` and automatic
    /// extraction of pending PDF attachments; `None` (no PDF runtime) leaves PDF text
    /// unindexed and searches report it as `PendingExtraction`.
    pub extractor: Option<Arc<dyn PageTextExtractor>>,
    /// #26 store for issued token plaintext. `None` = the platform store (Keychain /
    /// Secret Service) with the private-file fallback. Tests inject a memory store.
    pub secret_store: Option<Arc<dyn SecretStore>>,
    /// #26 size of each of the two audit log files.
    pub audit_max_bytes: u64,
    /// #27 REST listener options (origins, limits, request log). Whether it listens, and
    /// where, comes only from the persisted API settings.
    pub rest: crate::rest::RestConfig,
    /// #22 capture PDF fetch policy and name resolution.
    pub capture: CaptureConfig,
}
/// #22: how capture previews filter PDF candidates and how `capture.pdf_fetch` downloads.
#[derive(Clone)]
pub struct CaptureConfig {
    pub fetch: FetchPolicy,
    pub resolver: Arc<dyn HostResolver>,
}
impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            fetch: FetchPolicy::default(),
            resolver: Arc::new(SystemResolver),
        }
    }
}
impl DaemonConfig {
    pub fn new(library: PathBuf) -> Self {
        Self {
            library,
            runtime_dir: None,
            settings: None,
            idle_timeout: Duration::from_secs(30),
            jobs: JobPolicy::default(),
            handlers: default_handlers(),
            capture: CaptureConfig::default(),
            extractor: None,
            secret_store: None,
            audit_max_bytes: crate::audit::DEFAULT_MAX_BYTES,
            rest: crate::rest::RestConfig::default(),
        }
    }
}

#[derive(Debug)]
pub enum StartError {
    /// Another live daemon holds the library lock (normal outcome of a start race).
    AlreadyRunning,
    Io(io::Error),
    Storage(StorageError),
    Service(ServiceError),
}
impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => f.write_str("another papersd already serves this library"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Storage(e) => write!(f, "{e}"),
            Self::Service(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for StartError {}
impl From<io::Error> for StartError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

struct Shared {
    paths: DaemonPaths,
    auth: Arc<Authenticator>,
    engine: InProcessLibraryClient,
    jobs: JobEngine,
    /// #18 pool of interruptible read connections for search/extraction status (WAL
    /// readers do not block each other or the writer).
    readers: ReaderPool,
    activity: Arc<Activity>,
    sessions: AtomicU64,
    /// Import preview handle -> grant that created it (preview grant binding).
    previews: Mutex<HashMap<PreviewId, PreviewOwner>>,
    /// Live authenticated connections, closed on explicit shutdown.
    streams: Mutex<HashMap<u64, UnixStream>>,
    /// #26 token management, API settings and the audit log.
    api: crate::api::ApiService,
    /// #26 per-grant burst limit for trash/delete and membership changes.
    bulk: Mutex<BulkGuard>,
    /// #27 in-process adapter sessions (closed and drained before the lock is released).
    internal: Arc<gateway::Tracker>,
    /// #22 capture preview -> creating grant and offered PDF candidates.
    captures: Mutex<capture::Bindings>,
    capture: CaptureConfig,
}

/// A started daemon. `wait` blocks until idle exit or `shutdown`.
pub struct RunningDaemon {
    pub paths: DaemonPaths,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl RunningDaemon {
    pub fn wait(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
    pub fn shutdown(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wait();
    }
}
impl Drop for RunningDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start(config: DaemonConfig) -> Result<RunningDaemon, StartError> {
    let paths = DaemonPaths::new(
        &config.library,
        config.runtime_dir.as_deref(),
        lifecycle::euid(),
    )?;
    let directory = LibraryDirectory::prepare(&paths.library).map_err(StartError::Storage)?;
    lifecycle::prepare_private_dir(&paths.state_dir)?;
    let lock = LibraryLock::try_acquire(&paths.lock)?.ok_or(StartError::AlreadyRunning)?;
    // Only the lock holder runs jobs, so no fetch can be writing these right now.
    crate::jobs::sweep_stale_downloads(directory.path());
    // The lock holder is the only migration runner. Upgrades take a pre-migration backup.
    let control = LibraryDb::open_with_safety_backups(&directory).map_err(StartError::Storage)?;
    let control = Arc::new(Mutex::new(control));
    let readers =
        ReaderPool::new(directory.clone(), MAX_SEARCH_READERS).map_err(StartError::Storage)?;
    let auth = Arc::new(Authenticator::load(control.clone()).map_err(StartError::Service)?);
    auth.bootstrap_desktop(&paths.desktop_credential, now_ms())?;
    let secrets = match config.secret_store {
        Some(store) => store,
        None => Arc::new(PlatformSecretStore::detect(
            paths.secrets_dir(),
            lifecycle::euid(),
        )),
    };
    let api = crate::api::ApiService::new(
        auth.clone(),
        secrets,
        paths.library_key(),
        paths.library.clone(),
        Arc::new(crate::audit::AuditLog::new(
            paths.audit_log(),
            config.audit_max_bytes,
        )),
    )
    .map_err(StartError::Service)?;
    let activity = Arc::new(Activity::default());
    let (engine, engine_exit) =
        InProcessLibraryClient::start_with_exit(paths.library.clone(), config.settings)?;
    // Built-in #22 capture handler first, then the configured (possibly overriding) handlers,
    // then #18 extraction when a page text extractor is available.
    let mut handlers: Vec<Arc<dyn JobHandler>> = vec![Arc::new(CapturePdfFetchHandler::new(
        PdfFetcher::with_resolver(
            config.capture.fetch.clone(),
            config.capture.resolver.clone(),
        ),
    ))];
    handlers.extend(config.handlers);
    if let Some(extractor) = config.extractor {
        handlers.push(Arc::new(crate::fulltext::FulltextExtractHandler {
            extractor,
        }));
    }
    // The live grant registry decides whether a job's owner may still run it.
    let jobs = JobEngine::start_with_owners(
        directory,
        control,
        handlers,
        config.jobs,
        activity.clone(),
        auth.clone(),
    )
    .map_err(StartError::Service)?;
    // Attachments added while no daemon (or no PDF runtime) was running.
    jobs.ensure_extraction();
    lifecycle::prepare_private_dir(&paths.runtime_dir)?;
    lifecycle::remove_stale_socket(&paths.socket)?;
    let listener = UnixListener::bind(&paths.socket)?;
    std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let shared = Arc::new(Shared {
        paths: paths.clone(),
        auth,
        engine,
        jobs,
        readers,
        activity,
        sessions: AtomicU64::new(0),
        previews: Mutex::new(HashMap::new()),
        streams: Mutex::new(HashMap::new()),
        api,
        bulk: Mutex::new(BulkGuard::default()),
        internal: Arc::default(),
        captures: Mutex::new(capture::Bindings::default()),
        capture: config.capture,
    });
    let rest = crate::rest::Manager::start(gateway::Gateway(shared.clone()), config.rest)?;
    let stop = Arc::new(AtomicBool::new(false));
    let accept_stop = stop.clone();
    let idle = config.idle_timeout;
    let thread = thread::Builder::new()
        .name("papersd-accept".into())
        .spawn(move || {
            accept_loop(&listener, &shared, &accept_stop, idle);
            // Stop the REST listener (its runtime and every in-flight request session, so no
            // adapter thread keeps `shared` alive), stop accepting, then remove our socket while
            // still holding the lock.
            rest.stop();
            drop(listener);
            let _ = std::fs::remove_file(&shared.paths.socket);
            for stream in shared
                .streams
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
            {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while shared.activity.connections() > 0 && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            shared.jobs.stop();
            shared.auth.flush_last_used();
            shared.api.audit.flush();
            drop(shared);
            // Release the single-writer lock only after the library worker has closed its
            // connections; otherwise the next daemon could open the library (and see its
            // WAL sidecars being torn down) while this one still holds it open.
            if !engine_exit.wait(Duration::from_secs(10)) {
                eprintln!("papersd: library worker did not exit within 10 s");
            }
            drop(lock);
        })?;
    Ok(RunningDaemon {
        paths,
        stop,
        thread: Some(thread),
    })
}

fn accept_loop(listener: &UnixListener, shared: &Arc<Shared>, stop: &AtomicBool, idle: Duration) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if shared.activity.connections() >= MAX_CONNECTIONS {
                    continue;
                }
                let guard = shared.activity.connect();
                let shared = shared.clone();
                let _ = thread::Builder::new()
                    .name("papersd-conn".into())
                    .spawn(move || {
                        let _guard = guard;
                        serve(stream, &shared);
                    });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if shared.activity.idle_for(idle) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn reject(stream: &mut UnixStream, reason: HandshakeRejection) {
    let _ = write_frame(stream, &ServerFrame::Rejected(reason), MAX_HANDSHAKE_FRAME);
}

fn serve(mut stream: UnixStream, shared: &Arc<Shared>) {
    let _ = stream.set_nonblocking(false);
    if !auth::peer_allowed(&stream) {
        reject(&mut stream, HandshakeRejection::PeerRejected);
        return;
    }
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let hello = match read_frame::<_, ClientFrame>(&mut stream, MAX_HANDSHAKE_FRAME) {
        Ok(Some(ClientFrame::Hello(hello))) => hello,
        _ => {
            reject(&mut stream, HandshakeRejection::Malformed);
            return;
        }
    };
    if hello.protocol_version != PROTOCOL_VERSION {
        reject(
            &mut stream,
            HandshakeRejection::VersionMismatch {
                supported: PROTOCOL_VERSION,
            },
        );
        return;
    }
    if hello
        .library
        .as_ref()
        .is_some_and(|l| *l != shared.paths.library)
    {
        reject(&mut stream, HandshakeRejection::WrongLibrary);
        return;
    }
    let grant = match shared.auth.authenticate(&hello.credential, now_ms()) {
        Ok(grant) => grant,
        Err(_) => {
            reject(&mut stream, HandshakeRejection::Unauthenticated);
            return;
        }
    };
    let session = shared.sessions.fetch_add(1, Ordering::Relaxed) + 1;
    let welcome = Welcome {
        protocol_version: PROTOCOL_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        session,
        grant: grant.id,
        capabilities: policy::effective_at(&grant, hello.requested.as_ref(), now_ms()),
        max_request_frame: MAX_REQUEST_FRAME,
        max_in_flight: MAX_IN_FLIGHT as u32,
    };
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let Ok(overflow) = stream.try_clone() else {
        return;
    };
    let (tx, rx) = mpsc::sync_channel::<Outbound>(OUTBOUND_QUEUE);
    let outbox = Outbox {
        tx,
        stream: Some(Arc::new(overflow)),
    };
    let writer_thread = thread::Builder::new()
        .name("papersd-writer".into())
        .spawn(move || {
            for Outbound { frame, slot } in rx {
                let result = match write_frame(&mut writer, &frame, MAX_RESPONSE_FRAME) {
                    Err(cm_protocol::ipc::FrameError::TooLarge(_)) => match frame {
                        ServerFrame::Response { id, .. } => write_frame(
                            &mut writer,
                            &ServerFrame::Response {
                                id,
                                result: Err(WireError::TooLarge),
                            },
                            MAX_RESPONSE_FRAME,
                        ),
                        _ => Ok(()),
                    },
                    other => other,
                };
                // The request's in-flight slot is released only now that its reply left.
                drop(slot);
                if result.is_err() {
                    let _ = writer.shutdown(std::net::Shutdown::Both);
                    break;
                }
            }
        });
    if writer_thread.is_err() || !outbox.send(ServerFrame::Welcome(welcome), None) {
        return;
    }
    let _ = stream.set_read_timeout(None);
    if let Ok(clone) = stream.try_clone() {
        shared
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session, clone);
    }
    let session = Session {
        id: session,
        grant: grant.id,
        requested: hello.requested,
        engine: shared.engine.for_session(session),
        shared: shared.clone(),
        outbox,
        in_flight: Arc::new(AtomicUsize::new(0)),
        cancels: Arc::new(Mutex::new(HashMap::new())),
    };
    loop {
        match read_frame::<_, ClientFrame>(&mut stream, MAX_REQUEST_FRAME) {
            Ok(Some(ClientFrame::Request { id, command })) => session.dispatch(id, command),
            Ok(Some(ClientFrame::Cancel { id })) => {
                if let Some(c) = session
                    .cancels
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&id)
                {
                    c.cancel();
                }
            }
            Ok(Some(ClientFrame::Hello(_))) | Ok(None) | Err(_) => break,
        }
    }
    // Cancel this connection's outstanding work; its replies have nowhere to go.
    for c in session
        .cancels
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
    {
        c.cancel();
    }
    session.engine.end_session(session.id);
    shared
        .streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&session.id);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Frames queued per connection before the client counts as not reading. Responses hold
/// their request's in-flight slot until written, so at most `MAX_IN_FLIGHT` of them are
/// queued; the rest of the room is for immediate refusals (`Busy`, denials) and progress.
const OUTBOUND_QUEUE: usize = 2 * MAX_IN_FLIGHT;

/// One request's share of `MAX_IN_FLIGHT`, released when dropped (after its reply was
/// written, or when the reply is discarded).
struct Slot(Arc<AtomicUsize>);
impl Slot {
    fn take(in_flight: &Arc<AtomicUsize>) -> Self {
        in_flight.fetch_add(1, Ordering::SeqCst);
        Self(in_flight.clone())
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Outbound {
    frame: ServerFrame,
    slot: Option<Slot>,
}

/// Bounded outbound queue of one session. Nothing that produces replies (the shared library
/// worker included) ever blocks on it: a client that keeps sending but stops reading fills
/// it and is disconnected, so its unread replies cannot grow the daemon's memory.
#[derive(Clone)]
struct Outbox {
    tx: mpsc::SyncSender<Outbound>,
    /// Shut down when the queue is full; `None` for in-process adapter sessions, which
    /// drain their queue themselves.
    stream: Option<Arc<UnixStream>>,
}
impl Outbox {
    /// Queues a frame. `false` if it was not queued (writer gone, or the client was
    /// disconnected for not reading). Progress reports are best effort: a full queue drops
    /// them without disconnecting.
    fn send(&self, frame: ServerFrame, slot: Option<Slot>) -> bool {
        match self.tx.try_send(Outbound { frame, slot }) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Disconnected(_)) => false,
            Err(mpsc::TrySendError::Full(rejected)) => {
                if !matches!(rejected.frame, ServerFrame::PdfProgress { .. })
                    && let Some(stream) = &self.stream
                {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                false
            }
        }
    }
}

/// Sends exactly one response for a request id; its in-flight slot travels with the reply
/// and is released once the reply was written.
type Pending = (u64, Outbox, Slot);
#[derive(Clone)]
struct Responder(Arc<Mutex<Option<Pending>>>);
impl Responder {
    fn send(&self, result: Result<Reply, WireError>) {
        if let Some((id, outbox, slot)) = self.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
            outbox.send(ServerFrame::Response { id, result }, Some(slot));
        }
    }
}

struct Session {
    id: u64,
    grant: GrantId,
    requested: Option<CapabilitySet>,
    engine: InProcessLibraryClient,
    shared: Arc<Shared>,
    outbox: Outbox,
    in_flight: Arc<AtomicUsize>,
    cancels: Arc<Mutex<HashMap<u64, Cancellation>>>,
}

impl Session {
    fn respond_now(&self, id: u64, result: Result<Reply, WireError>) {
        self.outbox.send(ServerFrame::Response { id, result }, None);
    }
    fn preview_owned(&self, preview: &PreviewId) -> bool {
        self.shared
            .previews
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(preview)
            .map(|owner| owner.grant)
            == Some(self.grant)
    }
    /// #27: a caller that cannot read papers gets exact duplicates skipped on commit,
    /// decided from the unredacted preview (it never learned which rows matched).
    fn skip_hidden_duplicates(&self, commit: &mut ImportCommit, redacted: bool) {
        if !redacted {
            return;
        }
        let hidden = self
            .shared
            .previews
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&commit.preview_id)
            .and_then(|owner| owner.hidden_duplicates.clone());
        let Some(hidden) = hidden else {
            return;
        };
        for decision in &mut commit.decisions {
            if hidden.get(decision.candidate_index) == Some(&true)
                && decision.action == ImportAction::Create
            {
                decision.action = ImportAction::Skip;
            }
        }
    }
    fn cancellation(&self, id: u64) -> Cancellation {
        let cancel = Cancellation::default();
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, cancel.clone());
        cancel
    }

    fn dispatch(&self, id: u64, command: Command) {
        if self.in_flight.load(Ordering::SeqCst) >= MAX_IN_FLIGHT {
            self.respond_now(id, Err(WireError::Busy));
            return;
        }
        let operation = command.operation();
        let held = match admit(
            &self.shared,
            self.grant,
            self.requested.as_ref(),
            &command,
            now_ms(),
        ) {
            Ok(held) => held,
            Err(error) => {
                self.respond_now(id, Err(error));
                return;
            }
        };
        let target = match &command {
            Command::Api(c) => api_target(c),
            Command::Grants(GrantCommand::Revoke(g)) => Some(*g),
            _ => None,
        };
        let responder = Responder(Arc::new(Mutex::new(Some((
            id,
            self.outbox.clone(),
            Slot::take(&self.in_flight),
        )))));
        let cancels = self.cancels.clone();
        let done = {
            let responder = responder.clone();
            let audit = crate::api::audited(operation).then(|| {
                (
                    self.shared.api.audit.clone(),
                    self.grant,
                    self.shared.auth.kind_of(self.grant),
                )
            });
            move |result: Result<Reply, WireError>| {
                cancels
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                if let Some((log, grant, kind)) = audit {
                    log.record(audit_entry(grant, kind, operation, &result, target));
                }
                responder.send(result);
            }
        };
        let denied = |op| Err(WireError::Denied(Denial::Forbidden(op)));
        let submitted: Result<(), ServiceError> = match command {
            Command::Status => {
                done(Ok(Reply::Status(DaemonStatus {
                    daemon_version: env!("CARGO_PKG_VERSION").into(),
                    protocol_version: PROTOCOL_VERSION,
                    pid: std::process::id(),
                    session: self.id,
                    grant: self.grant,
                    capabilities: held,
                    connections: self.shared.activity.connections() as u32,
                    active_jobs: self.shared.activity.active_jobs() as u32,
                })));
                Ok(())
            }
            Command::Library(request) => self.engine.submit(
                request,
                Box::new(move |r| done(r.map(Reply::Library).map_err(Into::into))),
            ),
            Command::Query(query) => self.engine.query(
                query,
                Box::new(move |r| done(r.map(Reply::Page).map_err(Into::into))),
            ),
            Command::Organization(request) => self.engine.organization(
                request,
                Box::new(move |r| done(r.map(Reply::Organization).map_err(Into::into))),
            ),
            Command::Lifecycle { paper, deleted } => self.engine.lifecycle(
                paper,
                deleted,
                Box::new(move |r| done(r.map(|p| Reply::Paper(Box::new(p))).map_err(Into::into))),
            ),
            Command::LoadSettings => self.engine.load_settings(Box::new(move |r| {
                done(r.map(Reply::Settings).map_err(Into::into))
            })),
            Command::SaveSettings(settings) => self.engine.save_settings(
                settings,
                Box::new(move |r| done(r.map(|()| Reply::Unit).map_err(Into::into))),
            ),
            Command::Import(mut request) => {
                let redact = !held.contains(&Capability::LibraryRead)
                    && !held.contains(&Capability::MetadataRead);
                if let ImportRequestUi::Commit(commit) = &mut request {
                    self.skip_hidden_duplicates(commit, redact);
                }
                match &request {
                    ImportRequestUi::Commit(c) if !self.preview_owned(&c.preview_id) => {
                        done(denied(Operation::ImportCommit));
                        return;
                    }
                    ImportRequestUi::Cancel(p) if !self.preview_owned(p) => {
                        done(denied(Operation::ImportCancel));
                        return;
                    }
                    _ => {}
                }
                let previews = self.shared.clone();
                let grant = self.grant;
                self.engine.import(
                    request,
                    Box::new(move |r| {
                        done(
                            r.map(|reply| match reply {
                                ImportReply::Preview(mut p) => {
                                    let hidden_duplicates = redact.then(|| redact_matches(&mut p));
                                    previews
                                        .previews
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .insert(
                                            p.id.clone(),
                                            PreviewOwner {
                                                grant,
                                                hidden_duplicates,
                                            },
                                        );
                                    Reply::Import(ImportReply::Preview(p))
                                }
                                other => Reply::Import(other),
                            })
                            .map_err(Into::into),
                        )
                    }),
                )
            }
            Command::Resolve { doi } => self.engine.resolve(
                doi,
                self.cancellation(id),
                Box::new(move |r| {
                    done(
                        r.map(|m| Reply::Resolution(Box::new(m)))
                            .map_err(Into::into),
                    )
                }),
            ),
            Command::Export(request) => self.engine.export(
                request,
                Box::new(move |r| done(r.map(Reply::Exported).map_err(Into::into))),
            ),
            Command::Pdf(mut command) => {
                let redact = !held.contains(&Capability::LibraryRead)
                    && !held.contains(&Capability::MetadataRead);
                if let PdfCommand::CommitNew { commit, .. } = &mut command {
                    self.skip_hidden_duplicates(commit, redact);
                }
                if let PdfCommand::CommitNew { commit, .. } = &command
                    && !self.preview_owned(&commit.preview_id)
                {
                    done(denied(Operation::PdfCommitNew));
                    return;
                }
                let outbox = self.outbox.clone();
                let previews = self.shared.clone();
                let grant = self.grant;
                let adds_attachments = matches!(
                    command,
                    PdfCommand::Attach { .. }
                        | PdfCommand::CommitNew { .. }
                        | PdfCommand::RetryAttachment { .. }
                );
                self.engine.pdf(
                    command.into_request(self.cancellation(id)),
                    Box::new(move |report| {
                        outbox.send(ServerFrame::PdfProgress { id, report }, None);
                    }),
                    Box::new(move |mut r| {
                        if let Ok(PdfReply::Preview(p)) = &mut r {
                            // Same rule as import previews: no read scope, no existing matches.
                            let hidden_duplicates = redact.then(|| redact_matches(p));
                            previews
                                .previews
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(
                                    p.id.clone(),
                                    PreviewOwner {
                                        grant,
                                        hidden_duplicates,
                                    },
                                );
                        }
                        if adds_attachments && r.is_ok() {
                            previews.jobs.ensure_extraction();
                        }
                        done(r.map(Reply::Pdf).map_err(Into::into))
                    }),
                )
            }
            Command::Jobs(command) => {
                let owner = (!policy::authorizer::sees_all_owners(&held)).then_some(self.grant);
                let jobs = &self.shared.jobs;
                let result = match command {
                    JobCommand::Submit(submit) => jobs
                        .submit(self.grant, &held, submit)
                        .map(|s| JobReply::Job(Box::new(s))),
                    JobCommand::Get(job) => {
                        jobs.get(job, owner).map(|s| JobReply::Job(Box::new(s)))
                    }
                    JobCommand::List { active_only } => {
                        jobs.list(owner, active_only).map(JobReply::Jobs)
                    }
                    JobCommand::Cancel(job) => {
                        jobs.cancel(job, owner).map(|s| JobReply::Job(Box::new(s)))
                    }
                    JobCommand::Retry(job) => jobs
                        .retry(job, owner, &held)
                        .map(|s| JobReply::Job(Box::new(s))),
                    _ => Err(WireError::Service(ServiceError::InvalidInput(
                        "unsupported job command".into(),
                    ))),
                };
                done(result.map(Reply::Jobs));
                Ok(())
            }
            Command::Grants(command) => {
                let auth = &self.shared.auth;
                let result = match command {
                    GrantCommand::Issue {
                        label,
                        capabilities,
                        expires_at_ms,
                    } => auth
                        .issue(
                            Some(self.grant),
                            self.requested.as_ref(),
                            &label,
                            capabilities,
                            expires_at_ms,
                            now_ms(),
                        )
                        .map(|(grant, credential)| GrantReply::Issued { grant, credential })
                        .map_err(|e| match e {
                            IssueError::Denied(d) => WireError::Denied(d),
                            IssueError::Invalid(r) => {
                                WireError::Service(ServiceError::InvalidInput(r.into()))
                            }
                            IssueError::Service(e) => WireError::Service(e),
                            IssueError::Unavailable => WireError::Unavailable,
                        }),
                    GrantCommand::List => Ok(GrantReply::Grants(auth.list())),
                    GrantCommand::Revoke(grant) => {
                        // Same order as `ApiService::revoke`: registry, jobs, then the secret.
                        let revoked = auth.revoke(grant, now_ms());
                        // Fail closed: the grant is revoked in memory even if persisting failed,
                        // and its jobs must not keep running with its authority.
                        let jobs = match revoked {
                            Ok(false) => Ok(0),
                            Ok(true) | Err(_) => self.shared.jobs.owner_revoked(grant),
                        };
                        if !matches!(revoked, Ok(false)) {
                            self.shared.api.forget_secret(grant);
                            self.shared
                                .bulk
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .forget(grant);
                        }
                        match (revoked, jobs) {
                            (Ok(true), Ok(_)) => Ok(GrantReply::Revoked(grant)),
                            (Ok(false), _) => Err(WireError::Service(ServiceError::NotFound)),
                            (Err(e), _) => Err(WireError::Service(e)),
                            (Ok(true), Err(e)) => Err(e),
                        }
                    }
                    _ => Err(WireError::Service(ServiceError::InvalidInput(
                        "unsupported grant command".into(),
                    ))),
                };
                done(result.map(Reply::Grants));
                Ok(())
            }
            Command::Api(command) => {
                let revoked = match &command {
                    cm_protocol::api::ApiCommand::Revoke(g)
                    | cm_protocol::api::ApiCommand::Reissue(g) => Some(*g),
                    _ => None,
                };
                let result = self.shared.api.handle(
                    self.grant,
                    self.requested.as_ref(),
                    command,
                    &self.shared.jobs,
                    now_ms(),
                );
                if let Some(g) = revoked.filter(|_| result.is_ok()) {
                    self.shared
                        .bulk
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .forget(g);
                }
                done(result.map(Reply::Api));
                Ok(())
            }
            Command::Search(request) => {
                done(self.search(&request).map(Reply::Search));
                Ok(())
            }
            Command::Fulltext(request) => {
                done(self.fulltext(request).map(Reply::Fulltext));
                Ok(())
            }
            Command::Annotations(mut request) => {
                // The wire form carries no token (`#[serde(skip)]`): Propose gets this request's
                // server-side token, cancelled by `ClientFrame::Cancel` or by disconnecting.
                if let cm_protocol::annotations::AnnotationRequest::Propose { cancel, .. } =
                    &mut request
                {
                    *cancel = self.cancellation(id);
                }
                if let Err(error) = annotation_provenance(&request, &held) {
                    done(Err(error));
                    return;
                }
                self.engine.annotations(
                    request,
                    Box::new(move |r| done(r.map(Reply::Annotations).map_err(Into::into))),
                )
            }
            Command::Capture(command) => self.capture(id, command, held, done),
            _ => {
                done(Err(WireError::Service(ServiceError::InvalidInput(
                    "unsupported command".into(),
                ))));
                Ok(())
            }
        };
        if let Err(e) = submitted {
            // The engine refused to enqueue (queue full/closed); the callback never runs.
            responder.send(Err(WireError::Service(e)));
            self.cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
        }
    }
}

/// The per-request decision shared by every transport (IPC sessions and the in-process REST
/// adapter, which dispatches through `Session::dispatch` as well): the operation's capability
/// requirement against the grant's *current* state, then the #26 guards. Denials are audited.
fn admit(
    shared: &Shared,
    grant: GrantId,
    requested: Option<&CapabilitySet>,
    command: &Command,
    now: i64,
) -> Result<CapabilitySet, WireError> {
    let operation = command.operation();
    let held = match shared.auth.authorize(grant, requested, operation, now) {
        Ok(held) => held,
        Err(denial) => {
            let error = WireError::Denied(denial);
            record_denial(shared, grant, operation, &error);
            return Err(error);
        }
    };
    if let Err(error) = guard(shared, grant, command, &held) {
        // Bulk refusals are recorded as the bulk decision (admin audit sink).
        let decided = match &error {
            WireError::Denied(Denial::Forbidden(op)) => *op,
            _ => operation,
        };
        record_denial(shared, grant, decided, &error);
        return Err(error);
    }
    Ok(held)
}

/// #26 checks after the operation's capability requirement passed: the per-kind bulk guard
/// (decided as `BulkTrash`/`BulkMove`) and export folders for tokens without
/// `LocalFileWrite` (real-directory containment, `.bib`/`.ris` only).
fn guard(
    shared: &Shared,
    grant: GrantId,
    command: &Command,
    held: &CapabilitySet,
) -> Result<(), WireError> {
    let kind = bulk_kind(command);
    // Exempt: the owner's permanent BulkElevated, or a running elevation for exactly this
    // kind (and the session was not narrowed to exclude BulkElevated).
    let exempt = kind.is_some_and(|kind| {
        held.contains(&Capability::BulkElevated)
            && (shared
                .auth
                .stored(grant)
                .is_some_and(|s| s.grant.capabilities.contains(&Capability::BulkElevated))
                || shared.auth.elevation_of(grant) == Some(kind))
    });
    shared
        .bulk
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .check(grant, kind, exempt, Instant::now())
        .map_err(|bulk| WireError::Denied(Denial::Forbidden(bulk)))?;
    if let Command::Export(request) = command
        && !held.contains(&Capability::LocalFileWrite)
        && !export_allowed(request, &shared.api.export_dirs())
    {
        return Err(WireError::Denied(Denial::Forbidden(Operation::Export)));
    }
    Ok(())
}

fn record_denial(shared: &Shared, grant: GrantId, operation: Operation, error: &WireError) {
    shared.api.audit.record(audit_entry(
        grant,
        shared.auth.kind_of(grant),
        operation,
        &Err(error.clone()),
        None,
    ));
}

impl Session {
    /// #18 search on a pooled read connection, inside one read snapshot and within the
    /// request's `timeout_ms` (enforced inside SQLite; an interrupted lookup is reported as
    /// a timed-out page by the search service).
    ///
    /// Only real damage leads to recovery: a failure classified as corruption verifies the
    /// index, and only a failed verification schedules one `search.rebuild` job (which
    /// renumbers documents). Lock contention (BUSY/LOCKED), here or during verification,
    /// is returned as `WireError::Busy` (retry later) and never triggers a rebuild. The
    /// source tables are never modified by the search path.
    fn search(&self, request: &SearchRequest) -> Result<SearchReply, WireError> {
        let budget = request
            .timeout_ms
            .clamp(1, cm_core::search::types::MAX_TIMEOUT_MS);
        let reader = self
            .shared
            .readers
            .acquire(Instant::now() + Duration::from_millis(u64::from(budget)))?;
        let (error, fault) = {
            let snapshot = reader.snapshot().map_err(|_| WireError::Unavailable)?;
            match cm_core::search::search(&*snapshot, request) {
                Ok(reply) => return Ok(reply),
                Err(e) => (e, snapshot.fault()),
            }
        };
        match fault {
            Some(SqlFault::Busy) => Err(WireError::Busy),
            Some(SqlFault::Corrupt) => self.recover_index(&reader),
            _ => Err(WireError::Service(error)),
        }
    }
    /// A search hit corruption: verify, and schedule a rebuild only for confirmed damage.
    fn recover_index(&self, reader: &SearchReader) -> Result<SearchReply, WireError> {
        let index = reader.search_index();
        let health = match index.verify() {
            Ok(health) => health,
            Err(_) if index.fault() == Some(SqlFault::Busy) => return Err(WireError::Busy),
            Err(e) => return Err(WireError::Service(e)),
        };
        if health.ok() {
            return Err(WireError::Service(ServiceError::Unavailable));
        }
        let generation = index.generation().unwrap_or_default();
        let job = self.shared.jobs.submit_system(
            cm_protocol::jobs::kinds::SEARCH_REBUILD,
            &format!("auto-rebuild:{generation}"),
            serde_json::json!({}),
        )?;
        Err(WireError::Service(ServiceError::InvalidInput(format!(
            "search index is damaged; rebuild job {} scheduled",
            job.id.0
        ))))
    }
    fn fulltext(&self, request: FulltextRequest) -> Result<FulltextReply, WireError> {
        let reader = self
            .shared
            .readers
            .acquire(Instant::now() + FULLTEXT_READ_WAIT)?;
        let reader = reader.fulltext_reader();
        Ok(match request {
            FulltextRequest::Status(paper) => FulltextReply::Status(reader.for_paper(paper)?),
            FulltextRequest::AttachmentStatus(attachment) => {
                FulltextReply::Status(reader.for_attachment(attachment)?.into_iter().collect())
            }
            FulltextRequest::Pages(request) => {
                FulltextReply::Pages(Box::new(reader.pages(&request)?))
            }
            FulltextRequest::PaperPages(request) => {
                // Same limits as `Pages`, checked before anything is looked up.
                PageTextRequest {
                    attachment: cm_protocol::domain::AttachmentId(request.paper.0),
                    from_page: request.from_page,
                    max_pages: request.max_pages,
                    max_chars: request.max_chars,
                }
                .validate()?;
                let attachments = reader.for_paper(request.paper)?;
                let attachment = match request.attachment {
                    Some(wanted) => attachments.iter().find(|a| a.attachment_id == wanted),
                    None => attachments.first(),
                }
                .ok_or(ServiceError::NotFound)?
                .attachment_id;
                FulltextReply::Pages(Box::new(reader.pages(&PageTextRequest {
                    attachment,
                    from_page: request.from_page,
                    max_pages: request.max_pages,
                    max_chars: request.max_chars,
                })?))
            }
            _ => {
                return Err(WireError::Service(ServiceError::InvalidInput(
                    "unsupported full-text request".into(),
                )));
            }
        })
    }
}

/// Which burst limit a command counts toward (`policy::elevation`).
fn bulk_kind(command: &Command) -> Option<BulkKind> {
    use cm_protocol::annotations::AnnotationRequest;
    match command {
        Command::Lifecycle { .. }
        | Command::Organization(
            OrganizationRequest::DeleteCollection(_) | OrganizationRequest::DeleteTag(_),
        )
        | Command::Annotations(AnnotationRequest::Delete(_)) => Some(BulkKind::Delete),
        Command::Organization(
            OrganizationRequest::CollectionMember(..)
            | OrganizationRequest::MoveCollectionMember(..)
            | OrganizationRequest::TagMember(..),
        ) => Some(BulkKind::Move),
        _ => None,
    }
}

/// `exports:write` destinations: lexically inside an allowed folder, the parent chain
/// really contains that folder (same device and inode, so bind mounts or case-folding
/// spellings cannot fake containment), and the extension matches the format.
fn export_allowed(request: &cm_protocol::exchange::ExportRequest, allowed: &[PathBuf]) -> bool {
    use std::os::unix::fs::MetadataExt;
    let extension = match request.format {
        cm_protocol::exchange::Format::Bibtex => "bib",
        cm_protocol::exchange::Format::Ris => "ris",
    };
    if !policy::export::destination_allowed(&request.path, allowed)
        || !policy::export::extension_allowed(&request.path, &[extension])
    {
        return false;
    }
    let ids = |p: &std::path::Path| {
        std::fs::symlink_metadata(p)
            .ok()
            .map(|m| (m.dev(), m.ino()))
    };
    let roots: Vec<(u64, u64)> = allowed.iter().filter_map(|d| ids(d)).collect();
    let mut current = request.path.parent();
    while let Some(dir) = current {
        match ids(dir) {
            Some(id) if roots.contains(&id) => return true,
            Some(_) => current = dir.parent(),
            None => return false,
        }
    }
    false
}

/// The grant a token-management command acts on (recorded in the audit log).
fn api_target(command: &cm_protocol::api::ApiCommand) -> Option<GrantId> {
    use cm_protocol::api::ApiCommand as A;
    match command {
        A::Reissue(g) | A::Revoke(g) | A::EndElevation(g) => Some(*g),
        A::SetScopes { grant, .. } | A::Elevate { grant, .. } => Some(*grant),
        _ => None,
    }
}
/// Ids and fixed codes only: never request content, paths, labels or secrets.
fn audit_entry(
    grant: GrantId,
    kind: Option<policy::ClientKind>,
    operation: Operation,
    result: &Result<Reply, WireError>,
    target: Option<GrantId>,
) -> AuditEntry {
    let target = match result {
        Ok(Reply::Api(cm_protocol::api::ApiReply::Issued { token, .. })) => Some(token.id),
        Ok(Reply::Grants(GrantReply::Issued { grant, .. })) => Some(grant.id),
        _ => target,
    };
    AuditEntry {
        at_ms: now_ms(),
        grant: Some(grant),
        kind,
        operation,
        outcome: match result {
            Ok(_) => AuditOutcome::Succeeded,
            Err(WireError::Denied(_)) => AuditOutcome::Denied,
            Err(_) => AuditOutcome::Failed,
        },
        reason: result
            .as_ref()
            .err()
            .map(|e| crate::api::reason_code(e).to_owned()),
        target,
        count: 1,
    }
}

/// How long a full-text status/page read waits for a free search reader.
const FULLTEXT_READ_WAIT: Duration = Duration::from_secs(5);

/// #27 provenance: `source` decides whether the GUI shows an annotation as an unreviewed AI
/// proposal; `ConfirmAi` is the person's review and `Undo`/`Redo` replay a whole client-supplied
/// row (source and confirmation included). Only the owner (GrantAdmin, i.e. the desktop) may do
/// those; token clients (API/MCP/CLI) create `ai` annotations and edit/delete them.
fn annotation_provenance(
    request: &cm_protocol::annotations::AnnotationRequest,
    held: &CapabilitySet,
) -> Result<(), WireError> {
    use cm_protocol::annotations::{AnnotationRequest as A, Source};
    if held.contains(&Capability::GrantAdmin) {
        return Ok(());
    }
    match request {
        A::Create(new) if new.source != Source::Ai => {
            Err(WireError::Service(ServiceError::InvalidInput(
                "annotations created with a token are always source=ai".into(),
            )))
        }
        A::ConfirmAi(_) | A::Undo(_) | A::Redo(_) => Err(WireError::Denied(Denial::Forbidden(
            Operation::AnnotationWrite,
        ))),
        _ => Ok(()),
    }
}

/// The grant that created a preview (preview binding) and, for a caller that cannot read
/// papers, which rows exactly duplicate existing papers (skipped on its commit).
struct PreviewOwner {
    grant: GrantId,
    hidden_duplicates: Option<Vec<bool>>,
}

/// Same text for every redacted preview: it must not say whether anything matched.
pub const REDACTED_PREVIEW_WARNING: &str = "Existing-library matches are hidden for this \
    client; rows that exactly duplicate an existing paper are skipped on commit.";

/// Without a read scope a preview must not reveal existing papers (native host capture,
/// write-only tokens): matches, the exact-match id, citation-key collision notes and the
/// re-allocated key are removed, and the warning is constant. Returns which rows were exact
/// duplicates of existing papers, decided before redaction.
fn redact_matches(preview: &mut ImportPreview) -> Vec<bool> {
    let mut duplicates = Vec::with_capacity(preview.candidates.len());
    for candidate in &mut preview.candidates {
        duplicates.push(
            candidate.existing_match.is_some() || candidate.matches.iter().any(|m| m.kind.exact()),
        );
        candidate.matches.clear();
        candidate.existing_match = None;
        candidate.draft.bibliography.citation_key = None;
        candidate
            .diagnostics
            .retain(|d| !(d.field == "citation_key" && !d.error));
    }
    preview.warnings.push(REDACTED_PREVIEW_WARNING.into());
    duplicates
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_protocol::{
        annotations::{AnnotationId, AnnotationRequest},
        exchange::{ExportRequest, ExportSelection, Format},
    };

    #[test]
    fn annotation_deletes_count_toward_the_delete_burst() {
        let id = AnnotationId(uuid::Uuid::nil());
        assert_eq!(
            bulk_kind(&Command::Annotations(AnnotationRequest::Delete(id))),
            Some(BulkKind::Delete)
        );
        assert_eq!(
            bulk_kind(&Command::Annotations(AnnotationRequest::ConfirmAi(id))),
            None
        );
        assert_eq!(bulk_kind(&Command::Status), None);
    }

    #[test]
    fn export_containment_uses_the_real_directory_and_the_format_extension() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let allowed = root.join("exports");
        std::fs::create_dir_all(allowed.join("sub")).unwrap();
        let request = |path: PathBuf, format| ExportRequest {
            selection: ExportSelection::All,
            format,
            path,
        };
        let dirs = vec![allowed.clone()];
        assert!(export_allowed(
            &request(allowed.join("sub/a.bib"), Format::Bibtex),
            &dirs
        ));
        assert!(export_allowed(
            &request(allowed.join("a.ris"), Format::Ris),
            &dirs
        ));
        assert!(!export_allowed(
            &request(allowed.join("a.ris"), Format::Bibtex),
            &dirs
        ));
        assert!(!export_allowed(
            &request(allowed.join("a.json"), Format::Bibtex),
            &dirs
        ));
        // A missing parent cannot prove containment; neither can a deleted allowed folder.
        assert!(!export_allowed(
            &request(allowed.join("missing/a.bib"), Format::Bibtex),
            &dirs
        ));
        std::fs::remove_dir_all(&allowed).unwrap();
        std::fs::create_dir(&allowed).unwrap();
        // Recreated at the same path: the folder itself is the allowed root again.
        assert!(export_allowed(
            &request(allowed.join("a.bib"), Format::Bibtex),
            &dirs
        ));
        std::fs::remove_dir(&allowed).unwrap();
        assert!(!export_allowed(
            &request(allowed.join("a.bib"), Format::Bibtex),
            &dirs
        ));
    }
}
