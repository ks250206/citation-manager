//! Page text extraction contract (#18). The daemon runs `run` inside the persistent
//! `fulltext.extract` job; storage implements `FulltextStore`, cm-pdf (PDFium) implements
//! `PageTextExtractor`.
//!
//! Durability/resume: pages are committed in bounded batches together with the attachment's
//! `next_page`, so a crashed or cancelled job resumes at the first uncommitted page. Each
//! commit is one write transaction (it invalidates open #33 import previews, like any
//! committed library write); batches are therefore large (`COMMIT_PAGES`/`COMMIT_TEXT_BYTES`).
//! A document that cannot be read (encrypted, damaged, image-only, too large) gets an
//! explicit per-attachment state and the job continues with the next attachment. A
//! transient failure (batch time budget, read error) keeps the attachment pending with an
//! attempt count; later runs retry it and it becomes `Failed` after
//! `MAX_TRANSIENT_ATTEMPTS` consecutive transient failures without progress.
use crate::{ServiceError, metadata_resolve::Cancellation};
use cm_domain::{AttachmentId, PaperId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Pages per document beyond this are not extracted; the state becomes `Partial`.
pub const MAX_PAGES_PER_DOCUMENT: u32 = 5_000;
/// Unicode scalars kept per page; longer page text is cut and counted as truncated.
pub const MAX_CHARS_PER_PAGE: u32 = 100_000;
/// Pages requested from the extractor per call (one PDF load and hash of the whole file, so
/// calls are as large as the PDF worker allows).
pub const PAGES_PER_CALL: u32 = 64;
/// Consecutive transient failures of one attachment before it is marked `Failed`.
pub const MAX_TRANSIENT_ATTEMPTS: u32 = 3;
/// A batch is committed after this many pages or text bytes, or at the document end.
pub const COMMIT_PAGES: usize = 64;
pub const COMMIT_TEXT_BYTES: usize = 4 << 20;
/// Attachments handled per job run; the rest stay pending for the next run.
pub const MAX_ATTACHMENTS_PER_RUN: u32 = 500;
/// Wall-clock budget of one run. When it is used up the run commits what it has, ends
/// successfully with `more_pending` and the next run resumes (keeps each job attempt well
/// inside the job runner's per-attempt limit).
pub const RUN_TIME_BUDGET: std::time::Duration = std::time::Duration::from_secs(20 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionState {
    /// No extraction has run for this attachment yet.
    Pending,
    /// Started; `next_page` pages are committed. Resumed by the next job run.
    Extracting,
    Complete,
    /// Some text is indexed, but pages were truncated, skipped by the page limit or failed.
    Partial,
    /// Readable PDF without extractable text (e.g. scanned/image-only). OCR is out of scope.
    NoText,
    /// Password-protected or unsupported security handler.
    Encrypted,
    /// Damaged or not a PDF.
    Malformed,
    /// Over the input size limit of the PDF worker.
    TooLarge,
    /// Other error (including a stored file whose bytes no longer match its hash).
    Failed,
}
impl ExtractionState {
    pub const STORED: [ExtractionState; 8] = [
        Self::Extracting,
        Self::Complete,
        Self::Partial,
        Self::NoText,
        Self::Encrypted,
        Self::Malformed,
        Self::TooLarge,
        Self::Failed,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Extracting => "extracting",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::NoText => "no_text",
            Self::Encrypted => "encrypted",
            Self::Malformed => "malformed",
            Self::TooLarge => "too_large",
            Self::Failed => "failed",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::STORED
            .into_iter()
            .chain([Self::Pending])
            .find(|s| s.as_str() == value)
    }
    /// The attachment's text is not (fully) searchable for a permanent reason.
    pub fn lacks_text(self) -> bool {
        matches!(
            self,
            Self::NoText | Self::Encrypted | Self::Malformed | Self::TooLarge | Self::Failed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentExtraction {
    pub attachment_id: AttachmentId,
    pub paper_id: PaperId,
    pub state: ExtractionState,
    pub page_count: u32,
    /// Pages committed so far (equals page_count, or the page limit, when finished).
    pub pages_done: u32,
    pub pages_with_text: u32,
    pub truncated_pages: u32,
    /// Fixed, non-content reason (never PDF text or a password).
    pub detail: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum ExtractScope {
    /// Every PDF attachment without a finished extraction (never started or interrupted).
    Pending,
    Attachment(AttachmentId),
    Paper(PaperId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractTarget {
    pub attachment_id: AttachmentId,
    pub paper_id: PaperId,
    pub sha256: [u8; 32],
    /// Absolute managed path, resolved by storage inside the library root.
    pub path: PathBuf,
    /// First page not yet committed (resume point).
    pub resume_from: u32,
    /// Existing rows must be cleared first (forced re-extraction or a stale record).
    pub reset: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPage {
    pub page_index: u32,
    pub page_label: Option<String>,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageBatch {
    pub page_count: u32,
    /// SHA-256 of the bytes actually parsed.
    pub sha256: [u8; 32],
    pub pages: Vec<ExtractedPage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    Encrypted,
    Malformed,
    TooLarge,
    Cancelled,
    /// Transient: worker busy/closed or runtime not loaded yet. The job is retried.
    Busy(String),
    /// This attachment's attempt failed for a reason that may pass (batch time budget,
    /// read error). Retried by a later run, at most `MAX_TRANSIENT_ATTEMPTS` times in a row.
    /// The reason is a fixed, non-content diagnostic.
    Transient(String),
    /// Permanent for this attachment.
    Failed(String),
}

pub trait PageTextExtractor: Send + Sync {
    /// Pages `first_page .. first_page + max_pages` (clamped to the document), each cut to
    /// `max_chars` Unicode scalars.
    fn extract(
        &self,
        path: &Path,
        first_page: u32,
        max_pages: u32,
        max_chars: u32,
        cancel: &Cancellation,
    ) -> Result<PageBatch, ExtractError>;
}

/// Commit of one batch. `finish` closes the attachment (Some(page_limit_hit)).
pub struct StoreBatch<'a> {
    pub target: &'a ExtractTarget,
    pub page_count: u32,
    pub pages: &'a [ExtractedPage],
    pub next_page: u32,
    pub finish: Option<bool>,
}

pub trait FulltextStore {
    fn targets(
        &mut self,
        scope: ExtractScope,
        force: bool,
        limit: u32,
    ) -> Result<Vec<ExtractTarget>, ServiceError>;
    /// Removes stored pages and status of one attachment (one transaction).
    fn reset(&mut self, attachment: AttachmentId) -> Result<(), ServiceError>;
    /// Upserts pages and the extraction row in one transaction; returns the stored state.
    fn store(&mut self, batch: StoreBatch<'_>) -> Result<ExtractionState, ServiceError>;
    fn fail(
        &mut self,
        target: &ExtractTarget,
        state: ExtractionState,
        detail: &str,
    ) -> Result<(), ServiceError>;
    /// Records a transient failure: the attachment stays unfinished (resumable at its
    /// committed `next_page`) and its consecutive failure count grows; a committed batch
    /// resets the count. Returns the new count.
    fn defer(&mut self, target: &ExtractTarget, detail: &str) -> Result<u32, ServiceError>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionSummary {
    pub attachments: u32,
    pub pages: u32,
    pub complete: u32,
    pub partial: u32,
    pub no_text: u32,
    pub unreadable: u32,
    /// More targets than `MAX_ATTACHMENTS_PER_RUN` were pending.
    pub more_pending: bool,
    /// Attachments left unfinished after a transient failure; a later run retries them.
    #[serde(default)]
    pub deferred: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    Cancelled,
    Retryable(String),
    Fatal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub attachments_done: u32,
    pub attachments_total: u32,
    pub page: u32,
    pub page_count: u32,
}

/// Runs extraction for `scope`. `progress` returns false to stop (cancel or lost lease);
/// committed batches remain and the next run resumes from them.
pub fn run(
    store: &mut dyn FulltextStore,
    extractor: &dyn PageTextExtractor,
    scope: ExtractScope,
    force: bool,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<ExtractionSummary, RunError> {
    run_with_budget(
        store,
        extractor,
        scope,
        force,
        cancel,
        progress,
        RUN_TIME_BUDGET,
    )
}

/// `run` with an explicit wall-clock budget (tests use a zero budget).
pub fn run_with_budget(
    store: &mut dyn FulltextStore,
    extractor: &dyn PageTextExtractor,
    scope: ExtractScope,
    force: bool,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(Progress) -> bool,
    budget: std::time::Duration,
) -> Result<ExtractionSummary, RunError> {
    let started = std::time::Instant::now();
    let storage = |e: ServiceError| match e {
        ServiceError::Unavailable | ServiceError::Conflict => RunError::Retryable(e.to_string()),
        other => RunError::Fatal(other.to_string()),
    };
    let mut targets = store
        .targets(scope, force, MAX_ATTACHMENTS_PER_RUN + 1)
        .map_err(storage)?;
    let mut summary = ExtractionSummary {
        more_pending: targets.len() > MAX_ATTACHMENTS_PER_RUN as usize,
        ..Default::default()
    };
    targets.truncate(MAX_ATTACHMENTS_PER_RUN as usize);
    let total = targets.len() as u32;
    for (done, target) in targets.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(RunError::Cancelled);
        }
        if target.reset {
            store.reset(target.attachment_id).map_err(storage)?;
        }
        let mut next = target.resume_from;
        let mut buffer: Vec<ExtractedPage> = Vec::new();
        let mut buffered_bytes = 0usize;
        let mut page_count = 0;
        let state = loop {
            let report = Progress {
                attachments_done: done as u32,
                attachments_total: total,
                page: next,
                page_count,
            };
            if !progress(report) || cancel.is_cancelled() {
                return Err(RunError::Cancelled);
            }
            let batch = match extractor.extract(
                &target.path,
                next,
                PAGES_PER_CALL,
                MAX_CHARS_PER_PAGE,
                cancel,
            ) {
                Ok(batch) => batch,
                Err(ExtractError::Cancelled) => return Err(RunError::Cancelled),
                Err(ExtractError::Busy(reason)) => return Err(RunError::Retryable(reason)),
                Err(ExtractError::Transient(reason)) => {
                    // Keep what this run already extracted for the attachment.
                    if !buffer.is_empty() {
                        summary.pages += buffer.len() as u32;
                        store
                            .store(StoreBatch {
                                target,
                                page_count,
                                pages: &buffer,
                                next_page: next,
                                finish: None,
                            })
                            .map_err(storage)?;
                        buffer.clear();
                    }
                    let attempts = store.defer(target, &reason).map_err(storage)?;
                    if attempts < MAX_TRANSIENT_ATTEMPTS {
                        break ExtractionState::Pending;
                    }
                    store
                        .fail(
                            target,
                            ExtractionState::Failed,
                            "text extraction failed repeatedly (time budget or read error)",
                        )
                        .map_err(storage)?;
                    break ExtractionState::Failed;
                }
                Err(e) => {
                    let (state, detail) = match e {
                        ExtractError::Encrypted => (
                            ExtractionState::Encrypted,
                            "password-protected or unsupported security",
                        ),
                        ExtractError::Malformed => {
                            (ExtractionState::Malformed, "damaged or not a PDF")
                        }
                        ExtractError::TooLarge => (
                            ExtractionState::TooLarge,
                            "PDF exceeds the worker input limit",
                        ),
                        _ => (ExtractionState::Failed, "text extraction failed"),
                    };
                    // Pages already committed (from earlier batches) stay searchable.
                    store.fail(target, state, detail).map_err(storage)?;
                    break state;
                }
            };
            if batch.sha256 != target.sha256 {
                store
                    .fail(
                        target,
                        ExtractionState::Failed,
                        "stored file does not match its recorded SHA-256",
                    )
                    .map_err(storage)?;
                break ExtractionState::Failed;
            }
            let before = next;
            page_count = batch.page_count;
            let limit = batch.page_count.min(MAX_PAGES_PER_DOCUMENT);
            let limited = batch.page_count > MAX_PAGES_PER_DOCUMENT;
            for page in batch.pages {
                if page.page_index >= limit || page.page_index < next {
                    continue;
                }
                buffered_bytes += page.text.len();
                next = page.page_index + 1;
                buffer.push(page);
            }
            let finished = next >= limit;
            if finished || buffer.len() >= COMMIT_PAGES || buffered_bytes >= COMMIT_TEXT_BYTES {
                summary.pages += buffer.len() as u32;
                let state = store
                    .store(StoreBatch {
                        target,
                        page_count: batch.page_count,
                        pages: &buffer,
                        next_page: next,
                        finish: finished.then_some(limited),
                    })
                    .map_err(storage)?;
                buffer.clear();
                buffered_bytes = 0;
                if finished {
                    break state;
                }
            }
            if started.elapsed() >= budget {
                // Out of time: commit the buffered pages and leave the rest to the next run.
                if !buffer.is_empty() {
                    summary.pages += buffer.len() as u32;
                    store
                        .store(StoreBatch {
                            target,
                            page_count: batch.page_count,
                            pages: &buffer,
                            next_page: next,
                            finish: None,
                        })
                        .map_err(storage)?;
                }
                summary.more_pending = true;
                return Ok(summary);
            }
            if next == before {
                // A batch that makes no progress would loop forever.
                store
                    .fail(
                        target,
                        ExtractionState::Failed,
                        "extractor returned no pages",
                    )
                    .map_err(storage)?;
                break ExtractionState::Failed;
            }
        };
        if state == ExtractionState::Pending {
            summary.deferred += 1;
            continue;
        }
        summary.attachments += 1;
        match state {
            ExtractionState::Complete => summary.complete += 1,
            ExtractionState::Partial => summary.partial += 1,
            ExtractionState::NoText => summary.no_text += 1,
            _ => summary.unreadable += 1,
        }
    }
    let _ = progress(Progress {
        attachments_done: total,
        attachments_total: total,
        page: 0,
        page_count: 0,
    });
    Ok(summary)
}
