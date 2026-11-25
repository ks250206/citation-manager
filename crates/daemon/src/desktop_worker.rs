//! Local in-process composition. All IO and retained import state stay on workers.
mod files;
mod pdf;
mod settings;
use cm_core::{import::ImportCoordinator, papers::PaperService, ports::ImportService};
use cm_protocol::{
    attachments::*, exchange::*, library::*, library_query::*, organization::*, settings::*,
};
use cm_storage::{LibraryDb, LibraryDirectory};
use std::{path::PathBuf, sync::mpsc, thread};
type Work = Box<dyn for<'a> FnOnce(Result<&mut Worker<'a>, ServiceError>) + Send>;
type ResolveWork = (String, Cancellation, Reply<MetadataResolution>);
pub(super) struct Worker<'a> {
    db: LibraryDb,
    directory: LibraryDirectory,
    settings_path: PathBuf,
    imports: Box<dyn ImportService + 'a>,
    parsed: Option<cm_metadata::exchange::ParseReport>,
    pdfs: PdfReport,
    partial_pdfs: Vec<PdfFile>,
    pdf_preview: Option<(usize, cm_protocol::import::PreviewId)>,
    #[cfg(feature = "pdf-metadata")]
    pdf: Option<Result<cm_pdf::worker::PdfWorker, String>>,
}
#[derive(Clone)]
pub struct InProcessLibraryClient {
    sender: mpsc::SyncSender<Work>,
    resolver: mpsc::SyncSender<ResolveWork>,
}
impl InProcessLibraryClient {
    pub fn start(path: PathBuf) -> std::io::Result<Self> {
        Self::start_with_settings(path, None)
    }
    /// Only explicit dedicated paths; no default user library/settings or daemon listener.
    pub fn start_with_settings(path: PathBuf, settings: Option<PathBuf>) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(32);
        thread::Builder::new()
            .name("library-worker".into())
            .spawn(move || {
                while let Ok(first) = receiver.recv() {
                    let opened = (|| {
                        let directory = LibraryDirectory::prepare(&path)?;
                        let db = LibraryDb::open(&directory)?;
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
                        #[cfg(feature = "pdf-metadata")]
                        pdf: None,
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
            .spawn(move || {
                let mut service = None;
                while let Ok((doi, cancel, complete)) = lookups.recv() {
                    if service.is_none() {
                        match cm_metadata::doi::HttpProviders::new(Default::default()) {
                            Ok(provider) => {
                                service =
                                    Some(cm_core::metadata_resolve::MetadataResolveService::new(
                                        provider,
                                    ))
                            }
                            Err(e) => {
                                complete(Err(e));
                                continue;
                            }
                        }
                    }
                    complete(
                        service
                            .as_mut()
                            .expect("constructed")
                            .resolve(&doi, &cancel),
                    );
                }
            })?;
        Ok(Self { sender, resolver })
    }
    fn enqueue<T: Send + 'static>(
        &self,
        complete: Reply<T>,
        f: impl for<'a> FnOnce(&mut Worker<'a>) -> Result<T, ServiceError> + Send + 'static,
    ) -> Result<(), ServiceError> {
        self.sender
            .try_send(Box::new(move |worker| complete(worker.and_then(f))))
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
        self.resolver
            .try_send((doi, cancel, complete))
            .map_err(|_| ServiceError::Unavailable)
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
