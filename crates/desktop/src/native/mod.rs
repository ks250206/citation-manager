/// Static binding for the primary modifier: `cmd-` on macOS, `ctrl-` on Linux.
/// (`KeyBinding::new` rejects GPUI's runtime-resolved `secondary-` prefix.)
#[cfg(target_os = "macos")]
macro_rules! primary {
    ($key:literal) => {
        concat!("cmd-", $key)
    };
}
#[cfg(not(target_os = "macos"))]
macro_rules! primary {
    ($key:literal) => {
        concat!("ctrl-", $key)
    };
}
mod annotations_ui;
mod api_ui;
mod details_ui;
mod import_ui;
mod library_ui;
mod menu_ui;
mod palette_ui;
mod pdf_export_ui;
mod pdf_ui;
mod pdf_viewer_ui;
mod search_ui;
mod settings_ui;
mod text_input;
use self::text_input::TextInput;
use crate::{
    attachments::{AttachmentHost, OperationCancels, PdfForm},
    import_dialogs::ReviewSessions,
    library::Library,
    request_policy::{Lane, begin_request, invalidate_lookup},
};
use cm_protocol::{
    attachments::*, domain::*, exchange::*, import::*, library::*, library_query::*,
    organization::*, settings::*,
};
use gpui::{prelude::*, *};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
actions!(
    library,
    [
        New,
        Save,
        Refresh,
        Find,
        NextField,
        PreviousField,
        Quit,
        SelectNext,
        SelectPrevious,
        EditSelected,
        Back,
        ShowLibrary,
        ShowImportReview,
        ShowOrganize,
        OpenPalette,
        PaletteNext,
        PalettePrevious,
        PaletteClose,
        CopySelection,
        AnnotationUndo,
        AnnotationRedo,
        ExtendNext,
        ExtendPrevious,
        SelectAll,
        ShowActions,
        TrashSelected,
        CopyCitation,
        MenuNext,
        MenuPrevious,
        MenuConfirm,
        MenuClose,
        MenuOpenSubmenu,
        MenuCloseSubmenu,
    ]
);
enum Event {
    Query(Result<LibraryPage, ServiceError>),
    Editor(Result<LibraryReply, ServiceError>),
    Loaded(Result<LibraryReply, ServiceError>),
    Organization(Result<Organization, ServiceError>),
    Membership(Result<Organization, ServiceError>),
    Import(Result<ImportReply, ServiceError>),
    Doi(Box<Result<MetadataResolution, ServiceError>>),
    Pdf(Result<PdfReply, ServiceError>),
    Attachments(Result<PdfReply, ServiceError>),
    LoadSettings(Result<SettingsLoaded, ServiceError>),
    SaveSettings(Result<(), ServiceError>),
    Export(Result<Exported, ServiceError>),
    /// #84 several single-paper requests sent one after another (context menu).
    Bulk(Result<crate::bulk::BulkReport, ServiceError>),
    Progress(PdfReport),
    /// Page render replies bypass the single-ticket lanes; the viewer tracks them by key.
    Render(
        crate::pdf_viewer::RenderKey,
        u64,
        Result<PdfReply, ServiceError>,
    ),
    Search(
        Box<search_ui::SearchOp>,
        Box<Result<cm_protocol::search::SearchReply, ServiceError>>,
    ),
    Find(
        Box<search_ui::SearchOp>,
        Box<Result<cm_protocol::search::FulltextReply, ServiceError>>,
    ),
    Annotation(
        Box<annotations_ui::AnnOp>,
        Result<cm_protocol::annotations::AnnotationReply, ServiceError>,
    ),
    /// #26 API settings replies bypass the lanes like annotations.
    Api(
        Box<api_ui::ApiOp>,
        Result<cm_protocol::api::ApiReply, ServiceError>,
    ),
    /// #20 export replies bypass the lanes too (a save never blocks other PDF work).
    PdfExport(
        crate::pdf_export::ExportTarget,
        Result<PdfReply, ServiceError>,
    ),
}
struct Message {
    lane: Lane,
    ticket: u64,
    event: Event,
}
struct LostReply(Option<Box<dyn FnOnce() + Send>>);
impl LostReply {
    fn disarm(&mut self) {
        self.0.take();
    }
}
impl Drop for LostReply {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            send();
        }
    }
}
struct LibraryView {
    client: Arc<dyn LibraryClient>,
    library: Library,
    editor: Option<Paper>,
    draft: PaperDraft,
    fields: Vec<Entity<TextInput>>,
    search: Entity<TextInput>,
    author: Entity<TextInput>,
    year_from: Entity<TextInput>,
    year_to: Entity<TextInput>,
    paper_type: Entity<TextInput>,
    name: Entity<TextInput>,
    doi: Entity<TextInput>,
    focus: FocusHandle,
    table_focus: FocusHandle,
    focus_layout: (Screen, String),
    rows: UniformListScrollHandle,
    sender: mpsc::Sender<Message>,
    receiver: mpsc::Receiver<Message>,
    pdf_progress: Arc<Mutex<Option<(u64, PdfReport)>>>,
    pending: HashMap<Lane, u64>,
    sequence: u64,
    dirty: bool,
    settings: Settings,
    organization: Organization,
    membership: Organization,
    host: AttachmentHost,
    message: String,
    error: bool,
    screen: Screen,
    reviews: ReviewSessions,
    review_destination: String,
    batch_size: usize,
    parsed: Option<ParsedFile>,
    diagnostics: Vec<String>,
    cancels: OperationCancels,
    pdf_form: PdfForm,
    pdf_fields: Vec<Entity<TextInput>>,
    pdf_report: PdfReport,
    pdf_intent: PdfIntent,
    pdf_selection: Option<Selection>,
    pdf_row: usize,
    recursive: bool,
    merge_target: usize,
    merge_fields: Vec<String>,
    export_format: Format,
    export_scope: usize,
    editing_collection: Option<CollectionId>,
    editing_tag: Option<TagId>,
    parent: Option<CollectionId>,
    picker_generation: u64,
    registry: crate::commands::CommandRegistry,
    keymap: crate::keymap::Keymap,
    keymap_custom: bool,
    keymap_diagnostics: Vec<String>,
    keymap_path: Option<String>,
    resolver: crate::keymap::KeyResolver,
    started: std::time::Instant,
    palette_open: bool,
    palette_index: usize,
    palette_query: Entity<TextInput>,
    palette_scroll: ScrollHandle,
    viewer: Option<pdf_viewer_ui::Viewer>,
    ann: annotations_ui::AnnotationState,
    find_search: search_ui::SearchState,
    find: search_ui::FindState,
    pdf_export: pdf_export_ui::PdfExportState,
    /// Page to show once the viewer for that attachment is ready (from the Notes list).
    pending_jump: Option<(AttachmentId, u32)>,
    viewer_focus: FocusHandle,
    page_input: Entity<TextInput>,
    /// #26 API & Integrations screen.
    api: crate::api_settings::ApiSettingsState,
    api_inputs: api_ui::ApiInputs,
    /// #84 open popup menu (row context menu, ＋ Add, ⚙, columns) and its key focus.
    menu: Option<menu_ui::OpenMenu>,
    menu_focus: FocusHandle,
    /// Status verb of the running bulk action ("Moved to Trash:").
    bulk_verb: String,
    /// Whether the bulk action changed the papers themselves (Trash), so the open one is
    /// re-read when it ends.
    bulk_refetch: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Library,
    Import,
    Pdf,
    Settings,
    Report,
    Api,
}
impl LibraryView {
    fn new(client: Arc<dyn LibraryClient>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let fields = crate::details::FIELDS
            .iter()
            .map(|label| cx.new(|cx| TextInput::new(label, cx)))
            .collect();
        let mut view = Self {
            client,
            library: Library::default(),
            editor: None,
            draft: PaperDraft::default(),
            fields,
            search: cx.new(|cx| TextInput::new("Metadata keywords", cx)),
            author: cx.new(|cx| TextInput::new("Author", cx)),
            year_from: cx.new(|cx| TextInput::new("From year", cx)),
            year_to: cx.new(|cx| TextInput::new("To year", cx)),
            paper_type: cx.new(|cx| TextInput::new("Exact type", cx)),
            name: cx.new(|cx| TextInput::new("Collection or tag name", cx)),
            doi: cx.new(|cx| TextInput::new("DOI or doi.org URL", cx)),
            focus: cx.focus_handle(),
            table_focus: cx.focus_handle(),
            focus_layout: (Screen::Library, "Info".into()),
            rows: UniformListScrollHandle::new(),
            sender,
            receiver,
            pdf_progress: Arc::new(Mutex::new(None)),
            pending: HashMap::new(),
            sequence: 0,
            dirty: false,
            settings: Settings::default(),
            organization: Organization::default(),
            membership: Organization::default(),
            host: AttachmentHost::default(),
            message: "Loading library…".into(),
            error: false,
            screen: Screen::Library,
            reviews: ReviewSessions::default(),
            review_destination: String::new(),
            batch_size: 100,
            parsed: None,
            diagnostics: vec![],
            cancels: OperationCancels::default(),
            pdf_form: PdfForm::default(),
            pdf_fields: crate::details::FIELDS
                .iter()
                .map(|label| cx.new(|cx| TextInput::new(label, cx)))
                .collect(),
            pdf_report: PdfReport::default(),
            pdf_intent: PdfIntent::RegisterNew,
            pdf_selection: None,
            pdf_row: 0,
            recursive: false,
            merge_target: 0,
            merge_fields: vec![],
            export_format: Format::Bibtex,
            export_scope: 0,
            editing_collection: None,
            editing_tag: None,
            parent: None,
            picker_generation: 0,
            registry: crate::commands::CommandRegistry::m1(),
            keymap: crate::keymap::Keymap::defaults(&crate::commands::CommandRegistry::m1())
                .expect("built-in keymap is valid"),
            keymap_custom: false,
            keymap_diagnostics: vec![],
            keymap_path: None,
            resolver: crate::keymap::KeyResolver::new(
                crate::keymap::Keymap::defaults(&crate::commands::CommandRegistry::m1())
                    .expect("built-in keymap is valid"),
            ),
            started: std::time::Instant::now(),
            palette_open: false,
            palette_index: 0,
            palette_query: cx.new(|cx| TextInput::new("Type a command…", cx)),
            palette_scroll: ScrollHandle::new(),
            viewer: None,
            ann: annotations_ui::AnnotationState::new(cx),
            find_search: search_ui::SearchState::new(),
            find: search_ui::FindState::new(cx),
            pdf_export: Default::default(),
            pending_jump: None,
            viewer_focus: cx.focus_handle(),
            page_input: cx.new(|cx| TextInput::new("Page", cx)),
            api: Default::default(),
            api_inputs: api_ui::ApiInputs::new(cx),
            menu: None,
            menu_focus: cx.focus_handle(),
            bulk_verb: String::new(),
            bulk_refetch: false,
        };
        view.subscribe_viewer_inputs(window, cx);
        // The resolver belongs to the table focus owner: a prefix never survives focus loss.
        cx.on_blur(&view.table_focus, window, |this, _, _| {
            this.resolver.reset()
        })
        .detach();
        // An edited port is part of the unsaved API settings draft (kept across refreshes).
        cx.subscribe(
            &view.api_inputs.port,
            |this, _, _: &text_input::Edited, cx| {
                this.api.dirty = true;
                cx.notify();
            },
        )
        .detach();
        view.subscribe_annotation_inputs(window, cx);
        view.subscribe_search_inputs(window, cx);
        cx.subscribe(
            &view.palette_query,
            |this, _, _: &text_input::Edited, cx| {
                this.palette_index = 0;
                this.palette_scroll.scroll_to_item(0);
                cx.notify();
            },
        )
        .detach();
        cx.subscribe_in(
            &view.palette_query,
            window,
            |this, _, _: &text_input::Submitted, window, cx| this.palette_confirm(window, cx),
        )
        .detach();
        for field in &view.fields {
            cx.subscribe(field, |this, _, _: &text_input::Edited, cx| {
                this.dirty = true;
                this.message = "Unsaved metadata changes".into();
                this.invalidate_doi();
                cx.notify();
            })
            .detach();
        }
        for field in [
            &view.search,
            &view.author,
            &view.year_from,
            &view.year_to,
            &view.paper_type,
        ] {
            cx.subscribe(field, |this, _, _: &text_input::Submitted, cx| {
                this.search_or_filter(cx)
            })
            .detach();
        }
        cx.subscribe(&view.doi, |this, _, _: &text_input::Submitted, cx| {
            this.lookup_doi(cx)
        })
        .detach();
        cx.subscribe(&view.doi, |this, _, _: &text_input::Edited, cx| {
            this.invalidate_doi();
            cx.notify();
        })
        .detach();
        view.reload(cx);
        view.organize(OrganizationRequest::List, cx);
        view.call(Lane::LoadSettings, Event::LoadSettings, |c, done| {
            c.load_settings(done)
        });
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if view
                    .update(cx, |view, cx| {
                        let progress = view
                            .pdf_progress
                            .lock()
                            .ok()
                            .and_then(|mut slot| slot.take());
                        if let Some((ticket, report)) = progress {
                            view.receive(
                                Message {
                                    lane: Lane::Pdf,
                                    ticket,
                                    event: Event::Progress(report),
                                },
                                cx,
                            );
                        }
                        while let Ok(message) = view.receiver.try_recv() {
                            view.receive(message, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        view
    }
    fn call<T: Send + 'static>(
        &mut self,
        lane: Lane,
        wrap: fn(Result<T, ServiceError>) -> Event,
        submit: impl FnOnce(Arc<dyn LibraryClient>, Reply<T>) -> Result<(), ServiceError>,
    ) {
        let (ticket, cancel_lookup) = begin_request(&mut self.pending, &mut self.sequence, lane);
        if cancel_lookup {
            self.cancels.cancel_doi();
        }
        let lost_sender = self.sender.clone();
        let mut lost = LostReply(Some(Box::new(move || {
            let _ = lost_sender.send(Message {
                lane,
                ticket,
                event: wrap(Err(ServiceError::Unavailable)),
            });
        })));
        let sender = self.sender.clone();
        let done = Box::new(move |result| {
            lost.disarm();
            let _ = sender.send(Message {
                lane,
                ticket,
                event: wrap(result),
            });
        });
        let _ = submit(self.client.clone(), done);
    }
    fn dispatch_pdf(&mut self, request: PdfRequest) {
        let ticket = self.sequence + 1;
        // Coalesce full reports; a slow frame must not queue 1000 growing snapshots.
        let slot = self.pdf_progress.clone();
        let progress = Box::new(move |report| {
            if let Ok(mut latest) = slot.lock() {
                *latest = Some((ticket, report));
            }
        });
        self.call(Lane::Pdf, Event::Pdf, |client, done| {
            client.pdf(request, progress, done)
        });
    }
    fn busy(&self, lane: Lane) -> bool {
        self.pending.contains_key(&lane)
    }
    fn locked(&self) -> bool {
        self.busy(Lane::Editor) || self.busy(Lane::Import) || self.busy(Lane::Pdf)
    }
    fn status(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.message = text.into();
        self.error = error;
        cx.notify();
    }
    fn receive(&mut self, message: Message, cx: &mut Context<Self>) {
        let message = match message.event {
            Event::Render(key, serial, result) => {
                self.render_reply(key, serial, result, cx);
                return;
            }
            Event::Annotation(op, result) => {
                self.annotation_reply(*op, result, cx);
                return;
            }
            Event::Api(op, result) => {
                self.api_reply(*op, result, cx);
                return;
            }
            Event::Search(op, result) => {
                self.search_reply(*op, *result, cx);
                return;
            }
            Event::Find(op, result) => {
                self.find_reply(*op, *result, cx);
                return;
            }
            Event::PdfExport(target, result) => {
                self.pdf_export_reply(target, result, cx);
                return;
            }
            event => Message { event, ..message },
        };
        if let Event::Progress(report) = message.event {
            if self.pending.get(&message.lane) == Some(&message.ticket) {
                self.pdf_report = report;
                cx.notify();
            }
            return;
        }
        if self.pending.get(&message.lane) != Some(&message.ticket) {
            // A stale preview must be released even though it cannot change the editor.
            let id = match message.event {
                Event::Import(Ok(ImportReply::Preview(p)))
                | Event::Pdf(Ok(PdfReply::Preview(p))) => Some(p.id),
                _ => None,
            };
            if let Some(id) = id {
                let _ = self
                    .client
                    .import(ImportRequestUi::Cancel(id), Box::new(|_| {}));
            }
            return;
        }
        self.pending.remove(&message.lane);
        match message.event {
            Event::Query(result) => match result {
                Ok(page) => {
                    self.library.complete(self.library.generation, page);
                    if self.message == "Loading library…" {
                        self.status("Library loaded", false, cx);
                    }
                }
                Err(e) => {
                    self.library.fail(self.library.generation);
                    self.status(
                        format!("Library query failed: {e}. Retry the page or Reload"),
                        true,
                        cx,
                    );
                }
            },
            Event::Editor(result) => match result {
                Ok(LibraryReply::Paper(p)) => {
                    self.set_editor(Some(*p), None, cx);
                    self.status("Saved locally", false, cx);
                    self.reload(cx);
                }
                Ok(_) => self.status("Unexpected editor reply", true, cx),
                Err(e) => self.status(format!("Save/load failed: {e}. Draft retained"), true, cx),
            },
            Event::Loaded(result) => match result {
                Ok(LibraryReply::Paper(p)) => {
                    if self.dirty {
                        self.status(
                            format!("{}; unsaved metadata edits retained. Select the saved paper after saving or discarding these edits", self.message),
                            self.error,
                            cx,
                        );
                    } else {
                        // The editor now shows this paper (search hit, `paper://` link or
                        // import): selection-based actions such as export Selected must
                        // target it, not the row selected before.
                        self.library.select_only(p.id);
                        self.set_editor(Some(*p), None, cx);
                    }
                }
                Ok(_) => {
                    self.find_search.open = None;
                    self.status("Unexpected load reply", true, cx)
                }
                Err(e) => {
                    // A hit/link open that failed must not fire later on another selection.
                    self.find_search.open = None;
                    self.status(
                        format!("{}; paper load failed: {e}", self.message),
                        true,
                        cx,
                    )
                }
            },
            Event::Organization(result) => match result {
                Ok(org) => {
                    self.organization = org;
                    self.reload(cx);
                    self.load_membership();
                }
                Err(e) => self.status(format!("Organization failed: {e}"), true, cx),
            },
            Event::Membership(result) => match result {
                Ok(org) => self.membership = org,
                Err(e) => self.status(format!("Membership read failed: {e}"), true, cx),
            },
            Event::LoadSettings(result) => match result {
                Ok(loaded) => {
                    self.apply_keymap(loaded.keymap.as_deref(), loaded.keymap_path.as_deref());
                    self.settings = loaded.settings;
                    self.library.query.sort = self.settings.sort;
                    self.reload(cx);
                    if let Some(warning) = loaded.warning {
                        self.status(warning, true, cx);
                    } else if !self.keymap_diagnostics.is_empty() {
                        self.status(
                            format!(
                                "keymap.json ignored ({} problem(s)); using built-in keys. Details: ⚙ Settings › Organize & export › Keyboard",
                                self.keymap_diagnostics.len()
                            ),
                            true,
                            cx,
                        );
                    } else if self.keymap_custom {
                        self.status("Custom keymap loaded", false, cx);
                    } else if self.message.starts_with("Reloading") {
                        self.status("Settings reloaded; using the built-in keymap", false, cx);
                    }
                }
                Err(e) => self.status(format!("Using default layout: {e}"), true, cx),
            },
            Event::SaveSettings(result) => match result {
                // Saves are frequent (layout, history); only failures need attention.
                Ok(()) => (),
                Err(e) => self.status(format!("Layout changed but not saved: {e}"), true, cx),
            },
            Event::Import(result) => self.import_reply(result, cx),
            Event::Doi(result) => self.doi_reply(*result, cx),
            Event::Pdf(result) => self.pdf_reply(result, cx),
            Event::Attachments(result) => match result {
                Ok(PdfReply::Attachments(a)) => {
                    if let Some(p) = &self.editor {
                        self.host.set(p.id, a);
                    }
                    self.continue_open(cx);
                }
                Ok(_) => (),
                Err(e) => self.status(format!("Attachment read failed: {e}"), true, cx),
            },
            Event::Bulk(result) => self.bulk_reply(result, cx),
            Event::Export(result) => match result {
                Ok(report) => {
                    self.diagnostics = vec![
                        format!(
                            "Exported {} records to {}",
                            report.records,
                            report.path.display()
                        ),
                        report.migration_notice,
                    ];
                    self.diagnostics.extend(report.diagnostics);
                    self.screen = Screen::Report;
                    self.status("Export completed", false, cx);
                }
                Err(e) => self.status(format!("Export failed: {e}"), true, cx),
            },
            Event::Progress(_)
            | Event::Render(..)
            | Event::Annotation(..)
            | Event::Api(..)
            | Event::Search(..)
            | Event::Find(..)
            | Event::PdfExport(..) => (),
        }
        self.lock_fields(cx);
        cx.notify();
    }
    fn reload(&mut self, cx: &mut Context<Self>) {
        let query = self.library.reload();
        self.call(Lane::Query, Event::Query, |c, done| c.query(query, done));
        // Full-text results follow the sidebar scope (collection / Trash) too.
        if self.find_search.fulltext && self.find_search.request.is_some() {
            self.run_fulltext(cx);
        }
        cx.notify();
    }
    fn organize(&mut self, request: OrganizationRequest, cx: &mut Context<Self>) {
        if self.locked() {
            return;
        }
        self.call(Lane::Organization, Event::Organization, |c, done| {
            c.organization(request, done)
        });
        cx.notify();
    }
    fn load_membership(&mut self) {
        // Membership queries are defined only for active papers. A successful
        // Trash action must not be replaced by an expected ForPaper NotFound.
        if let Some(p) = self.editor.as_ref().filter(|p| p.deleted_at.is_none()) {
            let id = p.id;
            self.call(Lane::Membership, Event::Membership, |c, done| {
                c.organization(OrganizationRequest::ForPaper(id), done)
            });
        }
    }
    fn lock_fields(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.locked();
        for field in self.fields.iter().chain(&self.pdf_fields) {
            field.update(cx, |f, cx| {
                f.enabled = enabled;
                cx.notify();
            });
        }
    }
    fn invalidate_doi(&mut self) -> bool {
        let invalidated = invalidate_lookup(&mut self.pending);
        if invalidated {
            self.cancels.cancel_doi();
        }
        invalidated
    }
    fn can_replace_editor(&mut self, cx: &mut Context<Self>) -> bool {
        if self.dirty {
            self.status(
                "Unsaved metadata edits retained. Save or explicitly discard them before replacing this draft",
                false,
                cx,
            );
            false
        } else {
            true
        }
    }
    fn discard_edits(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            self.status("No unsaved metadata edits to discard", false, cx);
            return;
        }
        if self.locked() {
            self.status(
                "Wait for the current operation before discarding edits",
                true,
                cx,
            );
            return;
        }
        self.set_editor(self.editor.clone(), Some(self.draft.clone()), cx);
        self.screen = Screen::Library;
        self.status(
            "Metadata edits discarded; original form restored",
            false,
            cx,
        );
    }
    fn set_editor(
        &mut self,
        paper: Option<Paper>,
        draft: Option<PaperDraft>,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_doi();
        self.pending.remove(&Lane::Membership);
        self.pending.remove(&Lane::Attachments);
        self.picker_generation += 1;
        self.draft = draft.unwrap_or_else(|| {
            paper
                .as_ref()
                .map(|p| PaperDraft {
                    title: p.title.clone(),
                    external_ids: p.external_ids.clone(),
                    bibliography: p.bibliography.clone(),
                    ..Default::default()
                })
                .unwrap_or_default()
        });
        let values = crate::details::values(&self.draft);
        for (field, value) in self.fields.iter().zip(values) {
            field.update(cx, |f, cx| f.set_text(value, cx));
        }
        self.editor = paper;
        self.dirty = false;
        self.membership = Organization::default();
        self.host = AttachmentHost::default();
        self.load_membership();
        self.reset_annotations();
        if let Some(p) = &self.editor {
            let id = p.id;
            self.call(Lane::Attachments, Event::Attachments, |c, done| {
                c.pdf(PdfRequest::List(id), Box::new(|_| {}), done)
            });
        }
    }
    fn current_draft(&self, cx: &Context<Self>) -> Result<PaperDraft, ServiceError> {
        if self.fields.iter().any(|f| f.read(cx).is_composing()) {
            return Err(ServiceError::InvalidInput(
                "Finish text conversion before saving".into(),
            ));
        }
        crate::details::apply(
            &self.draft,
            &self
                .fields
                .iter()
                .map(|f| f.read(cx).content.to_string())
                .collect::<Vec<_>>(),
        )
    }
    fn bind_destination(&mut self) -> ImportDestination {
        let destination = self.destination();
        self.review_destination = format!(
            "Fixed destination: collection {}, tags {}",
            destination
                .collection
                .and_then(|id| self.organization.collections.iter().find(|c| c.id == id))
                .map(|c| c.name.as_str())
                .unwrap_or("none"),
            self.organization
                .tags
                .iter()
                .filter(|t| destination.tags.contains(&t.id))
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        destination
    }
    fn destination(&self) -> ImportDestination {
        ImportDestination {
            collection: self.library.query.filter.collection,
            tags: self.library.query.filter.tags.clone(),
        }
    }
    fn new_paper(&mut self, _: &New, _: &mut Window, cx: &mut Context<Self>) {
        if self.locked() || !self.can_replace_editor(cx) {
            return;
        }
        if !self.cancel_preview() {
            cx.notify();
            return;
        }
        self.set_editor(None, None, cx);
        self.screen = Screen::Library;
        self.settings.details_tab = "Info".into();
        self.status("New paper — preview before saving", false, cx);
    }
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Library {
            self.status(
                "Use the explicit preview/commit controls for this operation",
                false,
                cx,
            );
            return;
        }
        if self.locked() {
            return;
        }
        let draft = match self.current_draft(cx) {
            Ok(d) => d,
            Err(e) => {
                self.status(e.to_string(), true, cx);
                return;
            }
        };
        if let Some(mut p) = self.editor.clone() {
            p.title = draft.title;
            p.external_ids = draft.external_ids;
            p.bibliography = draft.bibliography;
            self.call(Lane::Editor, Event::Editor, |c, done| {
                c.submit(LibraryRequest::Update(Box::new(p)), done)
            });
            self.lock_fields(cx);
        } else {
            self.start_manual_preview(draft, cx);
        }
    }
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(cx);
    }
    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.resolver.reset();
        if self.screen != Screen::Library {
            self.screen = Screen::Library;
            cx.notify();
        }
        self.focus_layout = (self.screen, self.settings.details_tab.clone());
        self.search.focus_handle(cx).focus(window, cx);
    }
    fn next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_field(false, window, cx);
    }
    fn previous_field(&mut self, _: &PreviousField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_field(true, window, cx);
    }
    fn move_field(&self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let fields = if self.screen == Screen::Pdf {
            &self.pdf_fields
        } else {
            &self.fields
        };
        let Some(index) = fields
            .iter()
            .position(|f| f.focus_handle(cx).is_focused(window))
        else {
            return;
        };
        let next = if back {
            (index + fields.len() - 1) % fields.len()
        } else {
            (index + 1) % fields.len()
        };
        fields[next].focus_handle(cx).focus(window, cx);
    }
    fn select(&mut self, id: PaperId, kind: crate::library::ClickKind, cx: &mut Context<Self>) {
        self.change_selection(cx, |library| library.click(id, kind));
    }
    /// Apply a selection change and show its focused paper in the details pane. With
    /// several rows selected the pane follows the row that was clicked or moved to last.
    fn change_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Library) -> Option<PaperId>,
    ) {
        // Any selection change (mouse, palette, keymap) ends a pending key sequence, so the
        // first `g` of `gg` cannot complete against a paper chosen afterwards.
        self.resolver.reset();
        if self.locked() || !self.can_replace_editor(cx) {
            return;
        }
        if !self.cancel_preview() {
            cx.notify();
            return;
        }
        let shown = change(&mut self.library);
        if let Some(id) = shown
            && self.editor.as_ref().map(|p| p.id) != Some(id)
        {
            self.set_editor(
                self.library.papers.iter().find(|p| p.id == id).cloned(),
                None,
                cx,
            );
        }
        if let Some(index) = shown.and_then(|id| self.library.index_of(id)) {
            self.rows.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        self.screen = Screen::Library;
        cx.notify();
    }
    fn persist(&mut self, cx: &mut Context<Self>) {
        self.pending.remove(&Lane::LoadSettings);
        let settings = self.settings.clone();
        self.call(Lane::SaveSettings, Event::SaveSettings, |c, done| {
            c.save_settings(settings, done)
        });
        cx.notify();
    }
}
// Shared palette. Kept in one place so screens read as one app.
const ACCENT: u32 = 0x3b73d9;
const ACCENT_SOFT: u32 = 0xdde8fb;
const ACCENT_TEXT: u32 = 0x163c80;
const SURFACE: u32 = 0xffffff;
const CANVAS: u32 = 0xf5f6f8;
const SIDEBAR: u32 = 0xeef1f5;
const BORDER: u32 = 0xd5dce5;
const MUTED: u32 = 0x5f6b7c;
const TEXT: u32 = 0x1c2533;
/// Secondary action. Disabled buttons look inert and do not show a pointer.
fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    styled_button(id, label.into(), enabled, false)
}
/// The main action of a screen.
fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    styled_button(id, label.into(), enabled, true)
}
// GPUI accepts one hover/active refinement per element, so pick the variant once.
fn styled_button(
    id: impl Into<ElementId>,
    label: SharedString,
    enabled: bool,
    primary: bool,
) -> Stateful<Div> {
    let (bg, border, text, hover, active) = match (enabled, primary) {
        (false, _) => (0xf2f3f5, 0xe1e5ea, 0x9aa3af, 0, 0),
        (true, false) => (SURFACE, 0xc3cfe0, ACCENT_TEXT, 0xeef3fd, ACCENT_SOFT),
        (true, true) => (ACCENT, ACCENT, SURFACE, 0x2f62c2, 0x2654a8),
    };
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .whitespace_nowrap()
        .border_color(rgb(border))
        .bg(rgb(bg))
        .text_color(rgb(text))
        .when(enabled, |b| {
            b.cursor_pointer()
                .hover(move |s| s.bg(rgb(hover)))
                .active(move |s| s.bg(rgb(active)))
        })
        .child(label)
}
/// Selectable list/tab item: flat until hovered, filled when active.
fn nav_item(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .when(active, |b| {
            b.bg(rgb(ACCENT_SOFT))
                .text_color(rgb(ACCENT_TEXT))
                .font_weight(FontWeight::SEMIBOLD)
        })
        .when(!active, |b| b.hover(|s| s.bg(rgb(0xe2e7ee))))
        .child(label.into())
}
/// Small uppercase group heading.
fn section(text: impl Into<SharedString>) -> Div {
    div()
        .mt_3()
        .mb_1()
        .px_2()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(text.into())
}
/// Shortcut label for the primary modifier: ⌘ on macOS, Ctrl elsewhere.
/// Bindings use GPUI's `secondary-` modifier, which follows the same rule.
fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}
/// Secondary explanatory text.
fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(text.into())
}
fn line(text: impl Into<SharedString>) -> Div {
    div().mb_2().child(text.into())
}
impl LibraryView {
    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }
    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.screen != Screen::Library {
            return;
        }
        if let Some(id) = self.library.adjacent(delta) {
            self.select(id, Default::default(), cx);
        }
    }
    /// Shift+Up/Down: grow or shrink the range from its anchor.
    fn extend_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.screen != Screen::Library {
            return;
        }
        self.change_selection(cx, |library| library.extend(delta));
    }
    /// Enter on the table jumps into the first metadata field for editing.
    fn edit_selected(&mut self, _: &EditSelected, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Library || self.settings.details_tab != "Info" {
            return;
        }
        if let Some(field) = self.fields.first() {
            field.focus_handle(cx).focus(window, cx);
        }
    }
    /// Escape leaves a secondary screen, or returns focus from a field to the table.
    fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        self.resolver.reset();
        if self.screen != Screen::Library && self.screen != Screen::Import {
            self.screen = Screen::Library;
            cx.notify();
        }
        if self.screen == Screen::Library {
            self.list_focus().focus(window, cx);
        }
    }
    fn show(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        match screen {
            Screen::Import => {
                if self.locked() {
                    return;
                }
                if self.invalidate_doi() {
                    self.status(
                        "DOI lookup cancelled; retained import review is unchanged",
                        false,
                        cx,
                    );
                }
            }
            Screen::Library => self.list_focus().focus(window, cx),
            _ => (),
        }
        self.screen = screen;
        cx.notify();
    }
    // Fixed-height bar: dirty/error notices never shift the panes under the pointer.
    fn status_bar(&self, cx: &mut Context<Self>) -> Div {
        let busy = [
            Lane::Query,
            Lane::Editor,
            Lane::Import,
            Lane::Pdf,
            Lane::Doi,
            Lane::Export,
        ]
        .into_iter()
        .any(|lane| self.busy(lane));
        let (bg, fg) = if self.error {
            (0xffe5e5, 0x9b1c1c)
        } else if self.dirty {
            (0xfff4d6, 0x6b4e00)
        } else {
            (SIDEBAR, TEXT)
        };
        div()
            .h(px(34.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .border_t_1()
            .border_color(rgb(BORDER))
            .bg(rgb(bg))
            .text_color(rgb(fg))
            .text_size(px(12.))
            .child(if self.error {
                "⚠"
            } else if busy {
                "⏳"
            } else if self.dirty {
                "✎"
            } else {
                "●"
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(self.message.clone()),
            )
            .when(self.dirty, |el| {
                el.child(div().font_weight(FontWeight::SEMIBOLD).child(
                    if self.screen == Screen::Library {
                        format!("Unsaved edits — {} to save", shortcut("S"))
                    } else {
                        "Unsaved edits — return to Library to save".into()
                    },
                ))
                .child(
                    button("discard-metadata-edits", "Discard edits", !self.locked())
                        .py(px(1.))
                        .on_click(cx.listener(|this, _, _, cx| this.discard_edits(cx))),
                )
            })
            .when(!self.dirty, |el| {
                el.child(
                    div()
                        .text_color(rgb(MUTED))
                        .whitespace_nowrap()
                        .child(format!(
                            "{} commands · {} new · {} search · ↑↓ select (⇧ extends) · ⇧F10 actions · ↩ edit · {} save",
                            shortcut("K"),
                            shortcut("N"),
                            shortcut("F"),
                            shortcut("S")
                        )),
                )
            })
    }
}
impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Switching screen/tab unmounts whatever field had focus; with no focused
        // descendant no "Library" binding (Esc, shortcuts) would fire. Hand focus
        // to the table on Library, else to the root. Find opts out by updating
        // `focus_layout` itself before focusing the search field.
        // A shown token secret never outlives the screen it was shown on.
        if self.screen != Screen::Api && self.api.revealed.is_some() {
            self.api_conceal(cx);
        }
        let layout = (self.screen, self.settings.details_tab.clone());
        if layout != self.focus_layout {
            self.focus_layout = layout;
            let target = if self.screen == Screen::Library {
                self.list_focus()
            } else {
                self.focus.clone()
            };
            window.defer(cx, move |window, cx| target.focus(window, cx));
        }
        // The palette is a sibling of the "Library" key context, so while it has focus no
        // workbench shortcut (⌘N/⌘S/⌘1, Tab, Esc→Back…) fires behind it.
        let workbench = div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .key_context("Library")
            .on_action(cx.listener(Self::new_paper))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::next_field))
            .on_action(cx.listener(Self::previous_field))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| {
                this.run_command(crate::commands::ids::PALETTE, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ShowLibrary, w, cx| this.show(Screen::Library, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowImportReview, w, cx| this.show(Screen::Import, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowOrganize, w, cx| this.show(Screen::Settings, w, cx)),
            )
            .child(self.toolbar(cx))
            .child(match self.screen {
                Screen::Library => self.library_screen(window, cx).into_any_element(),
                Screen::Import => self.import_screen(cx).into_any_element(),
                Screen::Pdf => self.pdf_screen(cx).into_any_element(),
                Screen::Settings => self.settings_screen(cx).into_any_element(),
                Screen::Api => self.api_screen(cx).into_any_element(),
                Screen::Report => div()
                    .id("operation-report")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .child(div().mb_3().child(
                        button("report-back", "← Back to library", true).on_click(
                            cx.listener(|this, _, w, cx| this.show(Screen::Library, w, cx)),
                        ),
                    ))
                    .children(self.diagnostics.iter().cloned().map(line))
                    .into_any_element(),
            })
            .child(self.status_bar(cx));
        let menu = if self.menu.is_some() {
            Some(self.menu_overlay(cx))
        } else {
            None
        };
        div()
            .size_full()
            .relative()
            .bg(rgb(CANVAS))
            .text_color(rgb(TEXT))
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .child(workbench)
            .when(self.palette_open, |el| el.child(self.palette_overlay(cx)))
            .children(menu)
    }
}
pub fn run(client: Arc<dyn LibraryClient>) {
    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys(text_input::key_bindings());
        cx.bind_keys(key_bindings());
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1320.), px(860.)), cx);
        open_main_window(client, bounds, cx);
    });
}
fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new(primary!("n"), New, Some("Library")),
        KeyBinding::new(primary!("s"), Save, Some("Library")),
        KeyBinding::new(primary!("r"), Refresh, Some("Library")),
        KeyBinding::new(primary!("f"), Find, Some("Library")),
        KeyBinding::new(primary!("k"), OpenPalette, Some("Library")),
        KeyBinding::new("down", PaletteNext, Some("Palette")),
        KeyBinding::new("up", PalettePrevious, Some("Palette")),
        KeyBinding::new("escape", PaletteClose, Some("Palette")),
        KeyBinding::new(primary!("c"), CopySelection, Some("PdfViewer")),
        KeyBinding::new(primary!("z"), AnnotationUndo, Some("PdfViewer")),
        KeyBinding::new(primary!("shift-z"), AnnotationRedo, Some("PdfViewer")),
        KeyBinding::new("tab", NextField, Some("Library")),
        KeyBinding::new("shift-tab", PreviousField, Some("Library")),
        KeyBinding::new(primary!("q"), Quit, None),
        KeyBinding::new("escape", Back, Some("Library")),
        KeyBinding::new(primary!("1"), ShowLibrary, Some("Library")),
        KeyBinding::new(primary!("2"), ShowImportReview, Some("Library")),
        KeyBinding::new(primary!(","), ShowOrganize, Some("Library")),
        KeyBinding::new("down", SelectNext, Some("PaperTable")),
        KeyBinding::new("up", SelectPrevious, Some("PaperTable")),
        KeyBinding::new("enter", EditSelected, Some("PaperTable")),
        KeyBinding::new("shift-down", ExtendNext, Some("PaperTable")),
        KeyBinding::new("shift-up", ExtendPrevious, Some("PaperTable")),
        KeyBinding::new(primary!("a"), SelectAll, Some("PaperTable")),
        KeyBinding::new("shift-f10", ShowActions, Some("PaperTable")),
        KeyBinding::new("menu", ShowActions, Some("PaperTable")),
        KeyBinding::new("delete", TrashSelected, Some("PaperTable")),
        KeyBinding::new(primary!("backspace"), TrashSelected, Some("PaperTable")),
        KeyBinding::new(primary!("shift-c"), CopyCitation, Some("PaperTable")),
        KeyBinding::new("down", MenuNext, Some("Menu")),
        KeyBinding::new("up", MenuPrevious, Some("Menu")),
        KeyBinding::new("tab", MenuNext, Some("Menu")),
        KeyBinding::new("shift-tab", MenuPrevious, Some("Menu")),
        KeyBinding::new("right", MenuOpenSubmenu, Some("Menu")),
        KeyBinding::new("left", MenuCloseSubmenu, Some("Menu")),
        KeyBinding::new("enter", MenuConfirm, Some("Menu")),
        KeyBinding::new("space", MenuConfirm, Some("Menu")),
        KeyBinding::new("escape", MenuClose, Some("Menu")),
    ]
}
fn open_main_window(client: Arc<dyn LibraryClient>, bounds: Bounds<Pixels>, cx: &mut App) {
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(1000.), px(680.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Citation Manager — private local library".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| LibraryView::new(client, window, cx)),
        )
        .expect("open native window");
    window
        .update(cx, |view, window, cx| {
            view.table_focus.focus(window, cx);
            cx.activate(true);
        })
        .expect("activate native window");
}
#[cfg(test)]
mod tests {
    // KeyBinding::new panics on an unparsable keystroke only at app start;
    // build every binding here so the default test lane catches it.
    #[test]
    fn every_key_binding_parses_on_this_platform() {
        let library = super::key_bindings();
        let text = super::text_input::key_bindings();
        assert!(library.len() > 10 && text.len() > 10);
        let primary = if cfg!(target_os = "macos") {
            "cmd-n"
        } else {
            "ctrl-n"
        };
        assert_eq!(primary!("n"), primary);
    }
}
