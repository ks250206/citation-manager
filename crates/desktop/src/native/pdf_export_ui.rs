//! #20 "Save Current PDF…": viewer toolbar button and the `pdf.save` command. The native
//! save dialog chooses the destination (it asks before replacing an existing file); the
//! daemon writes, re-reads and verifies the copy; the status bar shows the outcome.
use super::*;
use crate::pdf_export::{ExportState, suggested_name};

/// Registered by `commands::pdf_export`; listed in `palette_ui::CONNECTED`.
pub(super) const SAVE_PDF: &str = crate::commands::pdf_export_ids::SAVE;

pub(super) type PdfExportState = ExportState;

impl LibraryView {
    /// The open PDF, if the PDF tab shows one.
    fn exportable_attachment(&self) -> Option<AttachmentId> {
        self.viewer.as_ref().and(self.host.selected)
    }
    /// `pdf.save` and the toolbar button. A second press while saving cancels.
    pub(super) fn save_current_pdf(&mut self, cx: &mut Context<Self>) {
        if self.pdf_export.cancel() {
            self.status("Cancelling save…", false, cx);
            return;
        }
        let Some(attachment) = self.exportable_attachment() else {
            self.status(
                "Open a PDF in the paper's PDF tab, then Save Current PDF…",
                true,
                cx,
            );
            return;
        };
        if let Some(request) = self.pdf_export.take_replace(attachment) {
            // The person confirmed replacing the existing PDF in this app.
            self.send_request(request, cx);
            return;
        }
        if let Some(confirm) = self
            .pdf_export
            .confirm
            .take()
            .filter(|c| c.attachment == attachment)
        {
            // The person saw which annotations cannot be exported and chose to continue.
            self.send_pdf_export(
                attachment,
                confirm.destination,
                confirm.replace_existing,
                true,
                cx,
            );
            return;
        }
        let Some(directory) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
        else {
            self.status(
                "Cannot open the save dialog: home directory is unavailable",
                true,
                cx,
            );
            return;
        };
        let title = self.editor.as_ref().map_or("", |p| p.title.as_str());
        let picker = cx.prompt_for_new_path(&directory, Some(&suggested_name(title)));
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |this, cx| match result {
                Ok(Ok(Some(path))) => {
                    // Not relying on the platform dialog's replace prompt: an existing file
                    // needs a confirmation here first (and must be a .pdf).
                    let exists = std::fs::symlink_metadata(&path).is_ok();
                    match this.pdf_export.picked(attachment, path, exists) {
                        crate::pdf_export::Picked::Export(request) => {
                            this.send_request(request, cx)
                        }
                        crate::pdf_export::Picked::Status(text, error) => {
                            this.status(text, error, cx)
                        }
                    }
                }
                Ok(Ok(None)) => this.status("Save cancelled; nothing written", false, cx),
                _ => this.status("The save dialog failed; nothing written", true, cx),
            });
        })
        .detach();
    }
    fn send_pdf_export(
        &mut self,
        attachment: AttachmentId,
        destination: std::path::PathBuf,
        replace_existing: bool,
        allow_omissions: bool,
        cx: &mut Context<Self>,
    ) {
        let request =
            self.pdf_export
                .begin(attachment, destination, replace_existing, allow_omissions);
        self.send_request(request, cx);
    }
    fn send_request(&mut self, request: PdfRequest, cx: &mut Context<Self>) {
        let PdfRequest::Export(r) = &request else {
            return;
        };
        let target = (r.attachment, r.destination.clone(), r.replace_existing);
        let lost_sender = self.sender.clone();
        let lost_target = target.clone();
        let mut lost = LostReply(Some(Box::new(move || {
            let _ = lost_sender.send(Message {
                lane: Lane::Pdf,
                ticket: 0,
                event: Event::PdfExport(lost_target, Err(ServiceError::Unavailable)),
            });
        })));
        let sender = self.sender.clone();
        let sent = self.client.pdf(
            request,
            Box::new(|_| {}),
            Box::new(move |result| {
                lost.disarm();
                let _ = sender.send(Message {
                    lane: Lane::Pdf,
                    ticket: 0,
                    event: Event::PdfExport(target, result),
                });
            }),
        );
        if sent.is_err() {
            self.pdf_export.running = None;
            self.status(
                "PDF not saved: the library service is unavailable",
                true,
                cx,
            );
        } else {
            self.status(
                "Saving annotated PDF… (written to a temporary file, re-read and verified first)",
                false,
                cx,
            );
        }
    }
    pub(super) fn pdf_export_reply(
        &mut self,
        target: (AttachmentId, std::path::PathBuf, bool),
        result: Result<PdfReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        let (text, error) = self.pdf_export.finish(target, result);
        self.status(text, error, cx);
    }
}

/// Viewer toolbar entry for the same command. Takes the export state and the shown
/// attachment rather than `&self`, so the toolbar can build it while it borrows the viewer.
pub(super) fn save_pdf_button(
    state: &PdfExportState,
    attachment: Option<AttachmentId>,
    cx: &mut Context<LibraryView>,
) -> Div {
    let save = button(
        "pdf-save",
        state.button_label(attachment),
        attachment.is_some(),
    )
    .on_click(cx.listener(|this, _, _, cx| this.save_current_pdf(cx)));
    let row = div().flex().items_center().gap_1().child(save);
    if state.pending_for(attachment) {
        row.child(
            button("pdf-save-dismiss", "✕", true).on_click(cx.listener(|this, _, _, cx| {
                this.pdf_export.dismiss();
                this.status("Nothing written; the existing file was kept", false, cx);
            })),
        )
    } else {
        row
    }
}

#[cfg(test)]
mod tests {
    // No glob import: `gpui::*` would shadow the standard `#[test]` attribute.
    use super::{super::palette_ui, SAVE_PDF};
    #[test]
    fn the_native_host_connects_pdf_save() {
        assert!(palette_ui::CONNECTED.contains(&SAVE_PDF));
        let registry = crate::commands::CommandRegistry::m1();
        let context = crate::commands::CommandContext {
            connected_handlers: palette_ui::CONNECTED,
            library_ready: true,
            has_selection: true,
            busy: false,
        };
        assert_eq!(
            registry.availability(SAVE_PDF, &context),
            crate::commands::Availability::Enabled
        );
    }
}
