//! Built-in job handlers. Later issues register more (e.g. #18 text extraction).
use super::{JobContext, JobHandler, Outcome};
use cm_core::{
    ServiceError,
    attachments::{AttachmentError, AttachmentService, ImportAttachment},
    papers::PaperService,
    policy::Capability,
    ports::AttachmentRepository,
};
use cm_protocol::jobs::{
    JobProgress,
    kinds::{self, AttachmentImport, DiagnosticWait},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, time::Instant};

const MAX_ATTACHMENT_BYTES: u64 = 1 << 30;

fn parse<T: serde::de::DeserializeOwned>(payload: &Value) -> Result<T, ServiceError> {
    serde_json::from_value(payload.clone())
        .map_err(|e| ServiceError::InvalidInput(format!("invalid job payload: {e}")))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Idempotency: the effect key is `(paper, sha256(content))`. If an earlier (possibly
/// crashed) attempt already linked this content to the paper, the job reports it as
/// committed and writes nothing. The store itself publishes blobs before the DB row.
pub struct AttachmentImportHandler;
impl JobHandler for AttachmentImportHandler {
    fn kind(&self) -> &'static str {
        kinds::ATTACHMENT_IMPORT
    }
    fn required(&self) -> &'static [Capability] {
        &[Capability::LibraryWrite, Capability::LocalFileRead]
    }
    fn validate(&self, payload: &Value) -> Result<(), ServiceError> {
        let job: AttachmentImport = parse(payload)?;
        if !job.path.is_absolute() || job.paper.0.is_nil() {
            return Err(ServiceError::InvalidInput(
                "attachment job needs an absolute path and a paper".into(),
            ));
        }
        Ok(())
    }
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome {
        let job: AttachmentImport = match parse(ctx.payload) {
            Ok(job) => job,
            Err(e) => return Outcome::Fatal(e.to_string()),
        };
        match PaperService::new(ctx.db.papers()).get(job.paper) {
            Ok(_) => {}
            Err(ServiceError::NotFound) => return Outcome::Fatal("paper not found".into()),
            Err(e) => return Outcome::Retryable(e.to_string()),
        }
        if let Err(e) = crate::desktop_worker::files::safe(&job.path, true) {
            return Outcome::Fatal(e.to_string());
        }
        let (digest, size) = match digest(&job.path, ctx) {
            Ok(Some(value)) => value,
            Ok(None) => return Outcome::Cancelled,
            Err(e) => return Outcome::Fatal(e),
        };
        // Committed-result detection before any write.
        match ctx.db.attachments().for_paper(job.paper) {
            Ok(existing) => {
                if let Some(a) = existing.into_iter().find(|a| a.sha256 == digest) {
                    return Outcome::AlreadyCommitted(
                        json!({"attachment": a.id.0, "sha256": hex(&digest), "bytes": size}),
                    );
                }
            }
            Err(e) => return Outcome::Retryable(e.to_string()),
        }
        let name = job.original_name.unwrap_or_else(|| {
            job.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "attachment".into())
        });
        let library = match ctx.db.managed_attachments(ctx.directory) {
            Ok(library) => library,
            Err(e) => return Outcome::Retryable(e.to_string()),
        };
        let imported = AttachmentService::new(library).import(ImportAttachment {
            paper_id: job.paper,
            source: job.path,
            original_name: name,
            media_type: job
                .media_type
                .unwrap_or_else(|| "application/octet-stream".into()),
            expected_sha256: Some(digest),
        });
        match imported {
            Ok(a) => Outcome::Succeeded(
                json!({"attachment": a.id.0, "sha256": hex(&digest), "bytes": size}),
            ),
            Err(AttachmentError::HashMismatch | AttachmentError::Unavailable) => {
                Outcome::Retryable("source changed or storage busy".into())
            }
            Err(AttachmentError::Service(ServiceError::Unavailable)) => {
                Outcome::Retryable("library busy".into())
            }
            Err(e) => Outcome::Fatal(e.to_string()),
        }
    }
}

fn digest(path: &std::path::Path, ctx: &JobContext<'_>) -> Result<Option<([u8; 32], u64)>, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1 << 16];
    let mut last = Instant::now();
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(Some((hash.finalize().into(), size)));
        }
        size += n as u64;
        if size > MAX_ATTACHMENT_BYTES {
            return Err("attachment exceeds 1 GiB".into());
        }
        hash.update(&buffer[..n]);
        if last.elapsed().as_millis() >= 200 {
            last = Instant::now();
            if !ctx.progress(JobProgress {
                done: size,
                total: None,
                message: Some("hashing".into()),
            }) {
                return Ok(None);
            }
        }
    }
}

/// Diagnostic job without library effects (lifecycle/retry/lease tests and support).
pub struct DiagnosticWaitHandler;
impl JobHandler for DiagnosticWaitHandler {
    fn kind(&self) -> &'static str {
        kinds::DIAGNOSTIC_WAIT
    }
    fn required(&self) -> &'static [Capability] {
        &[]
    }
    fn validate(&self, payload: &Value) -> Result<(), ServiceError> {
        let job: DiagnosticWait = parse(payload)?;
        if job.millis > 60_000 || job.fail_attempts > 20 {
            return Err(ServiceError::InvalidInput(
                "millis must be <= 60000 and fail_attempts <= 20".into(),
            ));
        }
        Ok(())
    }
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome {
        let job: DiagnosticWait = match parse(ctx.payload) {
            Ok(job) => job,
            Err(e) => return Outcome::Fatal(e.to_string()),
        };
        if ctx.attempt <= job.fail_attempts {
            return Outcome::Retryable(format!("diagnostic failure on attempt {}", ctx.attempt));
        }
        let started = Instant::now();
        loop {
            let elapsed = started.elapsed().as_millis() as u64;
            if elapsed >= job.millis {
                return Outcome::Succeeded(json!({"waited_ms": job.millis}));
            }
            if !ctx.progress(JobProgress {
                done: elapsed,
                total: Some(job.millis),
                message: None,
            }) {
                return if ctx.cancel.is_cancelled() {
                    Outcome::Cancelled
                } else {
                    Outcome::Retryable("lease lost".into())
                };
            }
            std::thread::sleep(std::time::Duration::from_millis(
                (job.millis - elapsed).min(50),
            ));
        }
    }
}
