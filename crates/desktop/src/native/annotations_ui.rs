//! #19 highlights, comments and notes: requests, undo/redo history and the Notes tab.
use super::*;
use cm_protocol::annotations::{
    Anchor, Annotation, AnnotationId, AnnotationKind, AnnotationPatch, AnnotationReply,
    AnnotationRequest, COLORS, Edit, History, NewAnnotation, Proposal, Source,
};

/// What to do with a reply; replies bypass the single-ticket lanes like page renders.
pub(super) enum AnnOp {
    List(PaperId),
    /// A new user edit: record it for undo, then refresh.
    Record,
    RecordMany,
    Undo(Box<Edit>),
    Redo(Box<Edit>),
    Propose(AnnotationId),
}

pub(super) struct AnnotationState {
    pub(super) list: Vec<Annotation>,
    pub(super) paper: Option<PaperId>,
    pub(super) history: History,
    pub(super) selected: Option<AnnotationId>,
    pub(super) color: &'static str,
    pub(super) comment: Entity<TextInput>,
    pub(super) note: Entity<TextInput>,
    pub(super) note_open: bool,
    pub(super) proposal: Option<(AnnotationId, Option<Proposal>)>,
}
impl AnnotationState {
    pub(super) fn new(cx: &mut Context<LibraryView>) -> Self {
        Self {
            list: vec![],
            paper: None,
            history: History::default(),
            selected: None,
            color: "yellow",
            comment: cx.new(|cx| TextInput::new("Comment", cx)),
            note: cx.new(|cx| TextInput::new("Note text", cx)),
            note_open: false,
            proposal: None,
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
    pub(super) fn ann_send(&mut self, request: AnnotationRequest, op: AnnOp) {
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
    }
    /// Called whenever the open paper changes.
    pub(super) fn reset_annotations(&mut self) {
        let paper = self.editor.as_ref().map(|p| p.id);
        if self.ann.paper != paper {
            self.ann.history.clear();
            self.ann.selected = None;
            self.ann.proposal = None;
            self.ann.list.clear();
        }
        self.ann.paper = paper;
        if let Some(id) = paper {
            self.ann_send(AnnotationRequest::ForPaper(id), AnnOp::List(id));
        }
    }
    fn reload_annotations(&mut self) {
        if let Some(id) = self.ann.paper {
            self.ann_send(AnnotationRequest::ForPaper(id), AnnOp::List(id));
        }
    }
    pub(super) fn annotation_reply(
        &mut self,
        op: AnnOp,
        result: Result<AnnotationReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        match (op, result) {
            (AnnOp::List(paper), Ok(AnnotationReply::List(list))) => {
                if self.ann.paper == Some(paper) {
                    self.ann.list = list;
                    if self.ann.selected().is_none() {
                        self.ann.selected = None;
                    }
                }
            }
            (AnnOp::Record, Ok(AnnotationReply::Edit(edit))) => {
                if let Edit::Created(a) = edit.as_ref() {
                    self.ann.selected = Some(a.id);
                    let text = a.comment.clone().unwrap_or_default();
                    self.ann.comment.update(cx, |f, cx| f.set_text(text, cx));
                }
                self.ann.history.record(*edit);
                self.reload_annotations();
            }
            (AnnOp::RecordMany, Ok(AnnotationReply::Edits(edits))) => {
                let count = edits.len();
                for edit in edits {
                    self.ann.history.record(edit);
                }
                self.status(
                    format!("Copied {count} annotation(s) as unresolved; place each from Notes"),
                    false,
                    cx,
                );
                self.reload_annotations();
            }
            (AnnOp::Undo(edit), Ok(_)) => {
                self.ann.history.undone(*edit);
                self.status("Undone", false, cx);
                self.reload_annotations();
            }
            (AnnOp::Redo(edit), Ok(_)) => {
                self.ann.history.redone(*edit);
                self.status("Redone", false, cx);
                self.reload_annotations();
            }
            (AnnOp::Undo(_) | AnnOp::Redo(_), Err(ServiceError::Conflict)) => {
                self.status(
                    "This annotation changed since; undo history skipped that step",
                    true,
                    cx,
                );
                self.reload_annotations();
            }
            (AnnOp::Propose(id), Ok(AnnotationReply::Proposal(proposal))) => {
                self.ann.proposal = Some((id, proposal));
            }
            (_, Err(e)) => {
                self.status(format!("Annotation change failed: {e}"), true, cx);
                self.reload_annotations();
            }
            _ => {}
        }
        cx.notify();
    }
    pub(super) fn ann_create(&mut self, input: NewAnnotation) {
        self.ann_send(AnnotationRequest::Create(Box::new(input)), AnnOp::Record);
    }
    pub(super) fn ann_update(&mut self, id: AnnotationId, patch: AnnotationPatch) {
        self.ann_send(AnnotationRequest::Update(id, patch), AnnOp::Record);
    }
    pub(super) fn ann_delete(&mut self, id: AnnotationId) {
        if self.ann.selected == Some(id) {
            self.ann.selected = None;
        }
        self.ann_send(AnnotationRequest::Delete(id), AnnOp::Record);
    }
    pub(super) fn ann_undo(&mut self, cx: &mut Context<Self>) {
        match self.ann.history.take_undo() {
            Some(edit) => {
                self.ann_send(
                    AnnotationRequest::Undo(Box::new(edit.clone())),
                    AnnOp::Undo(Box::new(edit)),
                );
            }
            None => self.status("Nothing to undo", false, cx),
        }
    }
    pub(super) fn ann_redo(&mut self, cx: &mut Context<Self>) {
        match self.ann.history.take_redo() {
            Some(edit) => {
                self.ann_send(
                    AnnotationRequest::Redo(Box::new(edit.clone())),
                    AnnOp::Redo(Box::new(edit)),
                );
            }
            None => self.status("Nothing to redo", false, cx),
        }
    }
    pub(super) fn ann_select(&mut self, id: Option<AnnotationId>, cx: &mut Context<Self>) {
        self.ann.selected = id;
        let comment = self
            .ann
            .selected()
            .and_then(|a| a.comment.clone())
            .unwrap_or_default();
        self.ann.comment.update(cx, |f, cx| f.set_text(comment, cx));
        cx.notify();
    }
    pub(super) fn ann_save_comment(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.ann.selected else { return };
        let text = self.ann.comment.read(cx).content.to_string();
        self.ann_update(
            id,
            AnnotationPatch {
                comment: Some((!text.trim().is_empty()).then_some(text)),
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
                    button("ann-undo", "↶ Undo", self.ann.history.can_undo())
                        .on_click(cx.listener(|this, _, _, cx| this.ann_undo(cx))),
                )
                .child(
                    button("ann-redo", "↷ Redo", self.ann.history.can_redo())
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
            let sources: Vec<(AttachmentId, String)> = self
                .host
                .attachments
                .iter()
                .filter(|(a, _)| a.id != target && self.ann.for_attachment(a.id).next().is_some())
                .map(|(a, _)| (a.id, a.original_name.clone()))
                .collect();
            for (i, (from, name)) in sources.into_iter().enumerate() {
                panel = panel.child(
                    button(
                        ("ann-carry", i),
                        format!("Copy annotations from {name} (unresolved)"),
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.ann_send(
                            AnnotationRequest::CarryOver { from, to: target },
                            AnnOp::RecordMany,
                        )
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
                                    this.ann_send(AnnotationRequest::ConfirmAi(id), AnnOp::Record)
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
                            cx.listener(move |this, _, _, cx| {
                                this.ann.proposal = None;
                                this.ann_send(AnnotationRequest::Propose(id), AnnOp::Propose(id));
                                this.status("Searching the PDF for the quoted text…", false, cx);
                            }),
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
                                        this.ann_send(
                                            AnnotationRequest::Resolve {
                                                id,
                                                page_index: place.page_index,
                                                page_label: place.page_label.clone(),
                                                quads: place.quads.clone(),
                                            },
                                            AnnOp::Record,
                                        )
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
