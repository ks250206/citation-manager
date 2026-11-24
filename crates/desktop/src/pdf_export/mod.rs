//! #20 "Save Current PDF…": request building and status text for the native host, without
//! GPUI. The daemon does the export and verification; this only phrases what happened and
//! never presents an incomplete or failed export as a plain success.
use cm_protocol::{
    attachments::{
        Cancellation, Omitted, PdfExportFailure, PdfExportReport, PdfExportRequest, PdfReply,
        PdfRequest,
    },
    domain::AttachmentId,
    library::ServiceError,
};
use std::path::PathBuf;

/// File name offered in the save dialog: the paper title without path separators or
/// control characters, at most 100 characters, with an "annotated" suffix.
pub fn suggested_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let mut stem: String = words.join(" ").chars().take(100).collect();
    stem = stem
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned();
    if stem.is_empty() {
        "annotated.pdf".into()
    } else {
        format!("{stem} (annotated).pdf")
    }
}

/// A chosen destination waiting for the person to accept leaving some annotations out.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirmation {
    pub attachment: AttachmentId,
    pub destination: PathBuf,
    pub replace_existing: bool,
    pub omitted: usize,
}

/// Per-window export state: at most one export runs at a time.
#[derive(Debug, Default)]
pub struct ExportState {
    pub running: Option<Cancellation>,
    pub confirm: Option<Confirmation>,
}
impl ExportState {
    /// Toolbar label for the current state.
    pub fn button_label(&self, attachment: Option<AttachmentId>) -> String {
        match (&self.running, &self.confirm) {
            (Some(_), _) => "Cancel Save".into(),
            (None, Some(c)) if Some(c.attachment) == attachment => {
                format!("Save without {} unplaced…", c.omitted)
            }
            _ => "Save PDF…".into(),
        }
    }
    /// Start an export; `allow_omissions` only after the person saw what will be left out.
    pub fn begin(
        &mut self,
        attachment: AttachmentId,
        destination: PathBuf,
        replace_existing: bool,
        allow_omissions: bool,
    ) -> PdfRequest {
        let cancel = Cancellation::default();
        self.running = Some(cancel.clone());
        self.confirm = None;
        PdfRequest::Export(PdfExportRequest {
            attachment,
            destination,
            replace_existing,
            allow_omissions,
            cancel,
        })
    }
    /// Cancel a running export; true when there was one.
    pub fn cancel(&mut self) -> bool {
        self.running.take().inspect(|c| c.cancel()).is_some()
    }
    /// Turn a reply into status text and whether it is an error. A copy with omitted
    /// annotations is reported as incomplete (error styling), never as a plain success.
    pub fn finish(
        &mut self,
        request: (AttachmentId, PathBuf, bool),
        result: Result<PdfReply, ServiceError>,
    ) -> (String, bool) {
        self.running = None;
        match result {
            Ok(PdfReply::Exported(Ok(report))) => saved(&report),
            Ok(PdfReply::Exported(Err(PdfExportFailure::Unexportable(omitted)))) => {
                let (attachment, destination, replace_existing) = request;
                let message = format!(
                    "Nothing saved: {}. Choose \"Save without {} unplaced…\" to save a copy without them",
                    describe(&omitted),
                    omitted.len()
                );
                self.confirm = Some(Confirmation {
                    attachment,
                    destination,
                    replace_existing,
                    omitted: omitted.len(),
                });
                (message, true)
            }
            Ok(PdfReply::Exported(Err(PdfExportFailure::Cancelled))) => {
                ("Save cancelled; nothing written".into(), false)
            }
            Ok(PdfReply::Exported(Err(failure))) => (format!("PDF not saved: {failure}"), true),
            Ok(_) => ("PDF not saved: unexpected reply".into(), true),
            Err(e) => (format!("PDF not saved: {e}"), true),
        }
    }
}

fn describe(omitted: &[Omitted]) -> String {
    match omitted {
        [one] => format!("1 annotation cannot be exported ({})", one.reason),
        many => format!(
            "{} annotations cannot be exported (e.g. {})",
            many.len(),
            many.first().map_or("", |o| o.reason.as_str())
        ),
    }
}

fn saved(report: &PdfExportReport) -> (String, bool) {
    let name = report.path.file_name().map_or_else(
        || report.path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let counts = format!(
        "{} highlight(s), {} note(s){}",
        report.highlights,
        report.notes,
        if report.preserved > 0 {
            format!(", {} existing kept", report.preserved)
        } else {
            String::new()
        }
    );
    if report.complete() {
        (
            format!("Saved and verified {name}: {counts}; original unchanged"),
            false,
        )
    } else {
        (
            format!(
                "Saved {name} INCOMPLETE: {counts}; {} annotation(s) not included",
                report.omitted.len()
            ),
            true,
        )
    }
}
