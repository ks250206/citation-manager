//! Selected local sources only. In-process cancellation/report state is not a durable job.
use crate::{domain::*, import::*};
pub use cm_core::{
    attachments::AttachmentStatus,
    geometry::{Quad, Rect, Rotation},
    import::file_metadata::{FileDisposition, PdfIntent},
    metadata_resolve::Cancellation,
};
use std::path::PathBuf;
#[derive(Debug, Clone)]
pub enum Selection {
    Files(Vec<PathBuf>),
    Folder { path: PathBuf, recursive: bool },
}
#[derive(Debug, Clone)]
pub struct PdfFile {
    pub path: PathBuf,
    pub name: String,
    pub sha256: Option<[u8; 32]>,
    pub bytes: u64,
    pub draft: Option<PaperDraft>,
    pub messages: Vec<String>,
    pub disposition: FileDisposition,
    /// Paper retained after a real bibliographic commit, including attachment failure.
    pub paper: Option<PaperId>,
    pub attachment: Option<AttachmentId>,
}
#[derive(Debug, Clone, Default)]
pub struct PdfReport {
    pub files: Vec<PdfFile>,
    pub assessed: usize,
    pub cancelled: bool,
}
#[derive(Debug, Clone)]
pub enum PdfRequest {
    Inspect {
        selection: Selection,
        intent: PdfIntent,
        cancel: Cancellation,
    },
    /// Uses retained inspected source/digest, never arbitrary UI replacement paths.
    Attach {
        indices: Vec<usize>,
        paper: PaperId,
        cancel: Cancellation,
    },
    /// Binds exactly one retained file to a normal reviewed common preview.
    PreviewNew {
        index: usize,
        explicit: Option<Box<PaperDraft>>,
        destination: ImportDestination,
    },
    CommitNew {
        index: usize,
        commit: ImportCommit,
        cancel: Cancellation,
    },
    RetryAttachment {
        index: usize,
        cancel: Cancellation,
    },
    List(PaperId),
    /// Render one page of a stored, hash-verified attachment for the viewer (#17).
    Render(RenderRequest),
}
/// Bounds enforced before any work: zoom is pixels per PDF point (device scale included).
pub const MIN_RENDER_ZOOM: f32 = 0.1;
pub const MAX_RENDER_ZOOM: f32 = 8.0;
#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub attachment: AttachmentId,
    /// Zero-based physical page.
    pub page: u32,
    pub zoom: f32,
    pub cancel: Cancellation,
}
impl RenderRequest {
    pub fn valid(&self) -> bool {
        self.zoom.is_finite() && (MIN_RENDER_ZOOM..=MAX_RENDER_ZOOM).contains(&self.zoom)
    }
}
/// One character in unrotated PDF user space (points); text order is the index order.
#[derive(Debug, Clone, PartialEq)]
pub struct PageChar {
    pub unicode: Option<char>,
    pub quad: Quad,
}
#[derive(Debug, Clone)]
pub struct RenderedPage {
    pub attachment: AttachmentId,
    pub page: u32,
    pub page_count: u32,
    pub label: Option<String>,
    /// MediaBox/CropBox intersection, unrotated PDF points.
    pub crop: Rect,
    /// The page's own /Rotate; the bitmap is already rendered with it.
    pub rotation: Rotation,
    pub zoom: f32,
    pub width: u32,
    pub height: u32,
    /// Straight RGBA, row-major, `width * height * 4` bytes.
    pub rgba: std::sync::Arc<[u8]>,
    /// False for image-only/scanned pages: viewing works, selection does not.
    pub has_text: bool,
    pub chars: Vec<PageChar>,
}
/// Why a page could not be shown. Bibliographic data is unaffected in every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderFailure {
    /// Stored bytes missing or not matching the recorded SHA-256.
    MissingOrCorrupt,
    Encrypted,
    Damaged,
    PageOutOfRange,
    /// Requested bitmap exceeds the pixel budget; choose a lower zoom.
    TooLarge,
    Cancelled,
    /// No PDF runtime in this build/bundle, or the worker stopped.
    RuntimeUnavailable(String),
    Other(String),
}
impl std::fmt::Display for RenderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingOrCorrupt => "The stored PDF is missing or does not match its checksum",
            Self::Encrypted => "This PDF is encrypted or password-protected",
            Self::Damaged => "This PDF is damaged and cannot be displayed",
            Self::PageOutOfRange => "Page out of range",
            Self::TooLarge => "Page too large at this zoom; zoom out",
            Self::Cancelled => "Rendering cancelled",
            Self::RuntimeUnavailable(reason) | Self::Other(reason) => reason,
        })
    }
}
#[derive(Debug, Clone)]
pub enum PdfReply {
    Report(PdfReport),
    Preview(ImportPreview),
    Attachments(Vec<(Attachment, AttachmentStatus)>),
    Page(Result<Box<RenderedPage>, RenderFailure>),
}
pub type PdfProgress = Box<dyn Fn(PdfReport) + Send>;
