//! #22 browser/URL capture over the real daemon: native-host grant limits, the common #33
//! preview/commit path, duplicate merge/cancel, destination tags, and the durable
//! `capture.pdf_fetch` job (SSRF-safe fetch, retry, failure without rolling back the paper,
//! SHA-256 dedupe). Fully offline: DOI lookups are disabled per request and PDFs come from a
//! loopback fixture server reached through a fixture name resolver.
mod support;
use cm_daemon::ipc::{CaptureConfig, start};
use cm_metadata::fetch_policy::{
    FetchPolicy, HostResolver,
    test_server::{Response, TestServer},
};
use cm_protocol::{
    attachments::PdfReply,
    capture::{CaptureCommand, CaptureCommit, CaptureCommitted, CapturePreview, CaptureReply},
    capture::{CaptureRequest, MetaTag},
    client::{ClientError, Session},
    domain::PaperId,
    import::{ImportAction, ImportCommit, ImportDecision, ImportDestination},
    ipc::{Capability, Command, Denial, Operation, PdfCommand, Reply, WireError},
    jobs::{JobCommand, JobId, JobState, JobStatus, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest},
    library_query::LibraryQuery,
    organization::{OrganizationRequest, TagInput},
};
use sha2::{Digest, Sha256};
use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};
use support::*;

const PDF: &[u8] = b"%PDF-1.4\n% synthetic capture fixture\n1 0 obj\n<<>>\nendobj\n%%EOF\n";

struct Fixtures;
impl HostResolver for Fixtures {
    fn resolve(&self, host: &str, _: u16) -> std::io::Result<Vec<IpAddr>> {
        match host {
            "pdf.example.org" => Ok(vec!["127.0.0.1".parse().unwrap()]),
            "private.example.org" => Ok(vec!["192.168.10.20".parse().unwrap()]),
            _ => Err(std::io::Error::other("unknown fixture host")),
        }
    }
}
fn config(env: &Env) -> cm_daemon::ipc::DaemonConfig {
    let mut config = env.config();
    config.capture = CaptureConfig {
        fetch: FetchPolicy::loopback_test_server(),
        resolver: Arc::new(Fixtures),
    };
    config
}
fn page(doi: &str, title: &str, pdf: Option<String>) -> CaptureRequest {
    let mut meta = vec![
        ("citation_title", title.to_owned()),
        ("citation_author", "Capture, Tester".into()),
        ("citation_publication_date", "2024/05/06".into()),
        ("citation_journal_title", "Journal of Captures".into()),
        ("citation_doi", doi.into()),
    ];
    if let Some(pdf) = pdf {
        meta.push(("citation_pdf_url", pdf));
    }
    CaptureRequest {
        page_url: format!("https://journal.example.org/{doi}"),
        meta: meta
            .into_iter()
            .map(|(name, content)| MetaTag {
                name: name.into(),
                content,
            })
            .collect(),
        offline: true,
        ..Default::default()
    }
}
fn denied(result: Result<Result<Reply, WireError>, ClientError>, op: Operation) {
    assert_eq!(
        result.unwrap().unwrap_err(),
        WireError::Denied(Denial::Forbidden(op))
    );
}
fn preview(session: &mut Session, request: CaptureRequest) -> CapturePreview {
    match ok(session.call(Command::Capture(CaptureCommand::Preview(Box::new(request))))) {
        Reply::Capture(CaptureReply::Preview(p)) => *p,
        other => panic!("{other:?}"),
    }
}
fn commit_call(
    session: &mut Session,
    p: &CapturePreview,
    action: ImportAction,
    pdf: Option<usize>,
) -> Result<Result<Reply, WireError>, ClientError> {
    session.call(Command::Capture(CaptureCommand::Commit(CaptureCommit {
        commit: ImportCommit {
            preview_id: p.preview.id.clone(),
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action,
            }],
        },
        pdf,
    })))
}
fn commit(
    session: &mut Session,
    p: &CapturePreview,
    action: ImportAction,
    pdf: Option<usize>,
) -> CaptureCommitted {
    match ok(commit_call(session, p, action, pdf)) {
        Reply::Capture(CaptureReply::Committed(c)) => *c,
        other => panic!("{other:?}"),
    }
}
fn pdf_job(session: &mut Session, id: JobId) -> JobStatus {
    match ok(session.call(Command::Capture(CaptureCommand::PdfJob(id)))) {
        Reply::Capture(CaptureReply::PdfJob(j)) => *j,
        other => panic!("{other:?}"),
    }
}
fn settle(session: &mut Session, id: JobId) -> JobStatus {
    let mut last = None;
    assert!(wait_until(Duration::from_secs(30), || {
        let job = pdf_job(session, id);
        let done = !job.state.is_active();
        last = Some(job);
        done
    }));
    last.unwrap()
}
fn attachments(desktop: &mut Session, paper: PaperId) -> Vec<[u8; 32]> {
    match ok(desktop.call(Command::Pdf(PdfCommand::List(paper)))) {
        Reply::Pdf(PdfReply::Attachments(list)) => {
            list.into_iter().map(|(a, _)| a.sha256).collect()
        }
        other => panic!("{other:?}"),
    }
}
fn paper_exists(desktop: &mut Session, paper: PaperId) -> bool {
    matches!(
        ok(desktop.call(Command::Library(LibraryRequest::Get(paper)))),
        Reply::Library(LibraryReply::Paper(_))
    )
}
fn capture(host: &mut Session, server: &TestServer, n: u32, path: &str) -> (PaperId, JobId) {
    let p = preview(
        host,
        page(
            &format!("10.9999/retry.{n}"),
            &format!("Retry case {n}"),
            Some(server.url("pdf.example.org", path)),
        ),
    );
    let saved = commit(host, &p, ImportAction::Create, Some(0));
    (saved.paper.unwrap(), saved.pdf_job.unwrap().id)
}
fn native_host(env: &Env, desktop: &mut Session) -> Session {
    let credential = issue(
        desktop,
        "browser host",
        cm_core::policy::presets::native_host(),
    );
    // A page/extension-forwarded client kind and scope request must not add authority.
    let mut connector = env.connector_with("desktop", credential);
    connector.requested = Some(Capability::ALL.into_iter().collect());
    let session = Session::open(&connector).unwrap();
    assert_eq!(
        session.welcome.capabilities,
        cm_core::policy::presets::native_host()
    );
    session
}

#[test]
fn native_host_capture_saves_paper_then_fetches_public_pdf() {
    let server = TestServer::start(|r| match r.path.as_str() {
        "/paper.pdf" => Response::redirect(302, "/files/paper.pdf").header("Set-Cookie", "s=1"),
        "/files/paper.pdf" => Response::pdf(PDF),
        _ => Response::new(404),
    });
    let env = Env::new();
    let config = config(&env);
    // A partial download left by a crashed daemon is removed at startup.
    let downloads = config.library.join("daemon").join("capture-downloads");
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&downloads)
            .unwrap();
    }
    std::fs::write(downloads.join("pdf-crashed.part"), b"%PDF-partial").unwrap();
    let _daemon = start(config).unwrap();
    assert!(!downloads.join("pdf-crashed.part").exists());
    let mut desktop = env.desktop();
    let mut host = native_host(&env, &mut desktop);
    let tag = match ok(
        desktop.call(Command::Organization(OrganizationRequest::CreateTag(
            TagInput::named("to-read"),
        ))),
    ) {
        Reply::Organization(o) => o.tags[0].id,
        other => panic!("{other:?}"),
    };
    let mut request = page(
        "10.9999/capture.1",
        "Captured From The Browser",
        Some(server.url("pdf.example.org", "/paper.pdf")),
    );
    request.destination = ImportDestination {
        collection: None,
        tags: vec![tag],
    };
    let p = preview(&mut host, request);
    assert_eq!(p.preview.candidates.len(), 1);
    assert_eq!(
        p.preview.candidates[0].draft.title,
        "Captured From The Browser"
    );
    assert_eq!(p.resolution.pdf_candidates.len(), 1);
    assert!(!p.resolution.provisional);
    // The native host has no general commit, no job control and cannot submit fetch jobs.
    denied(
        host.call(Command::Import(
            cm_protocol::exchange::ImportRequestUi::Commit(ImportCommit {
                preview_id: p.preview.id.clone(),
                decisions: vec![],
            }),
        )),
        Operation::ImportCommit,
    );
    let fetch_job = JobSubmit {
        kind: kinds::CAPTURE_PDF_FETCH.into(),
        idempotency_key: "direct".into(),
        payload: serde_json::json!({"paper": uuid::Uuid::now_v7(), "url": "https://example.org/a.pdf"}),
        max_attempts: None,
        deadline_at_ms: None,
    };
    denied(
        host.call(Command::Jobs(JobCommand::Submit(fetch_job.clone()))),
        Operation::JobSubmit,
    );
    // Not even the desktop can submit an arbitrary-URL fetch: it is preview-bound only.
    denied(
        desktop.call(Command::Jobs(JobCommand::Submit(fetch_job))),
        Operation::JobSubmit,
    );
    // Out-of-range PDF choice is rejected before anything is written.
    assert!(matches!(
        commit_call(&mut host, &p, ImportAction::Create, Some(1)).unwrap(),
        Err(WireError::Service(_))
    ));

    let saved = commit(&mut host, &p, ImportAction::Create, Some(0));
    let paper = saved.paper.expect("created");
    assert_eq!(saved.outcome.created, [paper]);
    let job = saved.pdf_job.expect("pdf job submitted");
    let job = settle(&mut host, job.id);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let digest: [u8; 32] = Sha256::digest(PDF).into();
    assert_eq!(attachments(&mut desktop, paper), [digest]);
    let result = job.result.unwrap();
    assert!(
        result["url"]
            .as_str()
            .unwrap()
            .ends_with("/files/paper.pdf")
    );
    // Destination tag applied in the same commit.
    match ok(desktop.call(Command::Organization(OrganizationRequest::ForPaper(paper)))) {
        Reply::Organization(o) => {
            assert_eq!(o.tags.iter().map(|t| t.id).collect::<Vec<_>>(), [tag])
        }
        other => panic!("{other:?}"),
    }
    for request in server.requests() {
        assert!(request.header("cookie").is_none());
        assert!(request.header("authorization").is_none());
        assert!(request.header("referer").is_none());
    }
    // The committed preview is consumed.
    denied(
        commit_call(&mut host, &p, ImportAction::Create, None),
        Operation::CaptureSave,
    );
}

#[test]
fn pdf_failures_keep_the_paper_and_are_retried() {
    let flaky = Arc::new(AtomicU32::new(0));
    let login_fixed = Arc::new(AtomicBool::new(false));
    let server = {
        let flaky = flaky.clone();
        let login_fixed = login_fixed.clone();
        TestServer::start(move |r| match r.path.as_str() {
            "/flaky.pdf" if flaky.fetch_add(1, Ordering::SeqCst) < 2 => Response::new(503),
            "/flaky.pdf" => Response::pdf(PDF),
            "/login.pdf" if !login_fixed.load(Ordering::SeqCst) => Response::new(200)
                .header("Content-Type", "text/html")
                .body(b"<form>sign in</form>"),
            "/login.pdf" => Response::pdf(b"%PDF-1.5\nanother synthetic pdf\n%%EOF\n"),
            "/down.pdf" => Response::new(502),
            "/to-private.pdf" => Response::redirect(302, "http://private.example.org/x.pdf"),
            _ => Response::new(404),
        })
    };
    let env = Env::new();
    let _daemon = start(config(&env)).unwrap();
    let mut desktop = env.desktop();
    let mut host = native_host(&env, &mut desktop);
    // Each capture settles before the next preview: any committed write (including job
    // bookkeeping) invalidates open #33 previews, see the capture ADR.
    let flaky_case = capture(&mut host, &server, 1, "/flaky.pdf");
    let flaky_job = settle(&mut host, flaky_case.1);
    let login_case = capture(&mut host, &server, 2, "/login.pdf");
    let login_job = settle(&mut host, login_case.1);
    let down_case = capture(&mut host, &server, 3, "/down.pdf");
    let down_job = settle(&mut host, down_case.1);
    let private_case = capture(&mut host, &server, 4, "/to-private.pdf");
    let private_job = settle(&mut host, private_case.1);

    // Transient 503s: automatic bounded retries, then success.
    let job = flaky_job;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(job.attempts, 3);
    assert_eq!(attachments(&mut desktop, flaky_case.0).len(), 1);

    // A login page instead of a PDF: fails at once with guidance; the paper stays saved.
    let job = login_job;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.attempts, 1);
    assert!(
        job.error
            .as_deref()
            .unwrap()
            .contains("download the PDF in your browser")
    );
    assert!(paper_exists(&mut desktop, login_case.0));
    assert!(attachments(&mut desktop, login_case.0).is_empty());
    // Explicit retry by the same native-host grant after the source became public.
    login_fixed.store(true, Ordering::SeqCst);
    match ok(host.call(Command::Capture(CaptureCommand::RetryPdf(login_case.1)))) {
        Reply::Capture(CaptureReply::PdfJob(j)) => assert_eq!(j.state, JobState::Queued),
        other => panic!("{other:?}"),
    }
    assert_eq!(settle(&mut host, login_case.1).state, JobState::Succeeded);
    assert_eq!(attachments(&mut desktop, login_case.0).len(), 1);

    // Persistent 5xx: attempt limit reached, paper retained.
    let job = down_job;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.attempts, 4);
    assert!(
        job.error
            .as_deref()
            .unwrap()
            .starts_with("attempt limit 4 reached")
    );
    assert!(paper_exists(&mut desktop, down_case.0));

    // A redirect to a name that resolves privately is blocked (no retry).
    let job = private_job;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.attempts, 1);
    assert!(
        job.error
            .as_deref()
            .unwrap()
            .contains("blocked by fetch policy")
    );
    assert!(paper_exists(&mut desktop, private_case.0));
    assert!(!server.requests().iter().any(|r| r.path == "/x.pdf"));
}

#[test]
fn duplicates_merge_cancel_and_preview_binding() {
    let server = TestServer::start(|r| match r.path.as_str() {
        "/a.pdf" | "/mirror/a.pdf" => Response::pdf(PDF),
        _ => Response::new(404),
    });
    let env = Env::new();
    let _daemon = start(config(&env)).unwrap();
    let mut desktop = env.desktop();
    let mut host = native_host(&env, &mut desktop);
    let mut other_host = native_host(&env, &mut desktop);
    let doi = "10.9999/dup.1";
    let first = preview(&mut desktop, page(doi, "Original title", None));
    let existing = commit(&mut desktop, &first, ImportAction::Create, None)
        .paper
        .unwrap();

    // Native host: the duplicate is hidden (no LibraryRead) and Merge needs the general
    // write grant. (Create on it is skipped: `hidden_exact_duplicates_are_skipped_on_commit`.)
    let dup = preview(
        &mut host,
        page(
            doi,
            "Original title",
            Some(server.url("pdf.example.org", "/a.pdf")),
        ),
    );
    assert!(dup.preview.candidates[0].matches.is_empty());
    assert!(dup.preview.warnings.iter().any(|w| w.contains("hidden")));
    assert_eq!(dup.hidden_duplicates, 1);
    denied(
        commit_call(
            &mut host,
            &dup,
            ImportAction::Merge {
                match_index: 0,
                fields: vec![],
            },
            Some(0),
        ),
        Operation::ImportCommit,
    );
    // Another grant can neither commit nor cancel this preview.
    denied(
        commit_call(&mut other_host, &dup, ImportAction::Skip, None),
        Operation::CaptureSave,
    );
    denied(
        other_host.call(Command::Capture(CaptureCommand::Cancel(
            dup.preview.id.clone(),
        ))),
        Operation::CaptureCancel,
    );
    // Cancel by the owner; the preview is gone afterwards.
    assert!(matches!(
        ok(host.call(Command::Capture(CaptureCommand::Cancel(
            dup.preview.id.clone()
        )))),
        Reply::Capture(CaptureReply::Cancelled)
    ));
    denied(
        commit_call(&mut host, &dup, ImportAction::Skip, None),
        Operation::CaptureSave,
    );

    // Desktop (LibraryRead + LibraryWrite) sees the exact match and merges with the PDF.
    let mut request = page(
        doi,
        "Original title",
        Some(server.url("pdf.example.org", "/a.pdf")),
    );
    request.meta.push(MetaTag {
        name: "citation_volume".into(),
        content: "42".into(),
    });
    let merge = preview(&mut desktop, request);
    let candidate = &merge.preview.candidates[0];
    assert_eq!(candidate.matches[0].existing.id.0, existing.0);
    assert!(
        candidate.matches[0]
            .differences
            .iter()
            .any(|d| d.field == "volume")
    );
    let merged = commit(
        &mut desktop,
        &merge,
        ImportAction::Merge {
            match_index: 0,
            fields: vec!["volume".into()],
        },
        Some(0),
    );
    assert_eq!(merged.outcome.merged, [existing]);
    assert_eq!(merged.paper, Some(existing));
    let job = settle(&mut desktop, merged.pdf_job.unwrap().id);
    assert_eq!(job.state, JobState::Succeeded);
    assert!(!job.detected_committed);
    // Same bytes from another URL: SHA-256 dedupe, no second attachment.
    let again = preview(
        &mut desktop,
        page(
            doi,
            "Original title",
            Some(server.url("pdf.example.org", "/mirror/a.pdf")),
        ),
    );
    let merged = commit(
        &mut desktop,
        &again,
        ImportAction::Merge {
            match_index: 0,
            fields: vec![],
        },
        Some(0),
    );
    let job = settle(&mut desktop, merged.pdf_job.unwrap().id);
    assert_eq!(job.state, JobState::Succeeded);
    assert!(job.detected_committed);
    assert_eq!(attachments(&mut desktop, existing).len(), 1);
    // Another grant cannot read this grant's PDF job.
    assert_eq!(
        host.call(Command::Capture(CaptureCommand::PdfJob(job.id)))
            .unwrap()
            .unwrap_err(),
        WireError::Service(cm_protocol::library::ServiceError::NotFound)
    );
}

fn paper_count(desktop: &mut Session) -> usize {
    match ok(desktop.call(Command::Query(LibraryQuery::default()))) {
        Reply::Page(page) => page.papers.len(),
        other => panic!("{other:?}"),
    }
}

/// #27 x #22: a capture grant cannot see existing papers, so an exact duplicate hidden in its
/// preview is skipped on commit (as the redaction warning says), reported as a skipped row
/// rather than a conflict, and nothing is created: no paper, no PDF job.
#[test]
fn hidden_exact_duplicates_are_skipped_on_commit() {
    let server = TestServer::start(|r| match r.path.as_str() {
        "/a.pdf" => Response::pdf(PDF),
        _ => Response::new(404),
    });
    let env = Env::new();
    let _daemon = start(config(&env)).unwrap();
    let mut desktop = env.desktop();
    let mut host = native_host(&env, &mut desktop);
    assert!(!host.welcome.capabilities.contains(&Capability::LibraryRead));
    assert!(
        !host
            .welcome
            .capabilities
            .contains(&Capability::LibraryWrite)
    );
    let doi = "10.9999/hidden.1";
    let first = preview(&mut desktop, page(doi, "Already saved", None));
    let existing = commit(&mut desktop, &first, ImportAction::Create, None)
        .paper
        .unwrap();
    assert_eq!(paper_count(&mut desktop), 1);

    // A new page: nothing hidden, Create creates.
    let fresh = preview(&mut host, page("10.9999/hidden.2", "New paper", None));
    assert_eq!(fresh.hidden_duplicates, 0);
    assert!(
        fresh
            .preview
            .warnings
            .iter()
            .any(|w| w == cm_daemon::ipc::REDACTED_PREVIEW_WARNING)
    );
    let created = commit(&mut host, &fresh, ImportAction::Create, None);
    assert_eq!(created.outcome.created.len(), 1);
    assert_eq!(paper_count(&mut desktop), 2);

    // The same page again: the match is hidden, but the preview says one row duplicates an
    // existing paper, with the same constant warning text.
    let dup = preview(
        &mut host,
        page(
            doi,
            "Already saved",
            Some(server.url("pdf.example.org", "/a.pdf")),
        ),
    );
    assert_eq!(dup.hidden_duplicates, 1);
    let candidate = &dup.preview.candidates[0];
    assert!(candidate.matches.is_empty());
    assert!(candidate.existing_match.is_none());
    assert_eq!(dup.preview.warnings, fresh.preview.warnings);
    // Create (and a PDF) on it is not a conflict: the row is skipped.
    let skipped = commit(&mut host, &dup, ImportAction::Create, Some(0));
    assert!(skipped.outcome.created.is_empty(), "{skipped:?}");
    assert!(skipped.outcome.merged.is_empty());
    assert_eq!(skipped.outcome.skipped, 1);
    assert_eq!(skipped.paper, None);
    assert!(skipped.pdf_job.is_none());
    assert!(skipped.pdf_error.is_none());
    // Nothing was created and the existing paper has no attachment.
    assert_eq!(paper_count(&mut desktop), 2);
    assert!(attachments(&mut desktop, existing).is_empty());
    // The preview was consumed like any commit.
    denied(
        commit_call(&mut host, &dup, ImportAction::Create, None),
        Operation::CaptureSave,
    );
    // Still Create/Skip only: Merge stays refused for the capture grant.
    let again = preview(&mut host, page(doi, "Already saved", None));
    denied(
        commit_call(
            &mut host,
            &again,
            ImportAction::Merge {
                match_index: 0,
                fields: vec![],
            },
            None,
        ),
        Operation::ImportCommit,
    );

    // The owner sees the match itself: nothing hidden, nothing skipped behind its back.
    let owner = preview(&mut desktop, page(doi, "Already saved", None));
    assert_eq!(owner.hidden_duplicates, 0);
    assert_eq!(owner.preview.candidates[0].matches[0].existing.id, existing);
    assert!(
        !owner
            .preview
            .warnings
            .iter()
            .any(|w| w == cm_daemon::ipc::REDACTED_PREVIEW_WARNING)
    );
}

/// One page of fixed text for any PDF (no PDFium on this lane).
struct OnePage;
impl cm_core::search::extraction::PageTextExtractor for OnePage {
    fn extract(
        &self,
        path: &std::path::Path,
        first_page: u32,
        _max_pages: u32,
        _max_chars: u32,
        _cancel: &cm_core::metadata_resolve::Cancellation,
    ) -> Result<cm_core::search::extraction::PageBatch, cm_core::search::extraction::ExtractError>
    {
        use cm_core::search::extraction::{ExtractError, ExtractedPage, PageBatch};
        let bytes = std::fs::read(path).map_err(|e| ExtractError::Failed(e.to_string()))?;
        Ok(PageBatch {
            page_count: 1,
            sha256: Sha256::digest(&bytes).into(),
            pages: (first_page..1)
                .map(|i| ExtractedPage {
                    page_index: i,
                    page_label: None,
                    text: "capturedfulltextmarker".into(),
                    truncated: false,
                })
                .collect(),
        })
    }
}

#[test]
fn captured_pdf_is_extracted_into_the_fulltext_index() {
    use cm_protocol::{
        jobs::JobReply,
        search::{ExtractionState, FulltextReply, FulltextRequest},
    };
    let server = TestServer::start(|r| match r.path.as_str() {
        "/indexed.pdf" => Response::pdf(PDF),
        _ => Response::new(404),
    });
    let env = Env::new();
    let mut config = config(&env);
    config.extractor = Some(Arc::new(OnePage));
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let mut host = native_host(&env, &mut desktop);
    let (paper, job) = capture(&mut host, &server, 7, "/indexed.pdf");
    assert_eq!(settle(&mut host, job).state, JobState::Succeeded);
    // The finished capture.pdf_fetch job submits the automatic #18 extraction like an
    // attachment import does; the native host cannot see that ownerless job.
    let extracted = |desktop: &mut Session| {
        let jobs = match ok(desktop.call(Command::Jobs(JobCommand::List { active_only: false }))) {
            Reply::Jobs(JobReply::Jobs(jobs)) => jobs,
            other => panic!("{other:?}"),
        };
        let states = match ok(desktop.call(Command::Fulltext(FulltextRequest::Status(paper)))) {
            Reply::Fulltext(FulltextReply::Status(s)) => s,
            other => panic!("{other:?}"),
        };
        jobs.iter().any(|j| j.kind == kinds::FULLTEXT_EXTRACT)
            && states.len() == 1
            && states[0].state == ExtractionState::Complete
    };
    assert!(wait_until(Duration::from_secs(30), || extracted(
        &mut desktop
    )));
}

/// The native host drops its session right after `capture.preview`. While that preview
/// awaits the user's decision papersd must not idle out (a fresh daemon would not own the
/// preview and deny the commit); once it is committed the daemon idles out as usual.
#[test]
fn an_outstanding_capture_preview_keeps_papersd_alive() {
    let env = Env::new();
    let mut config = config(&env);
    config.idle_timeout = Duration::from_millis(300);
    let daemon = start(config).unwrap();
    let credential = issue(
        &mut env.desktop(),
        "browser host",
        cm_core::policy::presets::native_host(),
    );
    let p = preview(
        &mut env.session("desktop", credential.clone()),
        page("10.9999/idle.1", "Reviewed Slowly", None),
    );
    // No connection and no job left, only the outstanding preview.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        env.paths().socket.exists(),
        "papersd idled out while a capture preview awaited a decision"
    );
    let mut host = env.session("desktop", credential);
    let saved = commit(&mut host, &p, ImportAction::Create, None);
    assert!(saved.paper.is_some());
    drop(host);
    assert!(wait_until(Duration::from_secs(10), || !env
        .paths()
        .socket
        .exists()));
    daemon.wait();
}
