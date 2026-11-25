//! #19 highlights, comments and notes: requests, undo/redo history and the Notes tab.
use super::*;
use crate::annotations::{PaperHistory, PendingSelection, Step, StepOutcome, pending_selection};
use cm_protocol::annotations::{
    Anchor, Annotation, AnnotationId, AnnotationKind, AnnotationPatch, AnnotationReply,
    AnnotationRequest, COLORS, Edit, NewAnnotation, Proposal, Source, pending_carry_over,
};
use cm_protocol::attachments::Cancellation;

/// What to do with a reply; replies bypass the single-ticket lanes like page renders.
/// Every op names the paper it was sent for: a reply that arrives after the person switched
/// papers must not touch the new paper's history, selection or proposal.
pub(super) enum AnnOp {
    List(PaperId),
    /// A new user edit: record it for undo, then refresh.
    Record(PaperId),
    RecordMany(PaperId),
    Undo(PaperId, Box<Edit>),
    Redo(PaperId, Box<Edit>),
    /// Serial of the search, so a cancelled search's late reply is ignored.
    Propose(PaperId, AnnotationId, u64),
}

pub(super) struct AnnotationState {
    pub(super) list: Vec<Annotation>,
    /// Open paper and its undo/redo history (one step in flight at a time).
    pub(super) history: PaperHistory,
    pub(super) selected: Option<AnnotationId>,
    pub(super) color: &'static str,
    pub(super) comment: Entity<TextInput>,
    pub(super) note: Entity<TextInput>,
    pub(super) note_open: bool,
    pub(super) proposal: Option<(AnnotationId, Option<Proposal>)>,
    /// The running position search (serial, cancellation).
    pub(super) search: Option<(u64, Cancellation)>,
    search_serial: u64,
    /// After an undo/redo, the next list refresh also refreshes the comment box.
    refresh_comment: bool,
    /// The open paper's list has arrived at least once (cleared when the paper changes).
    loaded: bool,
    /// Annotation a search hit or deep link asked for, selected once the list is loaded.
    pending: Option<AnnotationId>,
}
impl AnnotationState {
    pub(super) fn new(cx: &mut Context<LibraryView>) -> Self {
        Self {
            list: vec![],
            history: PaperHistory::default(),
            selected: None,
            color: "yellow",
            comment: cx.new(|cx| TextInput::new("Comment", cx)),
            note: cx.new(|cx| TextInput::new("Note text", cx)),
            note_open: false,
            proposal: None,
            search: None,
            search_serial: 0,
            refresh_comment: false,
            loaded: false,
            pending: None,
        }
    }
    fn cancel_search(&mut self) {
        if let Some((_, cancel)) = self.search.take() {
            cancel.cancel();
        }
    }
    pub(super) fn for_attachment(&self, id: AttachmentId) -> impl Iterator<Item = &Annotation> {
        self.list.iter().filter(move |a| a.attachment_id == id)
    }
    pub(super) fn selected(&self) -> Option<&Annotation> {
        self.selected
            .and_then(|id| self.list.iter().find(|a| a.id == id))
    }
}

pub(super) fn page_of(a: &Annotation) -> Option<u32> {
    match a.anchor {
        Anchor::Resolved { page_index, .. } => Some(page_index),
        Anchor::Unresolved { .. } => None,
    }
}

impl LibraryView {
    /// Returns false when the request could not be queued (its reply will never arrive).
    pub(super) fn ann_send(&mut self, request: AnnotationRequest, op: AnnOp) -> bool {
        let sender = self.sender.clone();
        let sent = self.client.annotations(
            request,
            Box::new(move |result| {
                let _ = sender.send(Message {
                    lane: Lane::Pdf,
                    ticket: 0,
                    event: Event::Annotation(Box::new(op), result),
                });
            }),
        );
        if sent.is_err() {
            self.message = "Annotation service unavailable".into();
            self.error = true;
        }
        sent.is_ok()
    }
    /// Called whenever the open paper changes.
    pub(super) fn reset_annotations(&mut self) {
        let paper = self.editor.as_ref().map(|p| p.id);
        if self.ann.history.open(paper) {
            self.ann.refresh_comment = false;
            self.ann.loaded = false;
            self.ann.pending = None;
            self.ann.selected = None;
            self.ann.proposal = None;
            self.ann.cancel_search();
            self.ann.list.clear();
        }
        if let Some(id) = paper {
            self.ann_send(AnnotationRequest::ForPaper(id), AnnOp::List(id));
        }
    }
    fn reload_annotations(&mut self) {
        if let Some(id) = self.ann.history.paper() {
            self.ann_send(AnnotationRequest::ForPaper(id), AnnOp::List(id));
        }
    }
    pub(super) fn annotation_reply(
        &mut self,
        op: AnnOp,
        result: Result<AnnotationReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        let current = self.ann.history.paper();
        let mine = |paper: PaperId| current == Some(paper);
        match (op, result) {
            (AnnOp::List(paper), Ok(AnnotationReply::List(list))) => {
                if mine(paper) {
                    self.ann.list = list;
                    if self.ann.selected().is_none() {
                        self.ann.selected = None;
                    }
                    if std::mem::take(&mut self.ann.refresh_comment) {
                        let text = self
                            .ann
                            .selected()
                            .and_then(|a| a.comment.clone())
                            .unwrap_or_default();
                        self.ann.comment.update(cx, |f, cx| f.set_text(text, cx));
                    }
                    self.ann.loaded = true;
                    self.ann_resolve_pending(cx);
                }
            }
            // Late replies for another paper: the change is stored, but it is not part of
            // this paper's history.
            (AnnOp::Record(paper) | AnnOp::RecordMany(paper) | AnnOp::Propose(paper, ..), _)
                if !mine(paper) => {}
            (AnnOp::Record(paper), Ok(AnnotationReply::Edit(edit))) => {
                if let Edit::Created(a) = edit.as_ref() {
                    self.ann.pending = None;
                    self.ann.selected = Some(a.id);
                    let text = a.comment.clone().unwrap_or_default();
                    self.ann.comment.update(cx, |f, cx| f.set_text(text, cx));
                }
                self.ann.history.record(paper, *edit);
                self.reload_annotations();
            }
            (AnnOp::RecordMany(paper), Ok(AnnotationReply::Edits(edits))) => {
                let count = edits.len();
                for edit in edits {
                    self.ann.history.record(paper, edit);
                }
                self.status(
                    if count == 0 {
                        "Nothing left to copy: those annotations are already on this PDF".into()
                    } else {
                        format!("Copied {count} annotation(s) as unresolved; place each from Notes")
                    },
                    false,
                    cx,
                );
                self.reload_annotations();
            }
            (AnnOp::Undo(paper, edit), result) => {
                self.history_reply(Step::Undo, paper, *edit, result.map(|_| ()), cx)
            }
            (AnnOp::Redo(paper, edit), result) => {
                self.history_reply(Step::Redo, paper, *edit, result.map(|_| ()), cx)
            }
            (AnnOp::Propose(_, id, serial), result) => {
                if self.ann.search.as_ref().is_some_and(|(s, _)| *s == serial) {
                    self.ann.search = None;
                    match result {
                        Ok(AnnotationReply::Proposal(proposal)) => {
                            self.ann.proposal = Some((id, proposal));
                            self.status("Search finished", false, cx);
                        }
                        Ok(_) => {}
                        Err(e) => self.status(format!("Search failed: {e}"), true, cx),
                    }
                }
            }
            (_, Err(e)) => {
                self.status(format!("Annotation change failed: {e}"), true, cx);
                self.reload_annotations();
            }
            _ => {}
        }
        cx.notify();
    }
    fn history_reply(
        &mut self,
        step: Step,
        paper: PaperId,
        edit: Edit,
        result: Result<(), ServiceError>,
        cx: &mut Context<Self>,
    ) {
        let error = result.as_ref().err().map(ToString::to_string);
        let (done, verb) = match step {
            Step::Undo => ("Undone", "undo"),
            Step::Redo => ("Redone", "redo"),
        };
        match self.ann.history.finish(step, paper, edit, result) {
            StepOutcome::Ignored => return,
            StepOutcome::Applied => {
                self.ann.refresh_comment = true;
                self.status(done, false, cx);
            }
            StepOutcome::Skipped => self.status(
                format!("This annotation changed since; {verb} history skipped that step"),
                true,
                cx,
            ),
            StepOutcome::Retained => self.status(
                format!(
                    "Could not {verb} ({}); the step is kept, try again",
                    error.unwrap_or_default()
                ),
                true,
                cx,
            ),
        }
        self.reload_annotations();
    }
    /// An edit to record in the open paper's history (no paper open: nothing to edit).
    pub(super) fn ann_record(&mut self, request: AnnotationRequest) {
        if let Some(paper) = self.ann.history.paper() {
            self.ann_send(request, AnnOp::Record(paper));
        }
    }
    pub(super) fn ann_create(&mut self, input: NewAnnotation) {
        self.ann_record(AnnotationRequest::Create(Box::new(input)));
    }
    pub(super) fn ann_update(&mut self, id: AnnotationId, patch: AnnotationPatch) {
        self.ann_record(AnnotationRequest::Update(id, patch));
    }
    pub(super) fn ann_delete(&mut self, id: AnnotationId) {
        if self.ann.selected == Some(id) {
            self.ann.selected = None;
        }
        self.ann_record(AnnotationRequest::Delete(id));
    }
    pub(super) fn ann_can_undo(&self) -> bool {
        self.ann.history.can_undo()
    }
    pub(super) fn ann_can_redo(&self) -> bool {
        self.ann.history.can_redo()
    }
    pub(super) fn ann_undo(&mut self, cx: &mut Context<Self>) {
        self.ann_step(Step::Undo, cx)
    }
    pub(super) fn ann_redo(&mut self, cx: &mut Context<Self>) {
        self.ann_step(Step::Redo, cx)
    }
    fn ann_step(&mut self, step: Step, cx: &mut Context<Self>) {
        if self.ann.history.busy() {
            return self.status("Waiting for the previous undo/redo", false, cx);
        }
        let Some((paper, edit)) = self.ann.history.begin(step) else {
            let nothing = match step {
                Step::Undo => "Nothing to undo",
                Step::Redo => "Nothing to redo",
            };
            return self.status(nothing, false, cx);
        };
        let (request, op) = match step {
            Step::Undo => (
                AnnotationRequest::Undo(Box::new(edit.clone())),
                AnnOp::Undo(paper, Box::new(edit.clone())),
            ),
            Step::Redo => (
                AnnotationRequest::Redo(Box::new(edit.clone())),
                AnnOp::Redo(paper, Box::new(edit.clone())),
            ),
        };
        if !self.ann_send(request, op) {
            self.ann.history.unsent(step, edit);
        }
        cx.notify();
    }
    /// Search the annotation's PDF; a newer search or a paper switch cancels this one.
    fn ann_propose(&mut self, id: AnnotationId, cx: &mut Context<Self>) {
        let Some(paper) = self.ann.history.paper() else {
            return;
        };
        self.ann.cancel_search();
        self.ann.proposal = None;
        self.ann.search_serial += 1;
        let serial = self.ann.search_serial;
        let cancel = Cancellation::default();
        self.ann.search = Some((serial, cancel.clone()));
        if self.ann_send(
            AnnotationRequest::Propose { id, cancel },
            AnnOp::Propose(paper, id, serial),
        ) {
            self.status("Searching the PDF for the quoted text…", false, cx);
        } else {
            self.ann.search = None;
        }
    }
    pub(super) fn ann_select(&mut self, id: Option<AnnotationId>, cx: &mut Context<Self>) {
        // An explicit selection replaces one still waiting for the list.
        self.ann.pending = None;
        self.ann.selected = id;
        let comment = self
            .ann
            .selected()
            .and_then(|a| a.comment.clone())
            .unwrap_or_default();
        self.ann.comment.update(cx, |f, cx| f.set_text(comment, cx));
        cx.notify();
    }
    /// Select an annotation of the open paper (search hit, deep link) through `ann_select`,
    /// so the comment editor shows its comment. Until the paper's list has arrived nothing is
    /// selected and the editor is empty, so Save cannot write a stale comment over it.
    pub(super) fn ann_select_when_loaded(&mut self, id: AnnotationId, cx: &mut Context<Self>) {
        self.ann_select(None, cx);
        self.ann.pending = Some(id);
        self.ann_resolve_pending(cx);
    }
    fn ann_resolve_pending(&mut self, cx: &mut Context<Self>) {
        let list = self.ann.loaded.then_some(self.ann.list.as_slice());
        match pending_selection(self.ann.pending, list) {
            PendingSelection::Idle | PendingSelection::Wait => {}
            PendingSelection::Select(id) => self.ann_select(Some(id), cx),
            PendingSelection::Missing(_) => {
                self.ann.pending = None;
                self.status("That annotation no longer exists on this paper", true, cx);
            }
        }
    }
    pub(super) fn ann_save_comment(&mut self, cx: &mut Context<Self>) {
        let Some(current) = self.ann.selected() else {
            return;
        };
        let id = current.id;
        let text = self.ann.comment.read(cx).content.to_string();
        let comment = (!text.trim().is_empty()).then_some(text);
        if current.comment == comment {
            return; // nothing changed: no history step
        }
        self.ann_update(
            id,
            AnnotationPatch {
                comment: Some(comment),
                color: None,
            },
        );
    }
    pub(super) fn ann_save_note(&mut self, page: u32, cx: &mut Context<Self>) {
        let Some(attachment) = self.host.selected else {
            return;
        };
        let text = self.ann.note.read(cx).content.to_string();
        if text.trim().is_empty() {
            self.status("Write the note text first", true, cx);
            return;
        }
        self.ann_create(NewAnnotation {
            attachment_id: attachment,
            kind: AnnotationKind::Note,
            source: Source::Human,
            anchor: Anchor::Resolved {
                page_index: page,
                page_label: None,
                quads: vec![],
            },
            quote: String::new(),
            context_before: None,
            context_after: None,
            color: None,
            comment: Some(text),
        });
        self.ann
            .note
            .update(cx, |f, cx| f.set_text(String::new(), cx));
        self.ann.note_open = false;
        cx.notify();
    }
    /// Jump from the list to the annotation's page in the PDF tab.
    pub(super) fn ann_open(&mut self, a: &Annotation, cx: &mut Context<Self>) {
        self.host.selected = Some(a.attachment_id);
        self.settings.details_tab = "PDF".into();
        self.persist(cx);
        self.ann_select(Some(a.id), cx);
        if let Some(page) = page_of(a) {
            self.pending_jump = Some((a.attachment_id, page));
        }
        cx.notify();
    }
    pub(super) fn subscribe_annotation_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            &self.ann.comment,
            window,
            |this, _, _: &text_input::Submitted, _, cx| this.ann_save_comment(cx),
        )
        .detach();
        cx.subscribe_in(
            &self.ann.note,
            window,
            |this, _, _: &text_input::Submitted, _, cx| {
                let page = this.viewer.as_ref().map_or(0, |v| v.current_page());
                this.ann_save_note(page, cx)
            },
        )
        .detach();
    }
    /// Notes tab: every annotation on the paper's PDFs, in page order, with review actions.
    pub(super) fn notes_tab(&self, cx: &mut Context<Self>) -> Div {
        let mut panel = div().flex().flex_col().gap_2();
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(section("ANNOTATIONS")))
                .child(
                    button("ann-undo", "↶ Undo", self.ann_can_undo())
                        .on_click(cx.listener(|this, _, _, cx| this.ann_undo(cx))),
                )
                .child(
                    button("ann-redo", "↷ Redo", self.ann_can_redo())
                        .on_click(cx.listener(|this, _, _, cx| this.ann_redo(cx))),
                ),
        );
        if self.editor.is_none() {
            return panel.child(hint("Select a paper to see its highlights and notes."));
        }
        if self.ann.list.is_empty() {
            panel = panel.child(hint(
                "No highlights or notes yet. Open the PDF tab, drag across text and choose Highlight, or add a page Note.",
            ));
        }
        // PDF replaced: copy annotations from another attachment as unresolved.
        if let Some(target) = self.host.selected {
            // Only sources with something not yet on this PDF (same rule as the service).
            let sources: Vec<(AttachmentId, String, usize)> = self
                .host
                .attachments
                .iter()
                .filter(|(a, _)| a.id != target)
                .map(|(a, _)| {
                    let pending = pending_carry_over(
                        self.ann.for_attachment(a.id),
                        self.ann.for_attachment(target),
                    )
                    .len();
                    (a.id, a.original_name.clone(), pending)
                })
                .filter(|(_, _, pending)| *pending > 0)
                .collect();
            for (i, (from, name, pending)) in sources.into_iter().enumerate() {
                panel = panel.child(
                    button(
                        ("ann-carry", i),
                        format!("Copy {pending} annotation(s) from {name} (unresolved)"),
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, _| {
                        if let Some(paper) = this.ann.history.paper() {
                            this.ann_send(
                                AnnotationRequest::CarryOver { from, to: target },
                                AnnOp::RecordMany(paper),
                            );
                        }
                    })),
                );
            }
        }
        let names: HashMap<AttachmentId, String> = self
            .host
            .attachments
            .iter()
            .map(|(a, _)| (a.id, a.original_name.clone()))
            .collect();
        for (i, a) in self.ann.list.iter().enumerate() {
            let id = a.id;
            let selected = self.ann.selected == Some(id);
            let unresolved = matches!(a.anchor, Anchor::Unresolved { .. });
            let ai_pending = a.source == Source::Ai && a.confirmed_at.is_none();
            let location = match page_of(a) {
                Some(p) => format!("p. {}", p + 1),
                None => "Unresolved".into(),
            };
            let excerpt: String = if a.kind == AnnotationKind::Note {
                format!("📝 {}", a.comment.clone().unwrap_or_default())
            } else {
                let q: String = a.quote.chars().take(140).collect();
                format!("“{q}”")
            };
            let item = a.clone();
            let mut row = div()
                .id(("ann-row", i))
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(if selected { ACCENT } else { BORDER }))
                .bg(rgb(if selected { ACCENT_SOFT } else { SURFACE }))
                .flex()
                .flex_col()
                .gap_1()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.ann_open(&item, cx)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(10.))
                                .h(px(10.))
                                .rounded_full()
                                .bg(rgb(crate::annotations::rgb(a.color.as_deref()))),
                        )
                        .child(hint(location))
                        .when(names.len() > 1, |el| {
                            el.child(hint(
                                names.get(&a.attachment_id).cloned().unwrap_or_default(),
                            ))
                        })
                        .child(div().flex_1())
                        .when(a.source == Source::Ai, |el| {
                            el.child(
                                div()
                                    .px_1()
                                    .rounded_md()
                                    .text_size(px(11.))
                                    .bg(rgb(if ai_pending { 0xfff4d6 } else { 0xe5efe8 }))
                                    .child(if ai_pending {
                                        "AI · review"
                                    } else {
                                        "AI · confirmed"
                                    }),
                            )
                        })
                        .when(unresolved, |el| {
                            el.child(
                                div()
                                    .px_1()
                                    .rounded_md()
                                    .text_size(px(11.))
                                    .bg(rgb(0xffe5e5))
                                    .child("needs placement"),
                            )
                        }),
                )
                .child(div().text_size(px(12.)).child(excerpt))
                .when(a.kind == AnnotationKind::Highlight, |el| {
                    el.when_some(a.comment.clone(), |el, c| el.child(hint(c)))
                });
            if ai_pending {
                row = row.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(("ann-ai-ok", i), "Confirm", true).on_click(cx.listener(
                                move |this, _, _, _| {
                                    this.ann_record(AnnotationRequest::ConfirmAi(id))
                                },
                            )),
                        )
                        .child(
                            button(("ann-ai-no", i), "Remove", true)
                                .on_click(cx.listener(move |this, _, _, _| this.ann_delete(id))),
                        ),
                );
            }
            if unresolved {
                let proposal = self
                    .ann
                    .proposal
                    .as_ref()
                    .filter(|(pid, _)| *pid == id)
                    .map(|(_, p)| p.clone());
                row = row.child(match proposal {
                    None => div().child(
                        button(("ann-find", i), "Find position in this PDF", true).on_click(
                            cx.listener(move |this, _, _, cx| this.ann_propose(id, cx)),
                        ),
                    ),
                    Some(None) => div().child(hint(
                        "The quoted text was not found in this PDF. Keep it unresolved or delete it.",
                    )),
                    Some(Some(p)) => {
                        let place = p.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(hint(format!(
                                "Found on p. {}{}{}",
                                p.page_index + 1,
                                if p.context_match { " (context matches)" } else { " (context differs)" },
                                if p.matches > 1 { format!(", {} matches", p.matches) } else { String::new() }
                            )))
                            .child(
                                primary_button(("ann-place", i), "Place here", true).on_click(
                                    cx.listener(move |this, _, _, _| {
                                        this.ann.proposal = None;
                                        this.ann_record(AnnotationRequest::Resolve {
                                            id,
                                            page_index: place.page_index,
                                            page_label: place.page_label.clone(),
                                            quads: place.quads.clone(),
                                        })
                                    }),
                                ),
                            )
                    }
                });
            }
            panel = panel.child(row);
        }
        panel.child(hint(format!(
            "Highlights are stored with their text and position; {} undoes, {} redoes.",
            shortcut("Z"),
            if cfg!(target_os = "macos") {
                "⇧⌘Z"
            } else {
                "Ctrl+Shift+Z"
            }
        )))
    }
    /// Colour swatches shared by the viewer's highlight and annotation bars.
    pub(super) fn color_swatches(
        &self,
        prefix: &'static str,
        current: Option<&str>,
        cx: &mut Context<Self>,
        on_pick: fn(&mut LibraryView, &'static str, &mut Context<LibraryView>),
    ) -> Div {
        div()
            .flex()
            .gap_1()
            .children(COLORS.iter().enumerate().map(|(i, color)| {
                let chosen = current == Some(*color);
                div()
                    .id((prefix, i))
                    .w(px(18.))
                    .h(px(18.))
                    .rounded_full()
                    .cursor_pointer()
                    .border_2()
                    .border_color(rgb(if chosen { TEXT } else { SURFACE }))
                    .bg(rgb(crate::annotations::rgb(Some(color))))
                    .on_click(cx.listener(move |this, _, _, cx| on_pick(this, color, cx)))
            }))
    }
}
