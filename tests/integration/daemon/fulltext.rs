//! #18 over real papersd IPC with a deterministic extractor: automatic extraction after
//! attachment import, page hits with labels, resumable cancel/retry, explicit per-attachment
//! failure states, rebuild job and authorization of the new commands.
mod support;
use cm_core::{
    metadata_resolve::Cancellation,
    search::extraction::{ExtractError, ExtractedPage, PageBatch, PageTextExtractor},
};
use cm_daemon::ipc::start;
use cm_protocol::{
    client::Session,
    domain::{AttachmentId, PaperId},
    ipc::{Capability, Command, Denial, Operation, Reply, WireError},
    jobs::{JobCommand, JobId, JobReply, JobState, JobStatus, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest, NewPaper},
    search::*,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::*;

/// File-content driven fake: "ENCRYPTED" -> Encrypted, "IMAGE" -> pages without text,
/// otherwise `pages` pages of text naming the file's marker word.
struct Fake {
    pages: u32,
    delay: Duration,
    calls: Mutex<Vec<u32>>,
}
impl PageTextExtractor for Fake {
    fn extract(
        &self,
        path: &Path,
        first_page: u32,
        max_pages: u32,
        _max_chars: u32,
        cancel: &Cancellation,
    ) -> Result<PageBatch, ExtractError> {
        if cancel.is_cancelled() {
            return Err(ExtractError::Cancelled);
        }
        std::thread::sleep(self.delay);
        self.calls.lock().unwrap().push(first_page);
        let bytes = std::fs::read(path).map_err(|e| ExtractError::Failed(e.to_string()))?;
        let content = String::from_utf8_lossy(&bytes).into_owned();
        if content.contains("ENCRYPTED") {
            return Err(ExtractError::Encrypted);
        }
        let marker = content.split_whitespace().last().unwrap_or("").to_owned();
        let end = (first_page + max_pages).min(self.pages);
        Ok(PageBatch {
            page_count: self.pages,
            sha256: Sha256::digest(&bytes).into(),
            pages: (first_page..end)
                .map(|i| ExtractedPage {
                    page_index: i,
                    page_label: Some(format!("L{}", i + 1)),
                    text: if content.contains("IMAGE") {
                        String::new()
                    } else {
                        format!("page {i} discusses {marker} and 電池")
                    },
                    truncated: false,
                })
                .collect(),
        })
    }
}

fn create_paper(session: &mut Session, title: &str) -> PaperId {
    match ok(
        session.call(Command::Library(LibraryRequest::Create(Box::new(
            NewPaper::titled(title),
        )))),
    ) {
        Reply::Library(LibraryReply::Paper(p)) => p.id,
        other => panic!("{other:?}"),
    }
}
fn import(env: &Env, session: &mut Session, paper: PaperId, name: &str, content: &str) {
    let path = env.temp.path().canonicalize().unwrap().join(name);
    std::fs::write(&path, content).unwrap();
    let job = match ok(session.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kinds::ATTACHMENT_IMPORT.into(),
        idempotency_key: name.into(),
        payload: json!({"paper": paper.0, "path": path, "original_name": name, "media_type": "application/pdf"}),
        max_attempts: None,
        deadline_at_ms: None,
    })))) {
        Reply::Jobs(JobReply::Job(job)) => job.id,
        other => panic!("{other:?}"),
    };
    assert_eq!(settle(session, job).state, JobState::Succeeded);
}
fn get(session: &mut Session, id: JobId) -> JobStatus {
    match ok(session.call(Command::Jobs(JobCommand::Get(id)))) {
        Reply::Jobs(JobReply::Job(job)) => *job,
        other => panic!("{other:?}"),
    }
}
fn settle(session: &mut Session, id: JobId) -> JobStatus {
    let mut last = None;
    assert!(wait_until(Duration::from_secs(30), || {
        let job = get(session, id);
        let done = !job.state.is_active();
        last = Some(job);
        done
    }));
    last.unwrap()
}
fn extraction_jobs(session: &mut Session) -> Vec<JobStatus> {
    match ok(session.call(Command::Jobs(JobCommand::List { active_only: false }))) {
        Reply::Jobs(JobReply::Jobs(jobs)) => jobs
            .into_iter()
            .filter(|j| j.kind == kinds::FULLTEXT_EXTRACT)
            .collect(),
        other => panic!("{other:?}"),
    }
}
fn settle_extraction(session: &mut Session) -> Vec<JobStatus> {
    assert!(wait_until(Duration::from_secs(30), || {
        let jobs = extraction_jobs(session);
        !jobs.is_empty() && jobs.iter().all(|j| !j.state.is_active())
    }));
    extraction_jobs(session)
}
fn status(session: &mut Session, paper: PaperId) -> Vec<AttachmentExtraction> {
    match ok(session.call(Command::Fulltext(FulltextRequest::Status(paper)))) {
        Reply::Fulltext(FulltextReply::Status(s)) => s,
        other => panic!("{other:?}"),
    }
}
fn search(session: &mut Session, request: SearchRequest) -> SearchPage {
    match ok(session.call(Command::Search(Box::new(request)))) {
        Reply::Search(SearchReply::Results(page)) => *page,
        other => panic!("{other:?}"),
    }
}

#[test]
fn imported_pdfs_are_extracted_automatically_and_searchable_by_page() {
    let env = Env::new();
    let mut config = env.config();
    let fake = Arc::new(Fake {
        pages: 3,
        delay: Duration::ZERO,
        calls: Mutex::new(Vec::new()),
    });
    config.extractor = Some(fake.clone());
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let text_paper = create_paper(&mut desktop, "Battery interfaces");
    let other = create_paper(&mut desktop, "Unrelated");
    import(
        &env,
        &mut desktop,
        text_paper,
        "text.pdf",
        "%PDF-1.4 dendrite",
    );
    import(
        &env,
        &mut desktop,
        other,
        "locked.pdf",
        "%PDF-1.4 ENCRYPTED",
    );
    import(&env, &mut desktop, other, "scan.pdf", "%PDF-1.4 IMAGE");
    let jobs = settle_extraction(&mut desktop);
    assert!(
        jobs.iter().all(|j| j.state == JobState::Succeeded),
        "{jobs:?}"
    );
    // Unreadable PDFs get explicit states without failing the queue.
    let states: Vec<ExtractionState> = status(&mut desktop, other)
        .iter()
        .map(|s| s.state)
        .collect();
    assert_eq!(
        states,
        vec![ExtractionState::Encrypted, ExtractionState::NoText]
    );
    let ok_state = &status(&mut desktop, text_paper)[0];
    assert_eq!(
        (
            ok_state.state,
            ok_state.page_count,
            ok_state.pages_with_text
        ),
        (ExtractionState::Complete, 3, 3)
    );
    // Page hits carry attachment/page index/label and a highlighted snippet.
    let page = search(&mut desktop, SearchRequest::new("dendrite"));
    assert_eq!(page.hits.len(), 3);
    let first = &page.hits[0];
    assert_eq!(first.paper_id, text_paper);
    assert_eq!(first.source, DocumentSource::Page);
    assert_eq!(
        (first.page_index, first.page_label.as_deref()),
        (Some(0), Some("L1"))
    );
    assert!(first.attachment_id.is_some());
    let h = first.highlights[0];
    assert_eq!(&first.snippet[h.start..h.end], "dendrite");
    assert_eq!(
        page.coverage.unsearched,
        vec![UnsearchedScope::NoExtractableText { attachments: 2 }]
    );
    assert!(page.coverage.complete);
    // Short Japanese term with an explicit scope searches the bounded page candidates.
    let mut short = SearchRequest::new("電池");
    short.papers = vec![text_paper];
    assert_eq!(search(&mut desktop, short).hits.len(), 3);
    // Bounded page read.
    let attachment: AttachmentId = first.attachment_id.unwrap();
    match ok(
        desktop.call(Command::Fulltext(FulltextRequest::Pages(PageTextRequest {
            attachment,
            from_page: 1,
            max_pages: 1,
            max_chars: 1000,
        }))),
    ) {
        Reply::Fulltext(FulltextReply::Pages(read)) => {
            assert_eq!(read.pages.len(), 1);
            assert_eq!(read.pages[0].page_index, 1);
            assert_eq!(read.next_page, Some(2));
        }
        other => panic!("{other:?}"),
    }
    // Syntax errors come back as structured diagnostics.
    match ok(desktop.call(Command::Search(Box::new(SearchRequest::new(
        "dendrite OR x",
    ))))) {
        Reply::Search(SearchReply::Invalid(e)) => {
            assert_eq!(e.diagnostics[0].code, "UnsupportedSyntax")
        }
        other => panic!("{other:?}"),
    }
    // Soft delete hides the paper's hits; restore brings them back.
    let lifecycle = |s: &mut Session, deleted| {
        ok(s.call(Command::Lifecycle {
            paper: text_paper,
            deleted,
        }))
    };
    lifecycle(&mut desktop, true);
    assert!(
        search(&mut desktop, SearchRequest::new("dendrite"))
            .hits
            .is_empty()
    );
    lifecycle(&mut desktop, false);
    assert_eq!(
        search(&mut desktop, SearchRequest::new("dendrite"))
            .hits
            .len(),
        3
    );
    // Explicit rebuild job keeps results identical.
    let rebuild = match ok(desktop.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kinds::SEARCH_REBUILD.into(),
        idempotency_key: "rebuild-1".into(),
        payload: json!({}),
        max_attempts: None,
        deadline_at_ms: None,
    })))) {
        Reply::Jobs(JobReply::Job(job)) => job.id,
        other => panic!("{other:?}"),
    };
    let rebuilt = settle(&mut desktop, rebuild);
    assert_eq!(rebuilt.state, JobState::Succeeded, "{rebuilt:?}");
    assert_eq!(
        search(&mut desktop, SearchRequest::new("dendrite"))
            .hits
            .len(),
        3
    );
    // Forced re-extraction of a failed attachment is an explicit job.
    let forced = match ok(desktop.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kinds::FULLTEXT_EXTRACT.into(),
        idempotency_key: "force-other".into(),
        payload: json!({"scope": {"kind": "paper", "id": other.0}, "force": true}),
        max_attempts: None,
        deadline_at_ms: None,
    })))) {
        Reply::Jobs(JobReply::Job(job)) => job.id,
        other => panic!("{other:?}"),
    };
    let forced = settle(&mut desktop, forced);
    assert_eq!(forced.state, JobState::Succeeded, "{forced:?}");
    assert_eq!(forced.result.as_ref().unwrap()["attachments"], 2);
}

#[test]
fn extraction_is_cancellable_and_resumes_from_committed_pages() {
    let env = Env::new();
    let mut config = env.config();
    let fake = Arc::new(Fake {
        pages: 320,
        delay: Duration::from_millis(40),
        calls: Mutex::new(Vec::new()),
    });
    config.extractor = Some(fake.clone());
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let owner = create_paper(&mut desktop, "Long document");
    import(&env, &mut desktop, owner, "long.pdf", "%PDF-1.4 resumable");
    assert!(wait_until(Duration::from_secs(20), || {
        status(&mut desktop, owner)
            .first()
            .is_some_and(|s| s.pages_done >= 64)
    }));
    let job = extraction_jobs(&mut desktop)[0].id;
    ok(desktop.call(Command::Jobs(JobCommand::Cancel(job))));
    let cancelled = settle(&mut desktop, job);
    assert_eq!(cancelled.state, JobState::Cancelled);
    let partial = status(&mut desktop, owner)[0].clone();
    assert_eq!(partial.state, ExtractionState::Extracting);
    assert!(
        partial.pages_done >= 64 && partial.pages_done < 320,
        "{partial:?}"
    );
    // Committed pages are already searchable while the rest is pending.
    let page = search(&mut desktop, SearchRequest::new("resumable"));
    assert_eq!(page.total_hits, partial.pages_done);
    assert!(
        page.coverage
            .unsearched
            .contains(&UnsearchedScope::PendingExtraction { attachments: 1 })
    );
    assert!(!page.coverage.complete);
    fake.calls.lock().unwrap().clear();
    ok(desktop.call(Command::Jobs(JobCommand::Retry(job))));
    let done = settle(&mut desktop, job);
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!(
        fake.calls.lock().unwrap().first().copied(),
        Some(partial.pages_done),
        "the retry resumes at the first uncommitted page"
    );
    let finished = status(&mut desktop, owner)[0].clone();
    assert_eq!(
        (finished.state, finished.pages_with_text),
        (ExtractionState::Complete, 320)
    );
    let mut all = SearchRequest::new("resumable");
    all.limit = 100;
    assert_eq!(search(&mut desktop, all).total_hits, 320);
}

#[test]
fn search_commands_are_authorized_by_capability() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    let host = issue(
        &mut desktop,
        "host",
        cm_core::policy::presets::native_host(),
    );
    let mut host = env.session("native-host", host);
    assert_eq!(
        host.call(Command::Search(Box::new(SearchRequest::new("x"))))
            .unwrap()
            .unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::FulltextSearch))
    );
    let mcp = issue(
        &mut desktop,
        "mcp",
        cm_core::policy::presets::mcp_read_only(),
    );
    let mut mcp = env.session("mcp", mcp);
    assert!(matches!(
        ok(mcp.call(Command::Search(Box::new(SearchRequest::new("x"))))),
        Reply::Search(SearchReply::Results(_))
    ));
    // Read-only MCP can read status but cannot start extraction or rebuild jobs.
    let paper = create_paper(&mut desktop, "Any");
    assert!(matches!(
        ok(mcp.call(Command::Fulltext(FulltextRequest::Status(paper)))),
        Reply::Fulltext(FulltextReply::Status(_))
    ));
    let read_only: std::collections::BTreeSet<Capability> =
        [Capability::LibraryRead, Capability::JobsControl]
            .into_iter()
            .collect();
    let submitter = issue(&mut desktop, "jobs-no-write", read_only);
    let mut submitter = env.session("cli", submitter);
    assert_eq!(
        submitter
            .call(Command::Jobs(JobCommand::Submit(JobSubmit {
                kind: kinds::SEARCH_REBUILD.into(),
                idempotency_key: "r".into(),
                payload: json!({}),
                max_attempts: None,
                deadline_at_ms: None,
            })))
            .unwrap()
            .unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::JobSubmit))
    );
    // Without an extractor the kind is not registered at all.
    assert!(matches!(
        desktop
            .call(Command::Jobs(JobCommand::Submit(JobSubmit {
                kind: kinds::FULLTEXT_EXTRACT.into(),
                idempotency_key: "e".into(),
                payload: json!({"scope": {"kind": "pending"}}),
                max_attempts: None,
                deadline_at_ms: None,
            })))
            .unwrap(),
        Err(WireError::Service(_))
    ));
}
