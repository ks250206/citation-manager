mod details_ui;
mod import_ui;
mod library_ui;
mod pdf_ui;
mod settings_ui;
mod text_input;
use self::text_input::TextInput;
use crate::{
    attachments::{AttachmentHost, OperationCancels, PdfForm},
    import_dialogs::ReviewSessions,
    library::Library,
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
    [New, Save, Refresh, Find, NextField, PreviousField, Quit]
);
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lane {
    Query,
    Editor,
    Organization,
    Membership,
    Import,
    Doi,
    Pdf,
    Attachments,
    LoadSettings,
    SaveSettings,
    Export,
}
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
    Progress(PdfReport),
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
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Library,
    Import,
    Pdf,
    Settings,
    Report,
}
impl LibraryView {
    fn new(client: Arc<dyn LibraryClient>, cx: &mut Context<Self>) -> Self {
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
        };
        for field in &view.fields {
            cx.subscribe(field, |this, _, _: &text_input::Edited, cx| {
                this.dirty = true;
                this.message = "Unsaved metadata changes".into();
                this.invalidate_doi();
                cx.notify();
            })
            .detach();
        }
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
        self.sequence += 1;
        let ticket = self.sequence;
        self.pending.insert(lane, ticket);
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
                        self.set_editor(Some(*p), None, cx);
                    }
                }
                Ok(_) => self.status("Unexpected load reply", true, cx),
                Err(e) => self.status(
                    format!("{}; paper load failed: {e}", self.message),
                    true,
                    cx,
                ),
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
                    self.settings = loaded.settings;
                    self.library.query.sort = self.settings.sort;
                    self.reload(cx);
                    if let Some(warning) = loaded.warning {
                        self.status(warning, true, cx);
                    }
                }
                Err(e) => self.status(format!("Using default layout: {e}"), true, cx),
            },
            Event::SaveSettings(result) => match result {
                Ok(()) => self.status("Layout saved", false, cx),
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
                }
                Ok(_) => (),
                Err(e) => self.status(format!("Attachment read failed: {e}"), true, cx),
            },
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
            Event::Progress(_) => (),
        }
        self.lock_fields(cx);
        cx.notify();
    }
    fn reload(&mut self, cx: &mut Context<Self>) {
        let query = self.library.reload();
        self.call(Lane::Query, Event::Query, |c, done| c.query(query, done));
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
        if let Some(p) = &self.editor {
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
    fn invalidate_doi(&mut self) {
        if self.pending.remove(&Lane::Doi).is_some() {
            self.cancels.cancel_doi();
        }
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
        if self.locked() || !self.dirty {
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
    fn select(&mut self, id: PaperId, multiple: bool, cx: &mut Context<Self>) {
        if self.locked() || !self.can_replace_editor(cx) {
            return;
        }
        if !self.cancel_preview() {
            cx.notify();
            return;
        }
        self.library.select(id, multiple);
        self.set_editor(
            self.library.papers.iter().find(|p| p.id == id).cloned(),
            None,
            cx,
        );
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
fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(if enabled { 0xe0eaff } else { 0xeeeeee }))
        .text_color(rgb(if enabled { 0x163c80 } else { 0x777777 }))
        .cursor_pointer()
        .child(label.into())
}
fn line(text: impl Into<SharedString>) -> Div {
    div().mb_2().child(text.into())
}
impl Render for LibraryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xf5f6f8))
            .text_color(rgb(0x1c2533))
            .font_family(".AppleSystemUIFont")
            .text_size(px(14.))
            .track_focus(&self.focus)
            .key_context("Library")
            .on_action(cx.listener(Self::new_paper))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::next_field))
            .on_action(cx.listener(Self::previous_field))
            .child(self.toolbar(cx))
            .when(self.dirty, |el| {
                el.child(
                    div()
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child("Unsaved metadata edits — return to Library to save")
                        .child(
                            button(
                                "discard-metadata-edits",
                                "Discard metadata edits",
                                !self.locked(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.discard_edits(cx))),
                        ),
                )
            })
            .child(
                div()
                    .px_4()
                    .py_2()
                    .bg(rgb(if self.error { 0xffe5e5 } else { 0xe8eff9 }))
                    .child(self.message.clone()),
            )
            .child(match self.screen {
                Screen::Library => self.library_screen(cx).into_any_element(),
                Screen::Import => self.import_screen(cx).into_any_element(),
                Screen::Pdf => self.pdf_screen(cx).into_any_element(),
                Screen::Settings => self.settings_screen(cx).into_any_element(),
                Screen::Report => div()
                    .id("operation-report")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .children(self.diagnostics.iter().cloned().map(line))
                    .into_any_element(),
            })
    }
}
pub fn run(client: Arc<dyn LibraryClient>) {
    gpui_platform::application().run(move |cx: &mut App| {
        text_input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-n", New, Some("Library")),
            KeyBinding::new("cmd-s", Save, Some("Library")),
            KeyBinding::new("cmd-r", Refresh, Some("Library")),
            KeyBinding::new("cmd-f", Find, Some("Library")),
            KeyBinding::new("tab", NextField, Some("Library")),
            KeyBinding::new("shift-tab", PreviousField, Some("Library")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1320.), px(860.)), cx);
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
                |_, cx| cx.new(|cx| LibraryView::new(client, cx)),
            )
            .expect("open native window");
        window
            .update(cx, |view, window, cx| {
                view.focus.focus(window, cx);
                cx.activate(true);
            })
            .expect("activate native window");
    });
}
