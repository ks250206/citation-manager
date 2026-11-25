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

/// A chosen destination that already exists, waiting for the person to confirm replacing it
/// in this app (the platform dialog's own prompt is not relied on).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplaceConfirmation {
    pub attachment: AttachmentId,
    pub destination: PathBuf,
}

/// What to do with the path the save dialog returned.
#[derive(Debug)]
pub enum Picked {
    /// New file: export now.
    Export(PdfRequest),
    /// Status text and whether it is an error; nothing is sent.
    Status(String, bool),
}

/// Identifies one export request; replies carry it back so a late reply from an earlier
/// (e.g. cancelled) export can never change the state of a newer one.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportTarget {
    pub generation: u64,
    pub attachment: AttachmentId,
    pub destination: PathBuf,
    pub replace_existing: bool,
}

/// Per-window export state: at most one export runs and one save dialog is open at a time.
#[derive(Debug, Default)]
pub struct ExportState {
    pub running: Option<Cancellation>,
    pub confirm: Option<Confirmation>,
    pub replace: Option<ReplaceConfirmation>,
    /// Incremented by every `begin`; the current request's generation.
    pub generation: u64,
    /// A save dialog is open.
    pub picking: bool,
}
impl ExportState {
    /// Open the save dialog unless one is already open; false means "do not open another".
    pub fn begin_pick(&mut self) -> bool {
        !std::mem::replace(&mut self.picking, true)
    }
    /// The save dialog closed (any outcome).
    pub fn end_pick(&mut self) {
        self.picking = false;
    }
    /// The reply key for a request `begin` just produced.
    pub fn target(&self, request: &PdfRequest) -> Option<ExportTarget> {
        let PdfRequest::Export(r) = request else {
            return None;
        };
        Some(ExportTarget {
            generation: self.generation,
            attachment: r.attachment,
            destination: r.destination.clone(),
            replace_existing: r.replace_existing,
        })
    }
    /// Toolbar label for the current state.
    pub fn button_label(&self, attachment: Option<AttachmentId>) -> String {
        if self.running.is_some() {
            return "Cancel Save".into();
        }
        match (&self.replace, &self.confirm) {
            (Some(r), _) if Some(r.attachment) == attachment => "Replace Existing PDF".into(),
            (_, Some(c)) if Some(c.attachment) == attachment => {
                format!("Save without {} unplaced…", c.omitted)
            }
            _ => "Save PDF…".into(),
        }
    }
    /// A confirmation is waiting for this attachment (the host shows a dismiss control).
    pub fn pending_for(&self, attachment: Option<AttachmentId>) -> bool {
        attachment.is_some()
            && (self.replace.as_ref().map(|r| r.attachment) == attachment
                || self.confirm.as_ref().map(|c| c.attachment) == attachment)
    }
    /// Drop any waiting confirmation; nothing is written.
    pub fn dismiss(&mut self) {
        self.replace = None;
        self.confirm = None;
    }
    /// The save dialog returned `destination`. An existing file is never replaced on the
    /// dialog's say-so: only `.pdf` files can be replaced, and only after the person confirms
    /// here (`take_replace`). `exists` is whether anything is at that path now.
    pub fn picked(
        &mut self,
        attachment: AttachmentId,
        destination: PathBuf,
        exists: bool,
    ) -> Picked {
        if !exists {
            return Picked::Export(self.begin(attachment, destination, false, false));
        }
        let name = destination
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if !(name.len() > 4 && name.to_ascii_lowercase().ends_with(".pdf")) {
            return Picked::Status(
                format!("“{name}” exists and is not a PDF; nothing written. Choose a new name"),
                true,
            );
        }
        self.confirm = None;
        self.replace = Some(ReplaceConfirmation {
            attachment,
            destination,
        });
        Picked::Status(
            format!(
                "“{name}” already exists. Choose “Replace Existing PDF” to overwrite it, or ✕ to keep it"
            ),
            false,
        )
    }
    /// The person confirmed replacing: the request to send, if a confirmation was waiting.
    pub fn take_replace(&mut self, attachment: AttachmentId) -> Option<PdfRequest> {
        let pending = self.replace.take().filter(|r| r.attachment == attachment)?;
        Some(self.begin(attachment, pending.destination, true, false))
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
        self.generation += 1;
        self.running = Some(cancel.clone());
        self.confirm = None;
        self.replace = None;
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
    ///
    /// A reply for an older generation (an export that was cancelled and superseded) leaves
    /// the current state alone and returns nothing, except that a copy it did save is still
    /// reported, since that file now exists.
    pub fn finish(
        &mut self,
        target: ExportTarget,
        result: Result<PdfReply, ServiceError>,
    ) -> Option<(String, bool)> {
        if target.generation != self.generation {
            return match result {
                Ok(PdfReply::Exported(Ok(report))) => {
                    let (text, error) = saved(&report);
                    Some((format!("Earlier save finished: {text}"), error))
                }
                _ => None,
            };
        }
        self.running = None;
        Some(match result {
            Ok(PdfReply::Exported(Ok(report))) => saved(&report),
            Ok(PdfReply::Exported(Err(PdfExportFailure::Unexportable(omitted)))) => {
                let ExportTarget {
                    attachment,
                    destination,
                    replace_existing,
                    ..
                } = target;
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
        })
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
