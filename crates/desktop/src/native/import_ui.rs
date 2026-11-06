use super::*;
impl LibraryView {
    pub(super) fn cancel_preview(&mut self) {
        if let Some(preview) = self.review.preview.take() {
            let _ = self
                .client
                .import(ImportRequestUi::Cancel(preview.id), Box::new(|_| {}));
        }
        self.review.decisions.clear();
    }
    pub(super) fn start_manual_preview(&mut self, draft: PaperDraft, cx: &mut Context<Self>) {
        self.invalidate_doi();
        self.cancel_preview();
        self.review = Review::default();
        self.parsed = None;
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
                            this.cancel_preview();
                            this.review = Review::default();
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
            Ok(ImportReply::Parsed(parsed))=>{self.review.total=parsed.records;self.review.from_file=true;self.diagnostics=vec![parsed.migration_notice.clone()];self.diagnostics.extend(parsed.diagnostics.iter().map(|d|format!("{} line {}: {}",if d.error{"ERROR"}else{"Warning"},d.line,d.message)));self.parsed=Some(parsed);self.status("Parsed. Review file diagnostics, then request a bounded preview",false,cx);},
            Ok(ImportReply::Preview(preview))=>{self.review.set_preview(preview);self.merge_fields.clear();self.merge_target=0;self.status("Review normalized candidates, source lines and differences before choosing decisions",false,cx);},
            Ok(ImportReply::Committed(outcome))=>{
                self.review.preview=None;self.review.decisions.clear();self.review.committed+=outcome.created.len()+outcome.merged.len();self.review.skipped+=outcome.skipped;
                self.review.batch_start+=self.review.batch_count;
                self.diagnostics.push(format!("Committed batch: {} created, {} merged, {} skipped. Prior commits remain if you cancel.",outcome.created.len(),outcome.merged.len(),outcome.skipped));
                for row in outcome.rows {for diagnostic in row.diagnostics{self.diagnostics.push(format!("Row {:?}, {}: {}",row.source.row,diagnostic.field,diagnostic.message));}}
                self.reload(cx);
                self.status(format!("Saved batch. {} committed / {} skipped; request next preview explicitly",self.review.committed,self.review.skipped),false,cx);
                if !self.review.from_file {let id=outcome.created.first().or(outcome.merged.first()).copied();if let Some(id)=id{self.call(Lane::Editor,Event::Editor,|c,done|c.submit(LibraryRequest::Get(id),done));}}
            }
            Ok(ImportReply::Cancelled)=>self.status("Preview cancelled. Earlier committed batches remain",false,cx),
            Err(e)=>self.status(format!("Import failed: {e}. Nothing in this batch was confirmed; prior commits remain. Cancel/repreview for stale state; reduce batch size for payload limits"),true,cx),
        }
    }
    fn preview_next(&mut self, cx: &mut Context<Self>) {
        if self.busy(Lane::Import) || self.review.preview.is_some() {
            return;
        }
        let start = self.review.batch_start;
        let count = (self.review.total.saturating_sub(start)).min(self.batch_size);
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
        let Some(commit) = self.review.commit() else {
            self.status("Choose Create, Merge or Skip for EVERY row", true, cx);
            return;
        };
        if let Some(index) = self.review.pdf_index {
            let cancel = Cancellation::default();
            self.cancel = cancel.clone();
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
        self.cancel = Cancellation::default();
        let cancel = self.cancel.clone();
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
                .child(line(format!("Import · {} / {} candidates processed · {} committed · {} skipped",self.review.batch_start,self.review.total,self.review.committed,self.review.skipped)))
                .child(button("file-diagnostics","File diagnostics / reports",true).on_click(cx.listener(|this,_,_,cx|{this.screen=Screen::Report;cx.notify();})))
                .child(button("cancel-import","Cancel preview / stop file",!busy).on_click(cx.listener(|this,_,_,cx|{if this.locked(){return;}this.cancel_preview();this.review.from_file=false;this.review.total=this.review.batch_start;this.status("Cancelled. Earlier commits remain saved; no later batch will run",false,cx);this.screen=Screen::Library;}))))
            .child(div().px_3().child("BibTeX/RIS transfer bibliography only; PDFs, notes and organization are not migrated. File and row warnings require review."));
        content = content.child(div().px_3().child(self.review_destination.clone()));
        content = content.child(
            button(
                "batch-size",
                format!("Batch size: {} — cycle (maximum 100)", self.batch_size),
                !busy && self.review.preview.is_none(),
            )
            .m_3()
            .on_click(cx.listener(|this, _, _, cx| {
                if this.locked() || this.review.preview.is_some() {
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
        if let Some(parsed) = &self.parsed {
            content = content.child(div().px_3().child(format!(
                "File: {} accepted candidates · {} file diagnostics (including rejected records)",
                parsed.records,
                parsed.diagnostics.len()
            )));
        }
        if let Some(preview) = &self.review.preview {
            let row = self
                .review
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
                                this.review.choose_valid_creates();
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        button(
                            "commit",
                            "Commit reviewed batch",
                            !busy && self.review.commit().is_some(),
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
                                self.review.batch_start + 1,
                                self.review.batch_start + preview.candidates.len()
                            )))
                            .child(
                                uniform_list(
                                    "import-rows",
                                    preview.candidates.len(),
                                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                        range
                                            .map(|index| {
                                                let c = &this
                                                    .review
                                                    .preview
                                                    .as_ref()
                                                    .expect("review")
                                                    .candidates[index];
                                                let decision = match &this.review.decisions[index] {
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
                                                    this.review.row = index;
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
        } else if self.review.from_file && self.review.batch_start < self.review.total {
            content = content.child(
                button(
                    "next-preview",
                    format!(
                        "Preview next {} candidates",
                        self.batch_size
                            .min(self.review.total - self.review.batch_start)
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
        let Some(preview) = &self.review.preview else {
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
                                this.review.choose(row, ImportAction::Create);
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        button("choose-skip", "Explicit Skip", enabled).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if !this.locked() {
                                    this.review.choose(row, ImportAction::Skip);
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
                                this.review.choose(
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
