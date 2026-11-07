//! Details/PDF attachment host selection; viewer/annotations are separate capabilities.
pub use cm_protocol::attachments::*;
use cm_protocol::domain::*;
use cm_protocol::{import::PaperDraft, library::ServiceError};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq)]
struct PdfOrigin {
    index: usize,
    path: PathBuf,
    sha256: [u8; 32],
}
/// A dedicated PDF form can never borrow the library/manual editor's metadata.
#[derive(Default)]
pub struct PdfForm {
    origin: Option<PdfOrigin>,
    draft: Option<PaperDraft>,
}
impl PdfForm {
    fn origin(index: usize, file: &PdfFile) -> Option<PdfOrigin> {
        (file.disposition == FileDisposition::Ready && file.paper.is_none()).then_some(())?;
        Some(PdfOrigin {
            index,
            path: file.path.clone(),
            sha256: file.sha256?,
        })
    }
    /// Returns true only when fields must be reloaded. Matching origins retain edits.
    pub fn bind(&mut self, index: usize, file: Option<&PdfFile>) -> bool {
        let origin = file.and_then(|file| Self::origin(index, file));
        if self.origin == origin && self.draft.is_some() {
            return false;
        }
        self.origin = origin;
        self.draft = self.origin.as_ref().and_then(|_| file?.draft.clone());
        true
    }
    pub fn values(&self) -> Vec<String> {
        crate::details::values(self.draft.as_ref().unwrap_or(&PaperDraft::default()))
    }
    pub fn edited_draft(
        &self,
        index: usize,
        file: &PdfFile,
        values: &[String],
    ) -> Result<PaperDraft, ServiceError> {
        if self.origin.is_none() || self.origin != Self::origin(index, file) {
            return Err(ServiceError::InvalidInput(
                "PDF form source changed; select the inspected row again".into(),
            ));
        }
        crate::details::apply(
            self.draft.as_ref().ok_or(ServiceError::Unavailable)?,
            values,
        )
    }
}

/// Independent local operations must never share a cancellation token.
#[derive(Default)]
pub struct OperationCancels {
    doi: Cancellation,
    pdf: Cancellation,
}
impl OperationCancels {
    pub fn start_doi(&mut self) -> Cancellation {
        self.doi.cancel();
        self.doi = Cancellation::default();
        self.doi.clone()
    }
    pub fn start_pdf(&mut self) -> Cancellation {
        self.pdf.cancel();
        self.pdf = Cancellation::default();
        self.pdf.clone()
    }
    pub fn cancel_doi(&self) {
        self.doi.cancel();
    }
    pub fn cancel_pdf(&self, busy: bool) -> bool {
        if busy {
            self.pdf.cancel();
        }
        busy
    }
}
#[derive(Default)]
pub struct AttachmentHost {
    pub attachments: Vec<(Attachment, AttachmentStatus)>,
    pub selected: Option<AttachmentId>,
    pub paper: Option<PaperId>,
}
impl AttachmentHost {
    pub fn set(&mut self, paper: PaperId, attachments: Vec<(Attachment, AttachmentStatus)>) {
        if self.paper != Some(paper) {
            self.selected = None;
        }
        self.paper = Some(paper);
        if !attachments.iter().any(|(a, _)| Some(a.id) == self.selected) {
            self.selected = attachments.first().map(|(a, _)| a.id);
        }
        self.attachments = attachments;
    }
}

pub fn file_status(disposition: &FileDisposition) -> String {
    match disposition {
        FileDisposition::Ready => "Ready for explicit review".into(),
        FileDisposition::OutsideScope => "Excluded: outside selected scope".into(),
        FileDisposition::UnsafeLink => {
            "Excluded: link, unsafe ancestor or nonregular source".into()
        }
        FileDisposition::NonPdf => "Excluded: not a PDF filename".into(),
        FileDisposition::Oversized => "Excluded: exceeds file size limit".into(),
        FileDisposition::DuplicateHash => "Skipped: same file hash in this selection".into(),
        FileDisposition::Failed(message) => format!("Failed: {message}"),
        FileDisposition::Cancelled => "Cancelled before processing".into(),
        FileDisposition::Imported(_) => "Attachment saved".into(),
    }
}
