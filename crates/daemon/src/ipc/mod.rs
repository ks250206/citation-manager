//! `papersd` local IPC server (#25): the single writer for one library.
//!
//! Start order: private dirs -> exclusive library lock -> migrations (with pre-migration
//! backup) -> grant registry + owner bootstrap credential -> library worker + job runner ->
//! stale socket cleanup -> bind (0600 socket in a 0700 directory). No HTTP listener.
//!
//! Per connection: OS peer check, a bounded/timed Hello, credential -> stored grant, then
//! every request is authorized against the *current* grant (default deny).
pub mod auth;

use crate::{
    desktop_worker::InProcessLibraryClient,
    jobs::{JobEngine, JobHandler, JobPolicy, default_handlers},
    lifecycle::{self, Activity, DaemonPaths, LibraryLock},
};
use auth::{Authenticator, IssueError, now_ms};
use cm_core::{
    ServiceError,
    metadata_resolve::Cancellation,
    policy::{self, Capability, CapabilitySet, Denial, GrantId, Operation},
};
use cm_protocol::{
    attachments::PdfReply,
    exchange::{ImportReply, ImportRequestUi},
    import::{ImportPreview, PreviewId},
    ipc::{
        ClientFrame, Command, DaemonStatus, GrantCommand, GrantReply, HandshakeRejection,
        MAX_HANDSHAKE_FRAME, MAX_IN_FLIGHT, MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME,
        PROTOCOL_VERSION, PdfCommand, Reply, ServerFrame, Welcome, WireError, read_frame,
        write_frame,
    },
    jobs::{JobCommand, JobReply},
    library::LibraryClient,
};
use cm_storage::{LibraryDb, LibraryDirectory, StorageError};
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
    time::Duration,
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
    pub handlers: Vec<Arc<dyn JobHandler>>,
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
    activity: Arc<Activity>,
    sessions: AtomicU64,
    /// Import preview handle -> grant that created it (preview grant binding).
    previews: Mutex<HashMap<PreviewId, GrantId>>,
    /// Live authenticated connections, closed on explicit shutdown.
    streams: Mutex<HashMap<u64, UnixStream>>,
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
    // The lock holder is the only migration runner. Upgrades take a pre-migration backup.
    let control = LibraryDb::open_with_safety_backups(&directory).map_err(StartError::Storage)?;
    let control = Arc::new(Mutex::new(control));
    let auth = Arc::new(Authenticator::load(control.clone()).map_err(StartError::Service)?);
    auth.bootstrap_desktop(&paths.desktop_credential, now_ms())?;
    let activity = Arc::new(Activity::default());
    let engine =
        InProcessLibraryClient::start_with_settings(paths.library.clone(), config.settings)?;
    // The live grant registry decides whether a job's owner may still run it.
    let jobs = JobEngine::start_with_owners(
        directory,
        control,
        config.handlers,
        config.jobs,
        activity.clone(),
        auth.clone(),
    )
    .map_err(StartError::Service)?;
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
        activity,
        sessions: AtomicU64::new(0),
        previews: Mutex::new(HashMap::new()),
        streams: Mutex::new(HashMap::new()),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let accept_stop = stop.clone();
    let idle = config.idle_timeout;
    let thread = thread::Builder::new()
        .name("papersd-accept".into())
        .spawn(move || {
            accept_loop(&listener, &shared, &accept_stop, idle);
            // Stop accepting first, then remove our socket while still holding the lock.
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
            drop(shared);
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
        capabilities: policy::effective(&grant, hello.requested.as_ref()),
        max_request_frame: MAX_REQUEST_FRAME,
        max_in_flight: MAX_IN_FLIGHT as u32,
    };
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let (tx, rx) = mpsc::channel::<ServerFrame>();
    let writer_thread = thread::Builder::new()
        .name("papersd-writer".into())
        .spawn(move || {
            for frame in rx {
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
                if result.is_err() {
                    let _ = writer.shutdown(std::net::Shutdown::Both);
                    break;
                }
            }
        });
    if writer_thread.is_err() || tx.send(ServerFrame::Welcome(welcome)).is_err() {
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
        tx,
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

/// Sends exactly one response for a request id and releases its in-flight slot.
type Pending = (u64, mpsc::Sender<ServerFrame>, Arc<AtomicUsize>);
#[derive(Clone)]
struct Responder(Arc<Mutex<Option<Pending>>>);
impl Responder {
    fn send(&self, result: Result<Reply, WireError>) {
        if let Some((id, tx, in_flight)) = self.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
            in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(ServerFrame::Response { id, result });
        }
    }
}

struct Session {
    id: u64,
    grant: GrantId,
    requested: Option<CapabilitySet>,
    engine: InProcessLibraryClient,
    shared: Arc<Shared>,
    tx: mpsc::Sender<ServerFrame>,
    in_flight: Arc<AtomicUsize>,
    cancels: Arc<Mutex<HashMap<u64, Cancellation>>>,
}

impl Session {
    fn respond_now(&self, id: u64, result: Result<Reply, WireError>) {
        let _ = self.tx.send(ServerFrame::Response { id, result });
    }
    fn preview_owned(&self, preview: &PreviewId) -> bool {
        self.shared
            .previews
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(preview)
            == Some(&self.grant)
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
        let held = match self.shared.auth.authorize(
            self.grant,
            self.requested.as_ref(),
            operation,
            now_ms(),
        ) {
            Ok(held) => held,
            Err(denial) => {
                self.respond_now(id, Err(WireError::Denied(denial)));
                return;
            }
        };
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let responder = Responder(Arc::new(Mutex::new(Some((
            id,
            self.tx.clone(),
            self.in_flight.clone(),
        )))));
        let cancels = self.cancels.clone();
        let done = {
            let responder = responder.clone();
            move |result: Result<Reply, WireError>| {
                cancels
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
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
            Command::Import(request) => {
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
                let redact = !held.contains(&Capability::LibraryRead);
                self.engine.import(
                    request,
                    Box::new(move |r| {
                        done(
                            r.map(|reply| match reply {
                                ImportReply::Preview(mut p) => {
                                    previews
                                        .previews
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .insert(p.id.clone(), grant);
                                    if redact {
                                        redact_matches(&mut p);
                                    }
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
            Command::Pdf(command) => {
                if let PdfCommand::CommitNew { commit, .. } = &command
                    && !self.preview_owned(&commit.preview_id)
                {
                    done(denied(Operation::PdfCommitNew));
                    return;
                }
                let tx = self.tx.clone();
                let previews = self.shared.clone();
                let grant = self.grant;
                let redact = !held.contains(&Capability::LibraryRead);
                self.engine.pdf(
                    command.into_request(self.cancellation(id)),
                    Box::new(move |report| {
                        let _ = tx.send(ServerFrame::PdfProgress { id, report });
                    }),
                    Box::new(move |mut r| {
                        if let Ok(PdfReply::Preview(p)) = &mut r {
                            previews
                                .previews
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(p.id.clone(), grant);
                            // Same rule as import previews: no LibraryRead, no existing matches.
                            if redact {
                                redact_matches(p);
                            }
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
                        let revoked = auth.revoke(grant, now_ms());
                        // Fail closed: the grant is revoked in memory even if persisting failed,
                        // and its jobs must not keep running with its authority.
                        let jobs = match revoked {
                            Ok(false) => Ok(0),
                            Ok(true) | Err(_) => self.shared.jobs.owner_revoked(grant),
                        };
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

/// Without LibraryRead a preview must not reveal existing papers (native host capture).
fn redact_matches(preview: &mut ImportPreview) {
    let mut hidden = 0;
    for candidate in &mut preview.candidates {
        hidden += candidate.matches.len();
        candidate.matches.clear();
        candidate.existing_match = None;
    }
    if hidden > 0 {
        preview.warnings.push(format!(
            "{hidden} possible existing-library matches hidden for this client"
        ));
    }
}
