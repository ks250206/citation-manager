use super::*;
impl LibraryView {
    pub(super) fn load_pdf_form(&mut self, index: usize, cx: &mut Context<Self>) {
        self.pdf_row = index;
        if self.pdf_form.bind(index, self.pdf_report.files.get(index)) {
            for (field, value) in self.pdf_fields.iter().zip(self.pdf_form.values()) {
                field.update(cx, |field, cx| field.set_text(value, cx));
            }
        }
    }
    pub(super) fn inspect(
        &mut self,
        selection: Selection,
        intent: PdfIntent,
        cx: &mut Context<Self>,
    ) {
        if self.locked() {
            return;
        }
        if !self.pause_file_review() {
            cx.notify();
            return;
        }
        self.invalidate_doi();
        let cancel = self.cancels.start_pdf();
        self.pdf_intent = intent.clone();
        self.pdf_report = PdfReport::default();
        self.load_pdf_form(0, cx);
        self.pdf_row = 0;
        self.pdf_selection = Some(selection.clone());
        self.screen = Screen::Pdf;
        self.dispatch_pdf(PdfRequest::Inspect {
            selection,
            intent,
            cancel,
        });
        self.status(
            "Inspecting selected scope, hashes and PDF metadata on workers…",
            false,
            cx,
        );
        self.lock_fields(cx);
    }
    pub(super) fn pick_pdfs(&mut self, folder: bool, intent: PdfIntent, cx: &mut Context<Self>) {
        if self.locked() {
            return;
        }
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: !folder,
            prompt: Some(
                if folder {
                    "Select PDF import folder"
                } else {
                    "Select PDF originals"
                }
                .into(),
            ),
        });
        self.picker_generation += 1;
        let generation = self.picker_generation;
        let recursive = self.recursive;
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |this, cx| {
                if generation != this.picker_generation {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if folder {
                            if let Some(path) = paths.into_iter().next() {
                                this.inspect(Selection::Folder { path, recursive }, intent, cx);
                            }
                        } else {
                            this.inspect(Selection::Files(paths), intent, cx);
                        }
                    }
                    Ok(Ok(None)) => {
                        this.status("PDF selection cancelled; nothing ingested", false, cx)
                    }
                    _ => this.status("Native PDF picker failed", true, cx),
                }
            });
        })
        .detach();
    }
    pub(super) fn pdf_reply(
        &mut self,
        result: Result<PdfReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(PdfReply::Report(report)) => {
                self.pdf_report = report;
                let client = self.client.clone();
                let release_error = self.reviews.finish_pdf(|id| client.import(ImportRequestUi::Cancel(id.clone()), Box::new(|_| {}))).err();
                self.load_pdf_form(self.pdf_row, cx);
                self.screen = Screen::Pdf;
                self.reload(cx);
                if let Some(paper) = self.pdf_report.files.get(self.pdf_row).and_then(|file| file.paper) {
                    self.call(Lane::Editor, Event::Loaded, |c, done| c.submit(LibraryRequest::Get(paper), done));
                }
                let imported = self.pdf_report.files.iter().filter(|f| f.attachment.is_some()).count();
                let failed = self.pdf_report.files.iter().filter(|f| matches!(f.disposition, FileDisposition::Failed(_))).count();
                let warning = release_error.as_ref().map(|error| format!("; preview release failed: {error}; retry before another review")).unwrap_or_default();
                self.status(format!("PDF report: {} assessed; {} attachments saved; {} failures{}{}", self.pdf_report.assessed, imported, failed, if self.pdf_report.cancelled { "; cancelled (prior successes retained)" } else { "" }, warning), failed > 0 || release_error.is_some(), cx);
            }
            Ok(PdfReply::Preview(preview)) => {
                self.reviews.active.pdf_index = Some(self.pdf_row);
                self.reviews.active.set_preview(preview);
                self.screen = Screen::Import;
                self.status("Review PDF registration. Embedded metadata is a suggestion; filename may remain provisional", false, cx);
            }
            Ok(PdfReply::Attachments(_))=>(),
            Err(e)=>self.status(format!("PDF operation failed: {e}. Earlier paper/attachment successes remain; retry explicitly"),true,cx),
        }
    }
    fn pdf_preview(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.locked() {
            return;
        }
        if self
            .pdf_fields
            .iter()
            .any(|field| field.read(cx).is_composing())
        {
            self.status(
                "Finish text conversion before reviewing PDF metadata",
                true,
                cx,
            );
            return;
        }
        let Some(file) = self.pdf_report.files.get(index) else {
            return;
        };
        let values = self
            .pdf_fields
            .iter()
            .map(|field| field.read(cx).content.to_string())
            .collect::<Vec<_>>();
        let explicit = match self.pdf_form.edited_draft(index, file, &values) {
            Ok(d) => Some(Box::new(d)),
            Err(e) => {
                self.status(e.to_string(), true, cx);
                return;
            }
        };
        if !self.pause_file_review() {
            cx.notify();
            return;
        }
        self.pdf_row = index;
        let destination = self.bind_destination();
        self.call(Lane::Pdf, Event::Pdf, |c, done| {
            c.pdf(
                PdfRequest::PreviewNew {
                    index,
                    explicit,
                    destination,
                },
                Box::new(|_| {}),
                done,
            )
        });
        self.status(
            "Preparing PDF candidate through common import policy…",
            false,
            cx,
        );
        self.lock_fields(cx);
    }
    pub(super) fn pdf_screen(&self, cx: &mut Context<Self>) -> Div {
        let enabled = !self.locked();
        div().flex_1().min_h_0().flex().flex_col()
            .child(div().p_3().flex().gap_2()
                .child(button("pdf-picker","Register NEW papers from PDFs…",enabled).on_click(cx.listener(|this,_,_,cx|this.pick_pdfs(false,PdfIntent::RegisterNew,cx))))
                .child(button("pdf-folder","Register from folder…",enabled).on_click(cx.listener(|this,_,_,cx|this.pick_pdfs(true,PdfIntent::RegisterNew,cx))))
                .child(button("recursive",format!("{} Include subfolders",if self.recursive{"☑"}else{"☐"}),enabled).on_click(cx.listener(|this,_,_,cx|{this.recursive = !this.recursive;cx.notify();})))
                .child(button("pdf-cancel","Cancel at file boundary",self.busy(Lane::Pdf)).on_click(cx.listener(|this,_,_,cx|{if this.cancels.cancel_pdf(this.busy(Lane::Pdf)) {this.status("Cancellation requested; in-flight native/atomic operations may finish",false,cx);}})))
                .child(button("pdf-reinspect","Retry inspection",enabled&&self.pdf_selection.is_some()).on_click(cx.listener(|this,_,_,cx|{if let Some(selection)=this.pdf_selection.clone(){this.inspect(selection,this.pdf_intent.clone(),cx);}}))))
            .when_some(self.reviews.paused_file.as_ref(), |el,file| el.child(line(format!("Bibliography paused: {} / {} processed, {} committed, {} skipped. Resume from Import review.",file.batch_start,file.total,file.committed,file.skipped))))
            .child(div().px_3().child("Explicit attachment never changes bibliography. New-paper registration requires a separate reviewed decision. Offline filename/manual registration preserves original bytes even when parsing fails."))
            .child(div().flex().flex_1().min_h_0()
                .child(div().w(px(480.)).flex_shrink_0().flex().flex_col()
                    .child(div().id("pdf-register-drop").p_3().m_2().bg(rgb(0xe5efe8)).child("Drop PDFs: register as NEW (review required)").on_drop(cx.listener(|this,paths:&ExternalPaths,_,cx|{if !this.locked(){this.inspect(Selection::Files(paths.paths().to_vec()),PdfIntent::RegisterNew,cx);}})))
                    .when_some(self.editor.as_ref(),|el,p|{let id=p.id;el.child(div().id("pdf-attach-drop").p_3().m_2().bg(rgb(0xe0eaff)).child(format!("Drop PDFs: attach to selected paper ‘{}’",p.title)).on_drop(cx.listener(move|this,paths:&ExternalPaths,_,cx|{if !this.locked(){this.inspect(Selection::Files(paths.paths().to_vec()),PdfIntent::AttachExisting(id),cx);}})))
                        .child(button("pdf-attach-picker","Pick PDFs to attach to selected paper…",enabled).on_click(cx.listener(move|this,_,_,cx|this.pick_pdfs(false,PdfIntent::AttachExisting(id),cx))))})
                    .child(line(format!("{} files · {} assessed{}",self.pdf_report.files.len(),self.pdf_report.assessed,if self.busy(Lane::Pdf){" · working…"}else{""})))
                    .child(uniform_list("pdf-files",self.pdf_report.files.len(),cx.processor(|this,range: std::ops::Range<usize>,_,cx|{range.map(|index|{let file=&this.pdf_report.files[index];button(("pdf-file",index),format!("{} · {}",file.name,crate::attachments::file_status(&file.disposition)),true).h(px(50.)).overflow_hidden().on_click(cx.listener(move|this,_,_,cx|{if this.locked(){return;}this.load_pdf_form(index,cx);cx.notify();}))}).collect()})).flex_1().min_h_0()))
                .child(div().id("pdf-file-report").flex_1().min_w_0().overflow_y_scroll().p_3()
                    .when_some(self.pdf_report.files.get(self.pdf_row),|el,file|{
                        let index=self.pdf_row;
                        let el=el.child(line(file.path.to_string_lossy().into_owned())).child(line(format!("{} bytes · {}",file.bytes,crate::attachments::file_status(&file.disposition))))
                            .children(file.messages.iter().cloned().map(line))
                            .when_some(file.paper,|el,p|el.child(line(format!("Paper retained: {}",p.0))))
                            .when(file.paper.is_some()&&file.attachment.is_none(),|el|el.child(button("retry-attachment","Retry attachment to retained paper",enabled).on_click(cx.listener(move|this,_,_,cx|{if this.locked(){return;}let cancel=this.cancels.start_pdf();this.dispatch_pdf(PdfRequest::RetryAttachment{index,cancel});cx.notify();}))));
                        match self.pdf_intent{
                            PdfIntent::AttachExisting(paper)=>el.child(line(format!("Attachment destination fixed to paper {}",paper.0)))
                                .child(button("attach-reviewed","Attach ready inspected files (bibliography unchanged)",enabled).on_click(cx.listener(move|this,_,_,cx|{if this.locked(){return;}let indices=this.pdf_report.files.iter().enumerate().filter(|(_,f)|f.disposition==FileDisposition::Ready).map(|(i,_)|i).collect();let cancel=this.cancels.start_pdf();this.dispatch_pdf(PdfRequest::Attach{indices,paper,cancel});this.status("Attaching inspected originals…",false,cx);}))),
                            PdfIntent::RegisterNew=>el.when(file.disposition==FileDisposition::Ready&&file.paper.is_none(),|el|el
                                .child(line("Review/edit metadata. Blank authors/date remain unresolved; filename may be a provisional title. Changing PDF rows resets unsubmitted edits."))
                                .children(crate::details::FIELDS.iter().zip(self.pdf_fields.iter()).map(|(label,field)|div().mb_2().child(line(*label)).child(field.clone())))
                                .child(button("preview-pdf-new","Preview registration as NEW paper",enabled).on_click(cx.listener(move|this,_,_,cx|this.pdf_preview(index,cx)))))
                        }
                    })))
    }
}
