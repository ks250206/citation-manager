//! One PDFium worker per process (pdfium-render keeps process-global bindings), shared by
//! PDF metadata reads and #18 page text extraction.
use cm_core::{
    metadata_resolve::Cancellation as CoreCancellation,
    search::extraction::{ExtractError, ExtractedPage, PageBatch, PageTextExtractor},
};
use cm_pdf::worker::{
    Cancellation, Operation, Output, PdfError, PdfWorker, Source, bundled_library,
};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

static WORKER: OnceLock<Result<PdfWorker, String>> = OnceLock::new();
/// Budget per text batch (`PAGES_PER_CALL` pages, one PDF load).
const BATCH_BUDGET: Duration = Duration::from_secs(30);

/// The process worker. The first call decides the library: `explicit` (tests and
/// developer tools pass an absolute pinned library) or the bundle location next to the
/// executable. Later calls reuse that result, including a load failure.
pub fn shared(explicit: Option<&Path>) -> Result<&'static PdfWorker, String> {
    WORKER
        .get_or_init(|| {
            let library = match explicit {
                Some(path) => path.to_path_buf(),
                None => std::env::current_exe()
                    .map_err(|e| e.to_string())
                    .and_then(|exe| bundled_library(&exe).map_err(|e| e.to_string()))?,
            };
            PdfWorker::start(library).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// PDFium-backed extractor. `library: None` uses the bundled library of this executable.
pub struct PdfiumExtractor {
    library: Option<PathBuf>,
}
impl PdfiumExtractor {
    /// The bundled library next to the running executable, if present (`None` when this
    /// installation has no PDF runtime, so no extraction job kind is registered).
    pub fn bundled() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        bundled_library(&exe)
            .ok()
            .filter(|p| p.is_file())
            .map(|_| Self { library: None })
    }
    pub fn with_library(library: PathBuf) -> Self {
        Self {
            library: Some(library),
        }
    }
}
impl PageTextExtractor for PdfiumExtractor {
    fn extract(
        &self,
        path: &Path,
        first_page: u32,
        max_pages: u32,
        max_chars: u32,
        cancel: &CoreCancellation,
    ) -> Result<PageBatch, ExtractError> {
        if cancel.is_cancelled() {
            return Err(ExtractError::Cancelled);
        }
        let worker = shared(self.library.as_deref())
            .map_err(|_| ExtractError::Busy("PDF runtime unavailable".into()))?;
        let output = worker
            .submit(
                Operation::Text {
                    source: Source {
                        path: path.to_path_buf(),
                        password: None,
                    },
                    first_page,
                    max_pages,
                    max_chars,
                },
                Cancellation::default(),
                BATCH_BUDGET,
            )
            .and_then(|ticket| ticket.wait(BATCH_BUDGET + Duration::from_secs(1)))
            .map_err(|e| match e {
                PdfError::PasswordRequiredOrIncorrect | PdfError::UnsupportedSecurity => {
                    ExtractError::Encrypted
                }
                PdfError::Malformed | PdfError::InvalidPage => ExtractError::Malformed,
                PdfError::LimitExceeded => ExtractError::TooLarge,
                PdfError::Cancelled => ExtractError::Cancelled,
                PdfError::QueueFull | PdfError::WorkerClosed => {
                    ExtractError::Busy("PDF worker busy".into())
                }
                PdfError::Deadline => {
                    ExtractError::Failed("page text extraction exceeded its time budget".into())
                }
                PdfError::Io => ExtractError::Failed("stored PDF could not be read".into()),
                _ => ExtractError::Failed("text extraction failed".into()),
            })?;
        let Output::Text(batch) = output else {
            return Err(ExtractError::Failed("unexpected PDF worker reply".into()));
        };
        Ok(PageBatch {
            page_count: batch.page_count,
            sha256: batch.sha256,
            pages: batch
                .pages
                .into_iter()
                .map(|p| ExtractedPage {
                    page_index: p.page_index,
                    page_label: p.page_label,
                    text: p.text,
                    truncated: p.truncated,
                })
                .collect(),
        })
    }
}
