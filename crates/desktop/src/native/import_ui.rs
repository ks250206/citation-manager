use super::*;
impl LibraryView {
    pub(super) fn cancel_preview(&mut self) -> bool {
        let client = self.client.clone();
        let result = self.reviews.cancel_active(|id| {
            client.import(ImportRequestUi::Cancel(id.clone()), Box::new(|_| {}))
        });
        self.review_transition(result)
    }
    pub(super) fn pause_file_review(&mut self) -> bool {
        let client = self.client.clone();
        let result = self
            .reviews
            .pause_file(|id| client.import(ImportRequestUi::Cancel(id.clone()), Box::new(|_| {})));
        self.review_transition(result)
    }
    pub(super) fn resume_file_review(&mut self) -> bool {
        let had_paused_file = self.reviews.paused_file.is_some();
        let client = self.client.clone();
        let result = self
            .reviews
            .resume_file(|id| client.import(ImportRequestUi::Cancel(id.clone()), Box::new(|_| {})));
        let resumed = self.review_transition(result);
        if resumed
            && had_paused_file
            && let Some(parsed) = &self.parsed
        {
            self.diagnostics = vec![parsed.migration_notice.clone()];
            self.diagnostics.extend(parsed.diagnostics.iter().map(|d| {
                format!(
                    "{} line {}: {}",
                    if d.error { "ERROR" } else { "Warning" },
                    d.line,
                    d.message
                )
            }));
            self.diagnostics.push(format!("Resumed at {} / {} processed; {} committed, {} skipped. Earlier commits remain saved.", self.reviews.active.batch_start, self.reviews.active.total, self.reviews.active.committed, self.reviews.active.skipped));
        }
        resumed
    }
    fn review_transition(&mut self, result: Result<(), ServiceError>) -> bool {
        if let Err(error) = result {
            self.message =
                format!("Cannot release current preview: {error}. Retry before switching imports");
            self.error = true;
            false
        } else {
            true
        }
    }
    fn cancel_review(&mut self, cx: &mut Context<Self>) {
        if self.locked() {
            return;
        }
        if !self.cancel_preview() {
            cx.notify();
            return;
        }
        self.reviews.active.from_file = false;
        self.reviews.active.total = self.reviews.active.batch_start;
        self.screen = Screen::Library;
        self.status(
            "Cancelled. Earlier commits remain saved; no later batch will run",
            false,
            cx,
        );
    }
    pub(super) fn start_manual_preview(&mut self, draft: PaperDraft, cx: &mut Context<Self>) {
        self.invalidate_doi();
        if !self.pause_file_review() {
            cx.notify();
            return;
        }
        let destination = self.bind_destination();
        self.call(Lane::Import, Event::Import, |c, done| {
            c.import(
                ImportRequestUi::Preview(ImportRequest {
                    candidates: vec![draft],
                    destination,
                }),
                done,
            )
        });
        self.screen = Screen::Import;
        self.status(
            "Preparing normalized preview and duplicate differences…",
            false,
            cx,
        );
        self.lock_fields(cx);
    }
    pub(super) fn pick_bibliography(&mut self, format: Format, cx: &mut Context<Self>) {
        self.invalidate_doi();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select UTF-8 bibliography".into()),
        });
        self.picker_generation += 1;
        let generation = self.picker_generation;
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |this, cx| {
                if generation != this.picker_generation {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            let client = this.client.clone();
                            let result = this.reviews.replace_file(|id| {
                                client.import(ImportRequestUi::Cancel(id.clone()), Box::new(|_| {}))
                            });
                            if !this.review_transition(result) {
                                cx.notify();
                                return;
                            }
                            this.parsed = None;
                            this.diagnostics.clear();
                            this.call(Lane::Import, Event::Import, |c, done| {
                                c.import(ImportRequestUi::Parse { path, format }, done)
                            });
                            this.screen = Screen::Import;
                            this.status("Parsing bounded UTF-8 bibliography on worker…", false, cx);
                        }
                    }
                    Ok(Ok(None)) => {
                        this.status("File selection cancelled; no records saved", false, cx)
                    }
                    _ => this.status("Bibliography file picker failed", true, cx),
                }
            });
        })
        .detach();
    }
    pub(super) fn import_reply(
        &mut self,
        result: Result<ImportReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        match result{
            Ok(ImportReply::Parsed(parsed))=>{self.reviews.active.total=parsed.records;self.reviews.active.from_file=true;self.diagnostics=vec![parsed.migration_notice.clone()];self.diagnostics.extend(parsed.diagnostics.iter().map(|d|format!("{} line {}: {}",if d.error{"ERROR"}else{"Warning"},d.line,d.message)));self.parsed=Some(parsed);self.status("Parsed. Review file diagnostics, then request a bounded preview",false,cx);},
            Ok(ImportReply::Preview(preview))=>{self.reviews.active.set_preview(preview);self.merge_fields.clear();self.merge_target=0;self.status("Review normalized candidates, source lines and differences before choosing decisions",false,cx);},
            Ok(ImportReply::Committed(outcome))=>{
                self.reviews.active.preview=None;self.reviews.active.decisions.clear();self.reviews.active.committed+=outcome.created.len()+outcome.merged.len();self.reviews.active.skipped+=outcome.skipped;
                self.reviews.active.batch_start+=self.reviews.active.batch_count;
                self.diagnostics.push(format!("Committed batch: {} created, {} merged, {} skipped. Prior commits remain if you cancel.",outcome.created.len(),outcome.merged.len(),outcome.skipped));
                for row in outcome.rows {for diagnostic in row.diagnostics{self.diagnostics.push(format!("Row {:?}, {}: {}",row.source.row,diagnostic.field,diagnostic.message));}}
                self.reload(cx);
                self.status(format!("Saved batch. {} committed / {} skipped; request next preview explicitly",self.reviews.active.committed,self.reviews.active.skipped),false,cx);
                if !self.reviews.active.from_file {let id=outcome.created.first().or(outcome.merged.first()).copied();if let Some(id)=id{self.call(Lane::Editor,Event::Loaded,|c,done|c.submit(LibraryRequest::Get(id),done));}}
            }
            Ok(ImportReply::Cancelled)=>self.status("Preview cancelled. Earlier committed batches remain",false,cx),
            Err(e)=>self.status(format!("Import failed: {e}. Nothing in this batch was confirmed; prior commits remain. Cancel/repreview for stale state; reduce batch size for payload limits"),true,cx),
        }
    }
    fn preview_next(&mut self, cx: &mut Context<Self>) {
        if self.busy(Lane::Import) || self.reviews.active.preview.is_some() {
            return;
        }
        let start = self.reviews.active.batch_start;
        let count = (self.reviews.active.total.saturating_sub(start)).min(self.batch_size);
        if count == 0 {
            return;
        }
        let destination = self.bind_destination();
        self.call(Lane::Import, Event::Import, |c, done| {
            c.import(
                ImportRequestUi::PreviewFile {
                    start,
                    count,
                    destination,
                },
                done,
            )
        });
        self.status("Preparing next separately reviewed preview…", false, cx);
    }
    pub(super) fn commit_review(&mut self, cx: &mut Context<Self>) {
        if self.busy(Lane::Import) || self.busy(Lane::Pdf) {
            return;
        }
        let Some(commit) = self.reviews.active.commit() else {
            self.status("Choose Create, Merge or Skip for EVERY row", true, cx);
            return;
        };
        if let Some(index) = self.reviews.active.pdf_index {
            let cancel = self.cancels.start_pdf();
            self.dispatch_pdf(PdfRequest::CommitNew {
                index,
                commit,
                cancel,
            });
        } else {
            self.call(Lane::Import, Event::Import, |c, done| {
                c.import(ImportRequestUi::Commit(commit), done)
            });
        }
        self.status("Committing reviewed decisions…", false, cx);
        self.lock_fields(cx);
    }
    pub(super) fn lookup_doi(&mut self, cx: &mut Context<Self>) {
        if self.locked() || self.busy(Lane::Doi) {
            return;
        }
        let input = self.doi.read(cx).content.to_string();
        let cancel = self.cancels.start_doi();
        self.call(
            Lane::Doi,
            |result| Event::Doi(Box::new(result)),
            |c, done| c.resolve(input, cancel, done),
        );
        self.status(
            "Looking up DOI (read-only). You can cancel and use manual metadata",
            false,
            cx,
        );
    }
    pub(super) fn doi_reply(
        &mut self,
        result: Result<MetadataResolution, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(resolution) => {
                self.diagnostics = resolution
                    .outcomes
                    .iter()
                    .map(|o| {
                        format!(
                            "{}: {:?}; attempts={}, cached={}",
                            o.provider.name(),
                            o.status,
                            o.attempts,
                            o.cached
                        )
                    })
                    .collect();
                self.diagnostics.extend(resolution.fields.iter().map(|f| {
                    format!(
                        "{} — {} {}: {}",
                        f.field,
                        f.provider.name(),
                        if f.selected {
                            "selected suggestion"
                        } else {
                            "alternative"
                        },
                        f.value
                    )
                }));
                self.diagnostics
                    .extend(resolution.pdf_candidates.iter().map(|p| {
                        format!(
                            "Unfetched PDF suggestion (no access/license proof): {}",
                            p.url
                        )
                    }));
                self.set_editor(None, Some(resolution.draft.clone()), cx);
                self.settings.details_tab = "Info".into();
                if resolution.manual_add {
                    self.screen = Screen::Library;
                    self.status("DOI metadata unavailable/incomplete. Fill the manual title, then preview; no records saved",true,cx);
                } else {
                    let diagnostics = self.diagnostics.clone();
                    self.start_manual_preview(resolution.draft, cx);
                    self.diagnostics = diagnostics;
                }
            }
            Err(e) => self.status(
                format!("DOI lookup failed: {e}. Manual draft remains available; nothing saved"),
                true,
                cx,
            ),
        }
    }
    pub(super) fn import_screen(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.busy(Lane::Import) || self.busy(Lane::Pdf);
        let mut content=div().flex_1().min_h_0().flex().flex_col()
            .child(div().p_3().flex().gap_2()
                .child(line(format!("Import · {} / {} candidates processed · {} committed · {} skipped",self.reviews.active.batch_start,self.reviews.active.total,self.reviews.active.committed,self.reviews.active.skipped)))
                .child(button("file-diagnostics","File diagnostics / reports",true).on_click(cx.listener(|this,_,_,cx|{this.screen=Screen::Report;cx.notify();})))
                .child(button("cancel-import","Cancel preview / stop file",!busy).on_click(cx.listener(|this,_,_,cx|this.cancel_review(cx)))))
            .child(div().px_3().child("BibTeX/RIS transfer bibliography only; PDFs, notes and organization are not migrated. File and row warnings require review."));
        content = content.child(div().px_3().child(self.review_destination.clone()));
        content = content.child(
            button(
                "batch-size",
                format!("Batch size: {} — cycle (maximum 100)", self.batch_size),
                !busy && self.reviews.active.preview.is_none(),
            )
            .m_3()
            .on_click(cx.listener(|this, _, _, cx| {
                if this.locked() || this.reviews.active.preview.is_some() {
                    return;
                }
                this.batch_size = match this.batch_size {
                    100 => 50,
                    50 => 20,
                    20 => 10,
                    10 => 5,
                    5 => 1,
                    _ => 100,
                };
                cx.notify();
            })),
        );
        if let Some(file) = &self.reviews.paused_file {
            content = content.child(line(format!("Bibliography paused: {} / {} processed, {} committed, {} skipped. Next batch still requires a fresh preview and decisions.", file.batch_start, file.total, file.committed, file.skipped)))
                .child(button("resume-file", "Resume bibliography / cancel current preview", !busy).on_click(cx.listener(|this, _, _, cx| {
                    if this.locked() { return; }
                    if this.resume_file_review() { this.status("Bibliography resumed. Request the next preview explicitly", false, cx); } else { cx.notify(); }
                })));
        }
        if let Some(summary) = &self.reviews.replaced_file {
            content = content.child(line(summary.clone()));
        }
        if let Some(parsed) = self
            .parsed
            .as_ref()
            .filter(|_| self.reviews.active.from_file)
        {
            content = content.child(div().px_3().child(format!(
                "File: {} accepted candidates · {} file diagnostics (including rejected records)",
                parsed.records,
                parsed.diagnostics.len()
            )));
        }
        if let Some(preview) = &self.reviews.active.preview {
            let row = self
                .reviews
                .active
                .row
                .min(preview.candidates.len().saturating_sub(1));
            content = content.child(
                div()
                    .p_3()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "create-valid",
                            "Choose Create for eligible rows in THIS preview",
                            !busy,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.locked() {
                                this.reviews.active.choose_valid_creates();
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        button(
                            "commit",
                            "Commit reviewed batch",
                            !busy && self.reviews.active.commit().is_some(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.commit_review(cx))),
                    ),
            );
            content = content.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .w(px(430.))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .child(line(format!(
                                "Preview rows {}–{} (at most 100)",
                                self.reviews.active.batch_start + 1,
                                self.reviews.active.batch_start + preview.candidates.len()
                            )))
                            .child(
                                uniform_list(
                                    "import-rows",
                                    preview.candidates.len(),
                                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                        range
                                            .map(|index| {
                                                let c = &this
                                                    .reviews
                                                    .active
                                                    .preview
                                                    .as_ref()
                                                    .expect("review")
                                                    .candidates[index];
                                                let decision =
                                                    match &this.reviews.active.decisions[index] {
                                                        None => "Choose a decision",
                                                        Some(ImportAction::Create) => "Create",
                                                        Some(ImportAction::Merge { .. }) => "Merge",
                                                        Some(ImportAction::Skip) => "Skip",
                                                    };
                                                let has_error =
                                                    c.diagnostics.iter().any(|d| d.error);
                                                button(
                                                    ("review-row", index),
                                                    format!(
                                                        "{} {} · {} · {}",
                                                        index + 1,
                                                        if has_error { "ERROR" } else { "" },
                                                        c.draft.title.replace(['\r', '\n'], " "),
                                                        decision
                                                    ),
                                                    true,
                                                )
                                                .h(px(44.))
                                                .overflow_hidden()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.reviews.active.row = index;
                                                    this.merge_target = 0;
                                                    this.merge_fields.clear();
                                                    cx.notify();
                                                }))
                                            })
                                            .collect()
                                    }),
                                )
                                .flex_1()
                                .min_h_0(),
                            ),
                    )
                    .child(
                        div()
                            .id("candidate-review")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .p_3()
                            .child(self.candidate_review(row, cx)),
                    ),
            );
        } else if self.reviews.active.from_file
            && self.reviews.active.batch_start < self.reviews.active.total
        {
            content = content.child(
                button(
                    "next-preview",
                    format!(
                        "Preview next {} candidates",
                        self.batch_size
                            .min(self.reviews.active.total - self.reviews.active.batch_start)
                    ),
                    !busy,
                )
                .m_3()
                .on_click(cx.listener(|this, _, _, cx| this.preview_next(cx))),
            );
        } else {
            content = content.child(div().p_4().child(if busy {
                "Working on worker…"
            } else {
                "No pending preview. Review the report or choose another import."
            }));
        }
        content
    }
    fn candidate_review(&self, row: usize, cx: &mut Context<Self>) -> Div {
        let Some(preview) = &self.reviews.active.preview else {
            return div();
        };
        let Some(candidate) = preview.candidates.get(row) else {
            return div();
        };
        let enabled = !self.locked();
        let valid = !candidate.diagnostics.iter().any(|d| d.error);
        let mut result = div()
            .child(line(format!(
                "Candidate {} · source {} line {:?}",
                row + 1,
                candidate.draft.source.adapter,
                candidate.draft.source.row
            )))
            .child(line(format!("Normalized title: {}", candidate.draft.title)))
            .children(
                crate::details::FIELDS
                    .iter()
                    .zip(crate::details::values(&candidate.draft))
                    .map(|(label, value)| line(format!("{label}: {value}"))),
            )
            .children(
                candidate
                    .draft
                    .external_ids
                    .iter()
                    .map(|id| line(format!("Identifier {}: {}", id.namespace, id.value))),
            )
            .children(preview.warnings.iter().cloned().map(line))
            .children(candidate.diagnostics.iter().map(|d| {
                line(format!(
                    "{} {}: {}",
                    if d.error { "ERROR" } else { "Warning" },
                    d.field,
                    d.message
                ))
            }))
            .child(line(format!(
                "Same-batch exact candidates: {:?}",
                candidate.batch_matches
            )))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "choose-create",
                            "Create",
                            enabled
                                && valid
                                && !candidate.matches.iter().any(|m| m.kind.exact())
                                && candidate.batch_matches.is_empty(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.locked() {
                                this.reviews.active.choose(row, ImportAction::Create);
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        button("choose-skip", "Explicit Skip", enabled).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if !this.locked() {
                                    this.reviews.active.choose(row, ImportAction::Skip);
                                    cx.notify();
                                }
                            },
                        )),
                    ),
            );
        for (index, duplicate) in candidate.matches.iter().enumerate() {
            result = result.child(
                div()
                    .mt_3()
                    .p_3()
                    .bg(rgb(0xf0e9dc))
                    .child(
                        button(
                            ("merge-target", index),
                            format!(
                                "Select merge target {}: {} ({:?}){}",
                                index + 1,
                                duplicate.existing.title,
                                duplicate.kind,
                                if duplicate.existing.deleted_at.is_some() {
                                    " — in Trash; restore separately"
                                } else {
                                    ""
                                }
                            ),
                            enabled,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.merge_target = index;
                            this.merge_fields.clear();
                            cx.notify();
                        })),
                    )
                    .children(duplicate.differences.iter().enumerate().map(|(n, diff)| {
                        let field = diff.field.clone();
                        let active = self.merge_target == index;
                        button(
                            ("diff", index * 64 + n),
                            format!(
                                "{} {}\nBEFORE: {}\nAFTER: {}",
                                if active && self.merge_fields.contains(&field) {
                                    "☑"
                                } else {
                                    "☐"
                                },
                                diff.field,
                                diff.before,
                                diff.after
                            ),
                            enabled && active,
                        )
                        .w_full()
                        .bg(rgb(if active && self.merge_fields.contains(&field) {
                            0xffe3ad
                        } else {
                            0xf5eee3
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.merge_target != index || this.locked() {
                                return;
                            }
                            if let Some(i) = this.merge_fields.iter().position(|f| f == &field) {
                                this.merge_fields.remove(i);
                            } else {
                                this.merge_fields.push(field.clone());
                            }
                            cx.notify();
                        }))
                    }))
                    .child(
                        button(
                            ("choose-merge", index),
                            "Choose Merge with checked fields (unchecked fields retained)",
                            enabled
                                && valid
                                && self.merge_target == index
                                && duplicate.existing.deleted_at.is_none(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.locked() && this.merge_target == index {
                                this.reviews.active.choose(
                                    row,
                                    ImportAction::Merge {
                                        match_index: index,
                                        fields: this.merge_fields.clone(),
                                    },
                                );
                                cx.notify();
                            }
                        })),
                    ),
            );
        }
        result
    }
}
