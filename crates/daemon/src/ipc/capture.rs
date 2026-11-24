//! #22 capture commands. Authorization (`Command::operation`) already passed; this layer
//! adds the preview binding: a capture preview can only be committed or cancelled by the
//! grant that created it, its PDF can only be one of the candidates that preview offered,
//! and without LibraryWrite only Create/Skip decisions are accepted (no merge into an
//! existing paper the client cannot even read).
use super::{Session, auth::now_ms, redact_matches};
use cm_core::{
    ServiceError,
    policy::{self, Capability, CapabilitySet, Denial, GrantId, Operation},
};
use cm_protocol::{
    capture::{CaptureCommand, CaptureCommit, CaptureCommitted, CapturePreview, CaptureReply},
    exchange::{ImportReply, ImportRequestUi},
    import::{ImportAction, PreviewId},
    ipc::{Reply, WireError},
    jobs::{JobId, JobStatus, JobSubmit, kinds},
    library::LibraryClient,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Longer than the coordinator's maximum preview lifetime (300 s).
const BINDING_LIFETIME: Duration = Duration::from_secs(330);
const MAX_BINDINGS: usize = 64;
/// Automatic attempts for a capture PDF fetch (network/5xx/429/timeout only).
pub const PDF_ATTEMPTS: u32 = 4;
/// A capture PDF job that still has not succeeded after a day fails instead of retrying.
const PDF_DEADLINE_MS: i64 = 24 * 60 * 60 * 1000;

/// Capture preview -> creating grant and the PDF URLs that preview offered.
pub(super) struct Binding {
    grant: GrantId,
    pdfs: Vec<String>,
    created: Instant,
}
#[derive(Default)]
pub(super) struct Bindings(HashMap<PreviewId, Binding>);
impl Bindings {
    fn insert(&mut self, id: PreviewId, binding: Binding) {
        self.0.retain(|_, b| b.created.elapsed() < BINDING_LIFETIME);
        if self.0.len() >= MAX_BINDINGS
            && let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, b)| b.created)
                .map(|(k, _)| k.clone())
        {
            self.0.remove(&oldest);
        }
        self.0.insert(id, binding);
    }
    /// The binding when `grant` created it; otherwise None (existence is not revealed).
    fn owned(&self, id: &PreviewId, grant: GrantId) -> Option<&Binding> {
        self.0
            .get(id)
            .filter(|b| b.grant == grant && b.created.elapsed() < BINDING_LIFETIME)
    }
}

fn idempotency_key(paper: uuid::Uuid, url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(url.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("capture-pdf:{paper}:{digest}")
}

impl Session {
    pub(super) fn capture(
        &self,
        id: u64,
        command: CaptureCommand,
        held: CapabilitySet,
        done: impl FnOnce(Result<Reply, WireError>) + Send + 'static,
    ) -> Result<(), ServiceError> {
        let denied = |op| Err(WireError::Denied(Denial::Forbidden(op)));
        let grant = self.grant;
        let shared = self.shared.clone();
        match command {
            CaptureCommand::Preview(request) => {
                if let Err(e) = request.validate() {
                    done(Err(e.into()));
                    return Ok(());
                }
                let redact = !held.contains(&Capability::LibraryRead);
                self.engine.capture_preview(
                    *request,
                    shared.capture.fetch.clone(),
                    self.cancellation(id),
                    Box::new(move |result| {
                        done(
                            result
                                .map(|(mut preview, resolution)| {
                                    shared
                                        .previews
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .insert(preview.id.clone(), grant);
                                    shared
                                        .captures
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .insert(
                                            preview.id.clone(),
                                            Binding {
                                                grant,
                                                pdfs: resolution
                                                    .pdf_candidates
                                                    .iter()
                                                    .map(|c| c.url.clone())
                                                    .collect(),
                                                created: Instant::now(),
                                            },
                                        );
                                    if redact {
                                        redact_matches(&mut preview);
                                    }
                                    Reply::Capture(CaptureReply::Preview(Box::new(
                                        CapturePreview {
                                            preview,
                                            resolution,
                                        },
                                    )))
                                })
                                .map_err(Into::into),
                        )
                    }),
                )
            }
            CaptureCommand::Commit(CaptureCommit { commit, pdf }) => {
                let url = {
                    let captures = shared.captures.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(binding) = captures.owned(&commit.preview_id, grant) else {
                        done(denied(Operation::CaptureSave));
                        return Ok(());
                    };
                    match pdf {
                        None => None,
                        Some(i) => match binding.pdfs.get(i) {
                            Some(url) => Some(url.clone()),
                            None => {
                                done(Err(WireError::Service(ServiceError::InvalidInput(
                                    "PDF index is not one of the preview's candidates".into(),
                                ))));
                                return Ok(());
                            }
                        },
                    }
                };
                // Merging rewrites an existing paper: that needs the general write grant.
                if !held.contains(&Capability::LibraryWrite)
                    && commit
                        .decisions
                        .iter()
                        .any(|d| matches!(d.action, ImportAction::Merge { .. }))
                {
                    done(denied(Operation::ImportCommit));
                    return Ok(());
                }
                let preview_id = commit.preview_id.clone();
                self.engine.import(
                    ImportRequestUi::Commit(commit),
                    Box::new(move |result| {
                        let outcome = match result {
                            Ok(ImportReply::Committed(outcome)) => outcome,
                            Ok(_) => return done(Err(WireError::Unavailable)),
                            // Validation errors keep the preview (and binding) for a retry.
                            Err(e) => return done(Err(e.into())),
                        };
                        shared
                            .captures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .0
                            .remove(&preview_id);
                        shared
                            .previews
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&preview_id);
                        let paper = outcome.created.first().or(outcome.merged.first()).copied();
                        let mut pdf_job = None;
                        let mut pdf_error = None;
                        if let (Some(paper), Some(url)) = (paper, url) {
                            // The paper is committed; the PDF is a separate durable job.
                            let submitted = shared.jobs.submit_internal(
                                grant,
                                &held,
                                JobSubmit {
                                    kind: kinds::CAPTURE_PDF_FETCH.into(),
                                    idempotency_key: idempotency_key(paper.0, &url),
                                    payload: serde_json::to_value(kinds::CapturePdfFetch {
                                        paper,
                                        url,
                                    })
                                    .unwrap_or_default(),
                                    max_attempts: Some(PDF_ATTEMPTS),
                                    deadline_at_ms: Some(now_ms() + PDF_DEADLINE_MS),
                                },
                            );
                            match submitted {
                                Ok(job) => pdf_job = Some(job),
                                Err(e) => pdf_error = Some(format!("{e:?}")),
                            }
                        }
                        done(Ok(Reply::Capture(CaptureReply::Committed(Box::new(
                            CaptureCommitted {
                                outcome,
                                paper,
                                pdf_job,
                                pdf_error,
                            },
                        )))))
                    }),
                )
            }
            CaptureCommand::Cancel(preview) => {
                let owned = shared
                    .captures
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .owned(&preview, grant)
                    .is_some();
                if !owned {
                    done(denied(Operation::CaptureCancel));
                    return Ok(());
                }
                self.engine.import(
                    ImportRequestUi::Cancel(preview.clone()),
                    Box::new(move |result| {
                        shared
                            .captures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .0
                            .remove(&preview);
                        shared
                            .previews
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&preview);
                        done(
                            result
                                .map(|_| Reply::Capture(CaptureReply::Cancelled))
                                .map_err(Into::into),
                        )
                    }),
                )
            }
            CaptureCommand::PdfJob(job) => {
                done(
                    self.capture_job(job, &held)
                        .map(|s| Reply::Capture(CaptureReply::PdfJob(Box::new(s)))),
                );
                Ok(())
            }
            CaptureCommand::RetryPdf(job) => {
                let owner = (!policy::authorizer::sees_all_owners(&held)).then_some(grant);
                let result = self.capture_job(job, &held).and_then(|_| {
                    shared
                        .jobs
                        .retry(job, owner, &held)
                        .map(|s| Reply::Capture(CaptureReply::PdfJob(Box::new(s))))
                });
                done(result);
                Ok(())
            }
            _ => {
                done(Err(WireError::Service(ServiceError::InvalidInput(
                    "unsupported capture command".into(),
                ))));
                Ok(())
            }
        }
    }

    /// This grant's (or, for GrantAdmin, any) capture PDF job; other kinds are NotFound.
    fn capture_job(&self, job: JobId, held: &CapabilitySet) -> Result<JobStatus, WireError> {
        let owner = (!policy::authorizer::sees_all_owners(held)).then_some(self.grant);
        self.shared.jobs.get(job, owner).and_then(|s| {
            if s.kind == kinds::CAPTURE_PDF_FETCH {
                Ok(s)
            } else {
                Err(WireError::Service(ServiceError::NotFound))
            }
        })
    }
}
