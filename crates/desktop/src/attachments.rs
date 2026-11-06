//! Details/PDF attachment host selection; viewer/annotations are separate capabilities.
pub use cm_protocol::attachments::*;
use cm_protocol::domain::*;
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
