//! #18 composition: extraction/rebuild job handlers, automatic extraction submission and
//! the PDFium-backed `PageTextExtractor` (behind `pdf-metadata`, the PDF runtime feature).
//!
//! Interaction with #33 import previews: every committed library write invalidates open
//! previews (conservative `data_version`). Extraction therefore commits pages in large
//! batches (`COMMIT_PAGES`/`COMMIT_TEXT_BYTES`, i.e. typically one commit per document) and
//! the automatic submission is a single job row per pending set. A preview opened while an
//! extraction job runs may need to be re-created; commits themselves never lose data.
#[cfg(feature = "pdf-metadata")]
pub mod pdf_runtime;
#[cfg(feature = "pdf-metadata")]
pub use pdf_runtime::PdfiumExtractor;

use crate::jobs::{JobContext, JobHandler, Outcome};
use cm_core::{
    ServiceError,
    policy::Capability,
    search::extraction::{self, PageTextExtractor, RunError},
};
use cm_protocol::jobs::{
    JobProgress,
    kinds::{self, FulltextExtract, SearchRebuild},
};
use serde_json::{Value, json};
use std::sync::Arc;

fn parse<T: serde::de::DeserializeOwned>(payload: &Value) -> Result<T, ServiceError> {
    serde_json::from_value(payload.clone())
        .map_err(|e| ServiceError::InvalidInput(format!("invalid job payload: {e}")))
}

/// `fulltext.extract`: resumable, cancellable, bounded page text extraction.
pub struct FulltextExtractHandler {
    pub extractor: Arc<dyn PageTextExtractor>,
}
impl JobHandler for FulltextExtractHandler {
    fn kind(&self) -> &'static str {
        kinds::FULLTEXT_EXTRACT
    }
    fn required(&self) -> &'static [Capability] {
        &[Capability::LibraryWrite]
    }
    /// A run ends itself after `RUN_TIME_BUDGET` (20 min) plus at most one batch (30 s
    /// worker budget); progress is reported before every batch.
    fn max_run(&self) -> std::time::Duration {
        extraction::RUN_TIME_BUDGET + std::time::Duration::from_secs(10 * 60)
    }
    fn validate(&self, payload: &Value) -> Result<(), ServiceError> {
        let job: FulltextExtract = parse(payload)?;
        match job.scope {
            extraction::ExtractScope::Attachment(id) if id.0.is_nil() => {
                Err(ServiceError::InvalidInput("attachment ID required".into()))
            }
            extraction::ExtractScope::Paper(id) if id.0.is_nil() => {
                Err(ServiceError::InvalidInput("paper ID required".into()))
            }
            _ => Ok(()),
        }
    }
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome {
        let job: FulltextExtract = match parse(ctx.payload) {
            Ok(job) => job,
            Err(e) => return Outcome::Fatal(e.to_string()),
        };
        let cancel = ctx.cancel.clone();
        let report = ctx.reporter();
        let mut progress = |p: extraction::Progress| {
            report(JobProgress {
                done: u64::from(p.attachments_done),
                total: Some(u64::from(p.attachments_total)),
                message: Some(if p.page_count > 0 {
                    format!("page {} of {}", p.page, p.page_count)
                } else {
                    format!("page {}", p.page)
                }),
            })
        };
        let mut store = match ctx.db.fulltext(ctx.directory) {
            Ok(store) => store,
            Err(e) => return Outcome::Retryable(e.to_string()),
        };
        match extraction::run(
            &mut store,
            self.extractor.as_ref(),
            job.scope,
            job.force,
            &cancel,
            &mut progress,
        ) {
            Ok(summary) if summary.attachments == 0 => {
                // Nothing left to do: an earlier (possibly crashed) attempt committed it.
                Outcome::AlreadyCommitted(json!(summary))
            }
            Ok(summary) => Outcome::Succeeded(json!(summary)),
            Err(RunError::Cancelled) => {
                if cancel.is_cancelled() {
                    Outcome::Cancelled
                } else {
                    Outcome::Retryable("lease lost".into())
                }
            }
            Err(RunError::Retryable(reason)) => Outcome::Retryable(reason),
            Err(RunError::Fatal(reason)) => Outcome::Fatal(reason),
        }
    }
}

/// `search.rebuild`: rebuild the projection and FTS indexes from source tables.
pub struct SearchRebuildHandler;
impl JobHandler for SearchRebuildHandler {
    fn kind(&self) -> &'static str {
        kinds::SEARCH_REBUILD
    }
    fn required(&self) -> &'static [Capability] {
        &[Capability::LibraryWrite]
    }
    fn validate(&self, payload: &Value) -> Result<(), ServiceError> {
        parse::<SearchRebuild>(payload).map(|_| ())
    }
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome {
        let index = ctx.db.search_index();
        match index.rebuild() {
            Ok(documents) => match index.verify() {
                Ok(health) if health.ok() => Outcome::Succeeded(json!({
                    "documents": documents,
                    "generation": index.generation().unwrap_or_default(),
                })),
                Ok(health) => Outcome::Fatal(
                    health
                        .detail
                        .unwrap_or_else(|| "index still inconsistent after rebuild".into()),
                ),
                Err(e) => Outcome::Retryable(e.to_string()),
            },
            Err(e) => Outcome::Retryable(e.to_string()),
        }
    }
}
