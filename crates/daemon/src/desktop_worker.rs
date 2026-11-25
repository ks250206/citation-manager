//! Library engine. All IO and retained import state stay on workers; #25 serves it over IPC.
mod annotations;
pub(crate) mod files;
mod pdf;
mod settings;
use cm_core::{import::ImportCoordinator, papers::PaperService, ports::ImportService};
use cm_protocol::{
    attachments::*, exchange::*, library::*, library_query::*, organization::*, settings::*,
};
use cm_storage::{LibraryDb, LibraryDirectory};
use std::{collections::HashMap, path::PathBuf, sync::mpsc, thread};
type Work = Box<dyn for<'a> FnOnce(Result<&mut Worker<'a>, ServiceError>) + Send>;
type Resolver = cm_core::metadata_resolve::MetadataResolveService<cm_metadata::doi::HttpProviders>;
/// Work for the network worker: DOI resolution and (#22) capture extraction + complement.
/// The provider service (and its runtime) is created lazily on this thread and only when
/// a lookup is actually needed.
type ResolveWork = Box<dyn FnOnce(&mut Option<Resolver>) + Send>;
fn resolver_slot(slot: &mut Option<Resolver>) -> Result<&mut Resolver, ServiceError> {
    if slot.is_none() {
        let provider = cm_metadata::doi::HttpProviders::new(Default::default())?;
        *slot = Some(cm_core::metadata_resolve::MetadataResolveService::new(
            provider,
        ));
    }
    Ok(slot.as_mut().expect("constructed"))
}
/// Delivers a reply exactly once: `finish` sends the result; if every holder is dropped
/// without finishing (e.g. the work panicked and unwound), the caller gets an error instead
/// of waiting forever.
struct ReplyOnce<T>(std::sync::Mutex<Option<Reply<T>>>);
impl<T> ReplyOnce<T> {
    fn new(reply: Reply<T>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(std::sync::Mutex::new(Some(reply))))
    }
    fn finish(&self, result: Result<T, ServiceError>) {
        let reply = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(reply) = reply {
            reply(result);
        }
    }
    /// Drops the reply without calling it (the caller reports the failure another way).
    fn disarm(&self) {
        drop(self.0.lock().unwrap_or_else(|e| e.into_inner()).take());
    }
}
impl<T> Drop for ReplyOnce<T> {
    fn drop(&mut self) {
        let reply = self.0.get_mut().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(reply) = reply {
            reply(Err(ServiceError::InvalidInput(
                "the request could not be processed (internal error)".into(),
            )));
        }
    }
}
/// Runs network-worker items one at a time. A panicking item (a bug triggered by one
/// request) is contained: its reply resolves to an error through `ReplyOnce`, the provider
/// service is rebuilt on next use, and later requests keep being served.
fn serve_resolver(lookups: mpsc::Receiver<ResolveWork>) {
    let mut service = None;
    while let Ok(work) = lookups.recv() {
        let run = std::panic::AssertUnwindSafe(|| work(&mut service));
        if std::panic::catch_unwind(run).is_err() {
            service = None;
        }
    }
}
pub(super) struct Worker<'a> {
    db: LibraryDb,
    directory: LibraryDirectory,
    settings_path: PathBuf,
    imports: Box<dyn ImportService + 'a>,
    parsed: Option<cm_metadata::exchange::ParseReport>,
    pdfs: PdfReport,
    partial_pdfs: Vec<PdfFile>,
    pdf_preview: Option<(usize, cm_protocol::import::PreviewId)>,
    /// Retained parse/PDF state of sessions other than the one currently executing.
    sessions: HashMap<u64, Retained>,
    /// Confirmation ledger for annotation undo/redo (#19), kept for the worker's lifetime.
    confirmations: cm_core::annotations::Confirmations,
    /// Tests replace the PDF page source of `Propose`.
    #[cfg(test)]
    test_pages: Option<Box<dyn FnMut() -> Box<dyn annotations::PageReader> + Send>>,
}
/// Per-session retained parse/PDF-inspection state (#25): one IPC session can never
/// preview or commit from another session's selected files.
#[derive(Default)]
struct Retained {
    parsed: Option<cm_metadata::exchange::ParseReport>,
    pdfs: PdfReport,
    partial_pdfs: Vec<PdfFile>,
    pdf_preview: Option<(usize, cm_protocol::import::PreviewId)>,
}
impl Worker<'_> {
    fn enter(&mut self, session: u64) {
        let r = self.sessions.remove(&session).unwrap_or_default();
        self.parsed = r.parsed;
        self.pdfs = r.pdfs;
        self.partial_pdfs = r.partial_pdfs;
        self.pdf_preview = r.pdf_preview;
    }
    fn leave(&mut self, session: u64) {
        let r = Retained {
            parsed: self.parsed.take(),
            pdfs: std::mem::take(&mut self.pdfs),
            partial_pdfs: std::mem::take(&mut self.partial_pdfs),
            pdf_preview: self.pdf_preview.take(),
        };
        if r.parsed.is_some()
            || !r.pdfs.files.is_empty()
            || !r.partial_pdfs.is_empty()
            || r.pdf_preview.is_some()
        {
            self.sessions.insert(session, r);
        }
    }
}
/// Result of a capture preview: the common #33 preview plus the capture resolution.
pub type CapturePreviewed = (
    cm_protocol::import::ImportPreview,
    cm_protocol::capture::CaptureResolution,
);
/// Signals that the library worker thread has finished (its database is closed).
pub struct WorkerExit(mpsc::Receiver<()>);
impl WorkerExit {
    /// Waits up to `timeout` for the worker to exit; `true` once it has.
    pub fn wait(&self, timeout: std::time::Duration) -> bool {
        matches!(
            self.0.recv_timeout(timeout),
            Err(mpsc::RecvTimeoutError::Disconnected)
        )
    }
}

/// In-process engine. The GUI no longer embeds it (#25): `papersd` owns it behind IPC,
/// one `for_session` handle per authenticated connection.
#[derive(Clone)]
pub struct InProcessLibraryClient {
    sender: mpsc::SyncSender<Work>,
    resolver: mpsc::SyncSender<ResolveWork>,
    session: u64,
}
impl InProcessLibraryClient {
    pub fn start(path: PathBuf) -> std::io::Result<Self> {
        Self::start_with_settings(path, None)
    }
    /// Only explicit dedicated paths; no default user library/settings or daemon listener.
    pub fn start_with_settings(path: PathBuf, settings: Option<PathBuf>) -> std::io::Result<Self> {
        Self::start_with_exit(path, settings).map(|(client, _)| client)
    }
    /// Like `start_with_settings`, plus a handle that reports when the library worker has
    /// exited and therefore closed its database connections (after the last clone is gone).
    pub fn start_with_exit(
        path: PathBuf,
        settings: Option<PathBuf>,
    ) -> std::io::Result<(Self, WorkerExit)> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(32);
        let (exited, exit) = mpsc::channel::<()>();
        thread::Builder::new()
            .name("library-worker".into())
            .spawn(move || {
                // Dropped when this thread ends, after the worker (and its connections).
                let _exited = exited;
                while let Ok(first) = receiver.recv() {
                    let opened = (|| {
                        let directory = LibraryDirectory::prepare(&path)?;
                        // Upgrades of an existing library take a pre-migration backup first
                        // (new schema from later issues, e.g. #19 annotations).
                        let db = LibraryDb::open_with_safety_backups(&directory)?;
                        let import_db = LibraryDb::open(&directory)?;
                        Ok::<_, cm_storage::StorageError>((directory, db, import_db))
                    })();
                    let (directory, db, mut import_db) = match opened {
                        Ok(value) => value,
                        Err(e) => {
                            first(Err(ServiceError::InvalidInput(format!(
                                "Cannot open library: {e}"
                            ))));
                            continue;
                        }
                    };
                    // Separate owned connections, SAME worker. The retained coordinator's
                    // data_version detects every write through the ordinary services.
                    let mut worker = Worker {
                        settings_path: settings
                            .clone()
                            .unwrap_or_else(|| directory.path().join("ui-settings.json")),
                        db,
                        directory,
                        imports: Box::new(ImportCoordinator::new(import_db.imports())),
                        parsed: None,
                        pdfs: PdfReport::default(),
                        partial_pdfs: Vec::new(),
                        pdf_preview: None,
                        sessions: HashMap::new(),
                        confirmations: Default::default(),
                        #[cfg(test)]
                        test_pages: None,
                    };
                    first(Ok(&mut worker));
                    while let Ok(work) = receiver.recv() {
                        work(Ok(&mut worker));
                    }
                    break;
                }
            })?;
        let (resolver, lookups) = mpsc::sync_channel::<ResolveWork>(8);
        thread::Builder::new()
            .name("doi-worker".into())
            .spawn(move || serve_resolver(lookups))?;
        Ok((
            Self {
                sender,
                resolver,
                session: 0,
            },
            WorkerExit(exit),
        ))
    }
    /// #22 capture preview: extraction and the DOI complement run on the network worker,
    /// then the draft enters the SAME common #33 coordinator as every other import (no
    /// write). `complete` runs exactly once.
    pub fn capture_preview(
        &self,
        request: cm_protocol::capture::CaptureRequest,
        policy: cm_metadata::fetch_policy::FetchPolicy,
        cancel: Cancellation,
        complete: Reply<CapturePreviewed>,
    ) -> Result<(), ServiceError> {
        let client = self.clone();
        let once = ReplyOnce::new(complete);
        let finish = |once: &ReplyOnce<CapturePreviewed>, result| once.finish(result);
        let work_once = once.clone();
        let sent = self.resolver.try_send(Box::new(move |slot| {
            let mut lookup = |doi: &str, cancel: &Cancellation| {
                resolver_slot(slot).and_then(|s| s.resolve(doi, cancel))
            };
            let resolution =
                cm_metadata::capture::resolve_with(&request, &policy, Some(&mut lookup), &cancel);
            let resolution = match resolution {
                Ok(r) if !cancel.is_cancelled() => r,
                Ok(_) => {
                    return finish(
                        &work_once,
                        Err(ServiceError::InvalidInput("cancelled".into())),
                    );
                }
                Err(e) => return finish(&work_once, Err(e)),
            };
            let preview_once = work_once.clone();
            let enqueued = client.enqueue(Box::new(move |r| finish(&preview_once, r)), move |w| {
                let request = resolution.request(request.destination.clone());
                w.imports.preview(request).map(|p| (p, resolution))
            });
            if let Err(e) = enqueued {
                finish(&work_once, Err(e));
            }
        }));
        if sent.is_err() {
            finish(&once, Err(ServiceError::Unavailable));
        }
        Ok(())
    }
    /// Same worker and database, separate retained parse/PDF state.
    pub fn for_session(&self, session: u64) -> Self {
        Self {
            session,
            ..self.clone()
        }
    }
    /// Drops a closed session's retained state. Best effort (queue full = kept until reuse).
    pub fn end_session(&self, session: u64) {
        let _ = self.sender.try_send(Box::new(move |worker| {
            if let Ok(w) = worker {
                w.sessions.remove(&session);
            }
        }));
    }
    fn enqueue<T: Send + 'static>(
        &self,
        complete: Reply<T>,
        f: impl for<'a> FnOnce(&mut Worker<'a>) -> Result<T, ServiceError> + Send + 'static,
    ) -> Result<(), ServiceError> {
        let session = self.session;
        self.sender
            .try_send(Box::new(move |worker| {
                complete(worker.and_then(|w| {
                    w.enter(session);
                    let result = f(w);
                    w.leave(session);
                    result
                }))
            }))
            .map_err(|_| ServiceError::Unavailable)
    }
}
impl LibraryClient for InProcessLibraryClient {
    fn submit(&self, request: LibraryRequest, complete: Completion) -> Result<(), ServiceError> {
        self.enqueue(complete, move |worker| {
            let mut service = PaperService::new(worker.db.papers());
            match request {
                LibraryRequest::List(query) => service.list(&query).map(LibraryReply::Papers),
                LibraryRequest::Get(id) => {
                    service.get(id).map(|p| LibraryReply::Paper(Box::new(p)))
                }
                LibraryRequest::Create(input) => service
                    .create(*input)
                    .map(|p| LibraryReply::Paper(Box::new(p))),
                LibraryRequest::Update(paper) => service
                    .update(&paper)
                    .map(|p| LibraryReply::Paper(Box::new(p))),
            }
        })
    }
    fn query(&self, query: LibraryQuery, complete: Reply<LibraryPage>) -> Result<(), ServiceError> {
        self.enqueue(complete, move |w| {
            LibraryQueryService::new(w.db.library_query()).query(&query)
        })
    }
    fn organization(
        &self,
        request: OrganizationRequest,
        complete: Reply<Organization>,
    ) -> Result<(), ServiceError> {
        self.enqueue(complete, move |w| {
            use cm_core::{collections::CollectionService, tags::TagService};
            match request {
                OrganizationRequest::List => (),
                OrganizationRequest::ForPaper(id) => {
                    return Ok(Organization {
                        collections: CollectionService::new(w.db.collections()).for_paper(id)?,
                        tags: TagService::new(w.db.tags()).for_paper(id)?,
                    });
                }
                OrganizationRequest::CreateCollection(v) => {
                    CollectionService::new(w.db.collections()).create(v)?;
                }
                OrganizationRequest::UpdateCollection(id, v) => {
                    CollectionService::new(w.db.collections()).update(id, v)?;
                }
                OrganizationRequest::DeleteCollection(id) => {
                    CollectionService::new(w.db.collections()).delete(id)?
                }
                OrganizationRequest::CollectionMember(p, c, present) => {
                    let mut s = CollectionService::new(w.db.collections());
                    if present {
                        s.add_paper(p, c)?;
                    } else {
                        s.remove_paper(p, c)?;
                    }
                }
                OrganizationRequest::CreateTag(v) => {
                    TagService::new(w.db.tags()).create(v)?;
                }
                OrganizationRequest::UpdateTag(id, v) => {
                    TagService::new(w.db.tags()).update(id, v)?;
                }
                OrganizationRequest::DeleteTag(id) => TagService::new(w.db.tags()).delete(id)?,
                OrganizationRequest::TagMember(p, t, present) => {
                    let mut s = TagService::new(w.db.tags());
                    if present {
                        s.add_paper(p, t)?;
                    } else {
                        s.remove_paper(p, t)?;
                    }
                }
            }
            Ok(Organization {
                collections: CollectionService::new(w.db.collections()).list()?,
                tags: TagService::new(w.db.tags()).list()?,
            })
        })
    }
    fn lifecycle(
        &self,
        paper: cm_protocol::domain::PaperId,
        deleted: bool,
        complete: Reply<cm_protocol::domain::Paper>,
    ) -> Result<(), ServiceError> {
        self.enqueue(complete, move |w| {
            let mut s = PaperService::new(w.db.papers());
            if deleted {
                s.soft_delete(paper)
            } else {
                s.restore(paper)
            }
        })
    }
    fn annotations(
        &self,
        request: cm_protocol::annotations::AnnotationRequest,
        complete: Reply<cm_protocol::annotations::AnnotationReply>,
    ) -> Result<(), ServiceError> {
        use cm_protocol::annotations::{AnnotationReply, AnnotationRequest};
        if let AnnotationRequest::Propose { id, cancel } = request {
            // Rows are looked up on the library worker; the PDF is verified and searched on a
            // helper thread (as page renders are), so library requests keep flowing.
            return self
                .sender
                .try_send(Box::new(move |worker| {
                    match worker.and_then(|w| w.begin_propose(id, cancel)) {
                        Err(e) => complete(Err(e)),
                        Ok(pending) => {
                            let spawned = thread::Builder::new()
                                .name("annotation-propose".into())
                                .spawn(move || {
                                    complete(pending.finish().map(AnnotationReply::Proposal))
                                });
                            if spawned.is_err() {
                                // `complete` moved into the closure; the caller's lost-reply
                                // guard reports Unavailable when it is dropped.
                            }
                        }
                    }
                }))
                .map_err(|_| ServiceError::Unavailable);
        }
        self.enqueue(complete, move |w| w.annotations(request))
    }
    fn load_settings(&self, complete: Reply<SettingsLoaded>) -> Result<(), ServiceError> {
        self.enqueue(complete, |w| settings::load(&w.settings_path))
    }
    fn save_settings(&self, value: Settings, complete: Reply<()>) -> Result<(), ServiceError> {
        self.enqueue(complete, move |w| settings::save(&w.settings_path, &value))
    }
    fn import(
        &self,
        request: ImportRequestUi,
        complete: Reply<ImportReply>,
    ) -> Result<(), ServiceError> {
        self.enqueue(complete, move |w| w.import(request))
    }
    fn resolve(
        &self,
        doi: String,
        cancel: Cancellation,
        complete: Reply<MetadataResolution>,
    ) -> Result<(), ServiceError> {
        let once = ReplyOnce::new(complete);
        let reply = once.clone();
        let sent = self.resolver.try_send(Box::new(move |slot| {
            reply.finish(resolver_slot(slot).and_then(|s| s.resolve(&doi, &cancel)))
        }));
        if sent.is_err() {
            // Not queued: the caller reports the error itself.
            once.disarm();
            return Err(ServiceError::Unavailable);
        }
        Ok(())
    }
    fn export(
        &self,
        request: ExportRequest,
        complete: Reply<Exported>,
    ) -> Result<(), ServiceError> {
        self.enqueue(complete, move |w| w.export(request))
    }
    fn pdf(
        &self,
        request: PdfRequest,
        progress: PdfProgress,
        complete: Reply<PdfReply>,
    ) -> Result<(), ServiceError> {
        if let PdfRequest::Render(request) = request {
            // Resolve and submit on the library worker; wait for PDFium elsewhere so a slow
            // page never blocks library reads/writes.
            return self
                .sender
                .try_send(Box::new(move |worker| match worker {
                    Err(e) => complete(Err(e)),
                    Ok(worker) => match worker.begin_render(request) {
                        Err(failure) => complete(Ok(PdfReply::Page(Err(failure)))),
                        Ok(pending) => {
                            let spawned = thread::Builder::new()
                                .name("pdf-render-wait".into())
                                .spawn(move || complete(Ok(PdfReply::Page(pending.finish()))));
                            if spawned.is_err() {
                                // `complete` moved into the closure; the caller's lost-reply
                                // guard reports Unavailable when it is dropped.
                            }
                        }
                    },
                }))
                .map_err(|_| ServiceError::Unavailable);
        }
        self.enqueue(complete, move |w| w.pdf(request, progress))
    }
}
impl Worker<'_> {
    fn import(&mut self, request: ImportRequestUi) -> Result<ImportReply, ServiceError> {
        use ImportRequestUi::*;
        match request {
            Parse { path, format } => {
                self.parsed = None;
                let data = files::read_bounded(&path, cm_metadata::exchange::MAX_INPUT_BYTES)?;
                let parsed = match format {
                    Format::Bibtex => cm_metadata::bibtex::parse(&data)?,
                    Format::Ris => cm_metadata::ris::parse(&data)?,
                };
                let reply = ParsedFile {
                    records: parsed.candidates.len(),
                    diagnostics: parsed
                        .diagnostics
                        .iter()
                        .map(|d| FileDiagnostic {
                            line: d.line,
                            message: d.message.clone(),
                            error: d.error,
                        })
                        .collect(),
                    migration_notice: parsed.migration_notice.clone(),
                };
                self.parsed = Some(parsed);
                Ok(ImportReply::Parsed(reply))
            }
            PreviewFile {
                start,
                count,
                destination,
            } => {
                let request = self.parsed.as_ref().ok_or(ServiceError::NotFound)?.batch(
                    start,
                    count,
                    destination,
                )?;
                self.imports.preview(request).map(ImportReply::Preview)
            }
            Preview(request) => self.imports.preview(request).map(ImportReply::Preview),
            Commit(request) => self.imports.commit(request).map(ImportReply::Committed),
            Cancel(id) => self.imports.cancel(&id).map(|()| ImportReply::Cancelled),
        }
    }
    fn export(&self, request: ExportRequest) -> Result<Exported, ServiceError> {
        let service = cm_core::export::ExportService::new(self.db.library_query());
        let report = match request.format {
            Format::Bibtex => {
                service.export(request.selection, &cm_metadata::bibtex::BibtexSerializer)?
            }
            Format::Ris => service.export(request.selection, &cm_metadata::ris::RisSerializer)?,
        };
        files::write_new(&request.path, report.text.as_bytes())?;
        Ok(Exported {
            path: request.path,
            records: report.records,
            diagnostics: report
                .diagnostics
                .into_iter()
                .map(|d| {
                    format!(
                        "{} {}: {}",
                        d.paper.map(|id| id.0.to_string()).unwrap_or_default(),
                        d.field,
                        d.message
                    )
                })
                .collect(),
            migration_notice: report.migration_notice,
        })
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use std::{sync::mpsc::channel, time::Duration};

    /// Regression (#22 review P1): a panic in one network-worker item used to kill the
    /// thread, so that request never got a reply and every later one got `Unavailable`.
    #[test]
    fn a_panicking_item_replies_with_an_error_and_the_worker_keeps_serving() {
        let dir = tempfile::tempdir().unwrap();
        let client = InProcessLibraryClient::start(dir.path().join("library")).unwrap();
        let (tx, rx) = channel();
        let reply = ReplyOnce::<u32>::new(Box::new(move |r| tx.send(r).unwrap()));
        client
            .resolver
            .try_send(Box::new(move |_| {
                let _held = reply;
                panic!("simulated extractor bug");
            }))
            .unwrap();
        let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            matches!(first, Err(ServiceError::InvalidInput(_))),
            "{first:?}"
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "replied twice"
        );
        // The thread survived and serves the next item.
        let (tx, rx) = channel();
        let reply = ReplyOnce::<u32>::new(Box::new(move |r| tx.send(r).unwrap()));
        client
            .resolver
            .try_send(Box::new(move |_| reply.finish(Ok(7))))
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap(),
            7
        );
    }
}
