//! Selected local sources only. In-process cancellation/report state is not a durable job.
use crate::{domain::*, import::*};
pub use cm_core::{
    attachments::AttachmentStatus,
    geometry::{Quad, Rect, Rotation},
    import::file_metadata::{FileDisposition, PdfIntent},
    metadata_resolve::Cancellation,
};
use std::path::PathBuf;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Selection {
    Files(Vec<PathBuf>),
    Folder { path: PathBuf, recursive: bool },
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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
    /// Write an annotated copy of a stored attachment to a new file (#20).
    Export(PdfExportRequest),
}
pub use cm_core::pdf_export::Omitted;
/// #20: an annotated *copy* of a stored PDF. The stored original is only read (its SHA-256 is
/// checked before and after); the copy is written next to `destination` as a temporary file,
/// re-read and verified, and only then moved into place.
#[derive(Debug, Clone)]
pub struct PdfExportRequest {
    pub attachment: AttachmentId,
    /// Absolute path the person chose in the save dialog; never inside the library directory.
    pub destination: PathBuf,
    /// The person confirmed replacing this existing file (the native save dialog asks).
    /// Without it an existing destination is refused, never overwritten.
    pub replace_existing: bool,
    /// Write the copy even though some annotations cannot be exported; they are listed in
    /// the report. Without it such an export writes nothing and returns `Unexportable`.
    pub allow_omissions: bool,
    pub cancel: Cancellation,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PdfExportReport {
    pub path: PathBuf,
    pub highlights: usize,
    pub notes: usize,
    /// Annotations the stored PDF already had, verified unchanged in the copy.
    pub preserved: usize,
    /// Not in the copy (only with `allow_omissions`); the export is then *incomplete*.
    pub omitted: Vec<Omitted>,
    /// Hex SHA-256 of the stored original, identical before and after the export.
    pub original_sha256: String,
    /// Largest QuadPoints/Rect difference found when re-reading the copy (points).
    pub max_position_error: f32,
    pub replaced: bool,
}
impl PdfExportReport {
    /// Every annotation of the attachment is in the copy.
    pub fn complete(&self) -> bool {
        self.omitted.is_empty()
    }
}
/// Why nothing was written. A failed export never leaves a partial file at `destination`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PdfExportFailure {
    /// Stored bytes missing or not matching the recorded SHA-256.
    MissingOrCorrupt,
    Encrypted,
    Damaged,
    /// Some annotations cannot be exported (e.g. unresolved after a PDF replacement).
    Unexportable(Vec<Omitted>),
    /// An annotation lies outside its page's visible area or is otherwise not representable.
    InvalidAnnotation,
    /// The destination is not allowed, already exists, or could not be written.
    Destination(String),
    /// The re-read copy did not match the annotations (each mismatch listed).
    VerificationFailed(Vec<String>),
    /// The stored original changed while exporting.
    OriginalChanged,
    Cancelled,
    /// The PDF worker queue was full or the document took too long.
    Busy,
    /// No PDF runtime in this build/bundle, or the worker stopped.
    RuntimeUnavailable(String),
    Other(String),
}
impl std::fmt::Display for PdfExportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOrCorrupt => {
                f.write_str("The stored PDF is missing or does not match its checksum")
            }
            Self::Encrypted => f.write_str("This PDF is encrypted or password-protected"),
            Self::Damaged => f.write_str("This PDF is damaged and cannot be exported"),
            Self::Unexportable(list) => write!(
                f,
                "{} annotation(s) cannot be exported: {}",
                list.len(),
                list.first().map_or("", |o| o.reason.as_str())
            ),
            Self::InvalidAnnotation => {
                f.write_str("An annotation lies outside its page and cannot be exported")
            }
            Self::Destination(reason) | Self::RuntimeUnavailable(reason) | Self::Other(reason) => {
                f.write_str(reason)
            }
            Self::VerificationFailed(problems) => write!(
                f,
                "The exported copy did not verify ({} problem(s)): {}",
                problems.len(),
                problems.first().map_or("", String::as_str)
            ),
            Self::OriginalChanged => f.write_str("The stored PDF changed during export"),
            Self::Cancelled => f.write_str("Export cancelled"),
            Self::Busy => f.write_str("The PDF worker is busy; try again shortly"),
        }
    }
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PageChar {
    pub unicode: Option<char>,
    pub quad: Quad,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Straight RGBA, row-major, `width * height * 4` bytes (base64 on the wire).
    #[serde(with = "crate::ipc::b64")]
    pub rgba: std::sync::Arc<[u8]>,
    /// False for image-only/scanned pages: viewing works, selection does not.
    pub has_text: bool,
    pub chars: Vec<PageChar>,
}
/// Why a page could not be shown. Bibliographic data is unaffected in every case.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RenderFailure {
    /// Stored bytes missing or not matching the recorded SHA-256.
    MissingOrCorrupt,
    Encrypted,
    Damaged,
    PageOutOfRange,
    /// Requested bitmap exceeds the pixel budget; choose a lower zoom.
    TooLarge,
    Cancelled,
    /// The PDF worker queue was full or the page took too long; worth retrying shortly.
    Busy,
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
            Self::Busy => "The PDF renderer is busy; retrying",
            Self::RuntimeUnavailable(reason) | Self::Other(reason) => reason,
        })
    }
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PdfReply {
    Report(PdfReport),
    Preview(ImportPreview),
    Attachments(Vec<(Attachment, AttachmentStatus)>),
    Page(Result<Box<RenderedPage>, RenderFailure>),
    /// #20 annotated PDF export outcome.
    Exported(Result<Box<PdfExportReport>, PdfExportFailure>),
}
pub type PdfProgress = Box<dyn Fn(PdfReport) + Send>;
