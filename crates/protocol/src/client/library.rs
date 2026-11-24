//! `LibraryClient` over IPC. Exactly-once completion: whoever removes a pending entry
//! (reader on response/EOF, or the transport thread on a failed write) completes it.
use super::{ClientError, Connector, Stream, handshake};
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
    time::Duration,
};

type RawCompletion = Box<dyn FnOnce(Result<Reply, WireError>) + Send>;
struct Pending {
    complete: RawCompletion,
    progress: Option<PdfProgress>,
    generation: u64,
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
    fn take(&self, id: u64) -> Option<Pending> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
    }
    fn fail_generation(&self, generation: u64) {
        let failed: Vec<Pending> = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
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
    pub fn new(connector: impl Connector) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Outgoing>(64);
        let shared = Arc::new(Shared::default());
        let transport_shared = shared.clone();
        thread::Builder::new()
            .name("ipc-client".into())
            .spawn(move || transport(connector, receiver, transport_shared))?;
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

fn connect(
    connector: &dyn Connector,
    shared: &Arc<Shared>,
    generation: u64,
) -> Result<Connection, ClientError> {
    let (mut stream, hello) = connector.connect()?;
    handshake(stream.as_mut(), hello)?;
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
                        let pending = reader_shared
                            .pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if let Some(progress) = pending.get(&id).and_then(|p| p.progress.as_ref()) {
                            progress(report);
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

fn transport(connector: impl Connector, receiver: mpsc::Receiver<Outgoing>, shared: Arc<Shared>) {
    let mut connection: Option<Connection> = None;
    let mut generation = 0u64;
    let mut cancels: Vec<(u64, Cancellation)> = Vec::new();
    let mut ensure = |connection: &mut Option<Connection>| -> bool {
        if connection
            .as_ref()
            .is_some_and(|c| c.alive.load(Ordering::SeqCst))
        {
            return true;
        }
        if let Some(old) = connection.take() {
            old.writer.shutdown_stream();
        }
        generation += 1;
        match connect(&connector, &shared, generation) {
            Ok(c) => {
                *connection = Some(c);
                *shared.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                true
            }
            Err(e) => {
                *shared.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
                false
            }
        }
    };
    loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(Outgoing::Connect) => {
                ensure(&mut connection);
            }
            Ok(Outgoing::Request {
                id,
                command,
                complete,
                progress,
                cancel,
            }) => {
                if !ensure(&mut connection) {
                    let reason = shared
                        .last_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .unwrap_or_default();
                    complete(Err(if reason.contains("rejected") {
                        WireError::Service(ServiceError::InvalidInput(reason))
                    } else {
                        WireError::Unavailable
                    }));
                    continue;
                }
                let c = connection.as_mut().expect("connected");
                shared
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        id,
                        Pending {
                            complete,
                            progress,
                            generation: c.generation,
                        },
                    );
                let frame = ClientFrame::Request {
                    id,
                    command: *command,
                };
                match write_frame(c.writer.as_mut(), &frame, MAX_REQUEST_FRAME) {
                    Ok(()) if c.alive.load(Ordering::SeqCst) => {
                        if let Some(cancel) = cancel {
                            cancels.push((id, cancel));
                        }
                    }
                    Ok(()) => {
                        if let Some(p) = shared.take(id) {
                            (p.complete)(Err(WireError::Unavailable));
                        }
                    }
                    Err(e) => {
                        let error = match e {
                            crate::ipc::FrameError::TooLarge(_) => WireError::TooLarge,
                            _ => {
                                c.alive.store(false, Ordering::SeqCst);
                                WireError::Unavailable
                            }
                        };
                        if let Some(p) = shared.take(id) {
                            (p.complete)(Err(error));
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !cancels.is_empty() {
            let pending: Vec<u64> = shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .copied()
                .collect();
            cancels.retain(|(id, _)| pending.contains(id));
            let fired: Vec<u64> = cancels
                .iter()
                .filter(|(_, c)| c.is_cancelled())
                .map(|(id, _)| *id)
                .collect();
            cancels.retain(|(_, c)| !c.is_cancelled());
            if let Some(c) = connection.as_mut() {
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
    if let Some(c) = connection {
        c.writer.shutdown_stream();
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
