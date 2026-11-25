//! #22 `capture.pdf_fetch`: download one public PDF chosen from a committed capture preview
//! and attach it to the saved paper. The paper is already committed; a failed download
//! never rolls it back. Transient failures (network, timeout, 408/429/5xx) are retried with
//! the engine's bounded backoff; policy/authentication/type failures fail at once with a
//! reason and can be retried explicitly. Re-runs are idempotent on (paper, sha256).
use super::{JobContext, JobHandler, Outcome};
use cm_core::{
    ServiceError,
    attachments::{AttachmentError, AttachmentService, ImportAttachment},
    papers::PaperService,
    policy::Capability,
    ports::AttachmentRepository,
};
use cm_metadata::fetch_policy::{FetchError, PdfFetcher};
use cm_protocol::jobs::{
    JobProgress,
    kinds::{self, CapturePdfFetch},
};
use serde_json::{Value, json};
use std::{
    io::Write,
    time::{Duration, Instant},
};

/// Directory (inside the private `<library>/daemon/`) for in-flight downloads.
pub const DOWNLOAD_DIR: &str = "capture-downloads";

/// Removes partial downloads (`pdf-*.part`) left by a daemon that stopped mid-fetch. Called
/// at startup while holding the library lock, before any job runs; jobs re-download.
/// Only regular files with that name pattern are removed. Returns how many were removed.
pub fn sweep_stale_downloads(library: &std::path::Path) -> usize {
    let dir = library.join("daemon").join(DOWNLOAD_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let partial = name.starts_with("pdf-") && name.ends_with(".part");
        let regular = entry
            .path()
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_file());
        if partial && regular && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub struct CapturePdfFetchHandler {
    fetcher: PdfFetcher,
}
impl CapturePdfFetchHandler {
    pub fn new(fetcher: PdfFetcher) -> Self {
        Self { fetcher }
    }
}

fn parse(payload: &Value) -> Result<CapturePdfFetch, ServiceError> {
    serde_json::from_value(payload.clone())
        .map_err(|e| ServiceError::InvalidInput(format!("invalid job payload: {e}")))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// A display name from the final URL's last segment; never used as a path.
fn file_name(url: &str) -> String {
    let segment = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    let safe = !segment.is_empty()
        && segment.len() <= 128
        && !segment.starts_with('.')
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    let mut name = if safe {
        segment.to_owned()
    } else {
        "capture".into()
    };
    if !name.to_ascii_lowercase().ends_with(".pdf") {
        name.push_str(".pdf");
    }
    name
}

impl JobHandler for CapturePdfFetchHandler {
    fn kind(&self) -> &'static str {
        kinds::CAPTURE_PDF_FETCH
    }
    fn required(&self) -> &'static [Capability] {
        &[Capability::CaptureCommit]
    }
    /// Only the capture commit (bound to a reviewed preview and one of its offered PDF
    /// candidates) may create this job; a client cannot submit arbitrary URLs.
    fn ipc_submittable(&self) -> bool {
        false
    }
    fn validate(&self, payload: &Value) -> Result<(), ServiceError> {
        let job = parse(payload)?;
        if job.paper.0.is_nil() {
            return Err(ServiceError::InvalidInput("PDF job needs a paper".into()));
        }
        self.fetcher
            .policy()
            .check_url(&job.url)
            .map(|_| ())
            .map_err(|e| ServiceError::InvalidInput(e.to_string()))
    }
    fn max_run(&self) -> Duration {
        self.fetcher.policy().total_timeout + Duration::from_secs(60)
    }
    fn run(&self, ctx: &mut JobContext<'_>) -> Outcome {
        let job = match parse(ctx.payload) {
            Ok(job) => job,
            Err(e) => return Outcome::Fatal(e.to_string()),
        };
        match PaperService::new(ctx.db.papers()).get(job.paper) {
            Ok(p) if p.deleted_at.is_none() => {}
            Ok(_) => return Outcome::Fatal("paper is in the trash".into()),
            Err(ServiceError::NotFound) => return Outcome::Fatal("paper not found".into()),
            Err(e) => return Outcome::Retryable(e.to_string()),
        }
        let dir = ctx.directory.path().join("daemon").join(DOWNLOAD_DIR);
        if let Err(e) = crate::lifecycle::prepare_private_dir(&dir) {
            return Outcome::Retryable(format!("download directory: {e}"));
        }
        let mut temp = match tempfile::Builder::new()
            .prefix("pdf-")
            .suffix(".part")
            .tempfile_in(&dir)
        {
            Ok(t) => t,
            Err(e) => return Outcome::Retryable(format!("download file: {e}")),
        };
        let cancel = ctx.cancel;
        let mut last = Instant::now();
        let fetched = {
            let ctx = &*ctx;
            let mut progress = |bytes: u64| {
                if last.elapsed() < Duration::from_millis(200) {
                    return !cancel.is_cancelled();
                }
                last = Instant::now();
                ctx.progress(JobProgress {
                    done: bytes,
                    total: None,
                    message: Some("downloading PDF".into()),
                })
            };
            self.fetcher
                .fetch(&job.url, temp.as_file_mut(), cancel, &mut progress)
        };
        let fetched = match fetched {
            Ok(f) => f,
            Err(FetchError::Cancelled) if cancel.is_cancelled() => return Outcome::Cancelled,
            Err(FetchError::Cancelled) => return Outcome::Retryable("lease lost".into()),
            Err(e) if e.retryable() => return Outcome::Retryable(e.to_string()),
            Err(e) => match e.guidance() {
                Some(guidance) => return Outcome::Fatal(format!("{e}. {guidance}")),
                None => return Outcome::Fatal(e.to_string()),
            },
        };
        if let Err(e) = temp
            .as_file_mut()
            .flush()
            .and_then(|()| temp.as_file().sync_all())
        {
            return Outcome::Retryable(format!("download file: {e}"));
        }
        let result = |attachment: uuid::Uuid| {
            json!({
                "attachment": attachment,
                "sha256": hex(&fetched.sha256),
                "bytes": fetched.bytes,
                "url": fetched.final_url,
                "redirects": fetched.redirects,
            })
        };
        // SHA-256 dedupe / committed-result detection before any write.
        match ctx.db.attachments().for_paper(job.paper) {
            Ok(existing) => {
                if let Some(a) = existing.into_iter().find(|a| a.sha256 == fetched.sha256) {
                    return Outcome::AlreadyCommitted(result(a.id.0));
                }
            }
            Err(e) => return Outcome::Retryable(e.to_string()),
        }
        let library = match ctx.db.managed_attachments(ctx.directory) {
            Ok(library) => library,
            Err(e) => return Outcome::Retryable(e.to_string()),
        };
        let imported = AttachmentService::new(library).import(ImportAttachment {
            paper_id: job.paper,
            source: temp.path().to_path_buf(),
            original_name: file_name(&fetched.final_url),
            media_type: "application/pdf".into(),
            expected_sha256: Some(fetched.sha256),
        });
        match imported {
            Ok(a) => Outcome::Succeeded(result(a.id.0)),
            Err(AttachmentError::HashMismatch | AttachmentError::Unavailable) => {
                Outcome::Retryable("download changed or storage busy".into())
            }
            Err(AttachmentError::Service(ServiceError::Unavailable)) => {
                Outcome::Retryable("library busy".into())
            }
            Err(AttachmentError::Service(ServiceError::NotFound)) => {
                Outcome::Fatal("paper not found or in the trash".into())
            }
            Err(e) => Outcome::Fatal(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    /// Regression (#22 review P3): partial downloads from a crash are cleaned at startup.
    #[test]
    fn startup_sweep_removes_only_partial_downloads() {
        let library = tempfile::tempdir().unwrap();
        let dir = library.path().join("daemon").join(super::DOWNLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pdf-abc.part"), b"%PDF-partial").unwrap();
        std::fs::write(dir.join("pdf-def.part"), b"").unwrap();
        std::fs::write(dir.join("keep.txt"), b"x").unwrap();
        std::fs::create_dir(dir.join("pdf-dir.part")).unwrap();
        let outside = library.path().join("outside.part");
        std::fs::write(&outside, b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join("pdf-link.part")).unwrap();
        assert_eq!(super::sweep_stale_downloads(library.path()), 2);
        assert!(!dir.join("pdf-abc.part").exists());
        assert!(dir.join("keep.txt").exists());
        assert!(dir.join("pdf-dir.part").is_dir());
        assert!(outside.exists());
        // Missing directory is fine.
        assert_eq!(
            super::sweep_stale_downloads(&library.path().join("none")),
            0
        );
    }
    #[test]
    fn display_names_are_inert() {
        assert_eq!(
            super::file_name("https://e.org/a/paper.pdf?x=1"),
            "paper.pdf"
        );
        assert_eq!(
            super::file_name("https://e.org/a/2401.01234v2"),
            "2401.01234v2.pdf"
        );
        assert_eq!(
            super::file_name("https://e.org/a/..%2F..%2Fetc"),
            "capture.pdf"
        );
        assert_eq!(super::file_name("https://e.org/"), "capture.pdf");
        assert_eq!(super::file_name("https://e.org/.hidden.pdf"), "capture.pdf");
    }
}
