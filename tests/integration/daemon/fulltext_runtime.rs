//! #18 PDF -> pinned PDFium text extraction job -> FTS search -> page jump data, over real
//! papersd IPC. Requires `CM_PDFIUM_LIBRARY` (absolute path of the pinned library).
mod support;
use cm_daemon::{fulltext::PdfiumExtractor, ipc::start};
use cm_protocol::{
    client::Session,
    domain::PaperId,
    ipc::{Command, Reply},
    jobs::{JobCommand, JobReply, JobState, JobStatus, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest, NewPaper},
    search::*,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use support::*;

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
fn jobs(session: &mut Session) -> Vec<JobStatus> {
    match ok(session.call(Command::Jobs(JobCommand::List { active_only: false }))) {
        Reply::Jobs(JobReply::Jobs(jobs)) => jobs,
        other => panic!("{other:?}"),
    }
}
fn search(session: &mut Session, request: SearchRequest) -> SearchPage {
    match ok(session.call(Command::Search(Box::new(request)))) {
        Reply::Search(SearchReply::Results(page)) => *page,
        other => panic!("{other:?}"),
    }
}
fn state(session: &mut Session, paper: PaperId) -> ExtractionState {
    match ok(session.call(Command::Fulltext(FulltextRequest::Status(paper)))) {
        Reply::Fulltext(FulltextReply::Status(s)) => s[0].state,
        other => panic!("{other:?}"),
    }
}
fn percentile(sorted: &[f64], p: usize) -> f64 {
    // Nearest rank, as in the #53 measurement.
    sorted[(sorted.len() * p).div_ceil(100).max(1) - 1]
}

#[test]
fn real_pdf_is_extracted_by_the_job_and_found_by_page() {
    let library = PathBuf::from(
        std::env::var_os("CM_PDFIUM_LIBRARY").expect("explicit pinned runtime path required"),
    );
    let env = Env::new();
    let mut config = env.config();
    config.extractor = Some(Arc::new(PdfiumExtractor::with_library(library)));
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .unwrap();
    let mut papers = Vec::new();
    let started = Instant::now();
    for (title, file) in [
        ("Corpus", "pdf/corpus.pdf"),
        ("Scanned", "library/scanned.pdf"),
        ("Encrypted", "pdf/encrypted.pdf"),
        ("Damaged", "pdf/malformed.pdf"),
    ] {
        let paper = create_paper(&mut desktop, title);
        let job = match ok(desktop.call(Command::Jobs(JobCommand::Submit(JobSubmit {
            kind: kinds::ATTACHMENT_IMPORT.into(),
            idempotency_key: file.into(),
            payload: json!({"paper": paper.0, "path": fixtures.join(file), "media_type": "application/pdf"}),
            max_attempts: None,
            deadline_at_ms: None,
        })))) {
            Reply::Jobs(JobReply::Job(job)) => job.id,
            other => panic!("{other:?}"),
        };
        assert!(wait_until(Duration::from_secs(30), || {
            jobs(&mut desktop)
                .iter()
                .any(|j| j.id == job && j.state == JobState::Succeeded)
        }));
        papers.push(paper);
    }
    assert!(wait_until(Duration::from_secs(60), || {
        let all = jobs(&mut desktop);
        all.iter().any(|j| j.kind == kinds::FULLTEXT_EXTRACT)
            && all.iter().all(|j| !j.state.is_active())
            && papers
                .iter()
                .all(|p| state(&mut desktop, *p) != ExtractionState::Pending)
    }));
    let extraction_ms = started.elapsed().as_secs_f64() * 1000.0;
    let all = jobs(&mut desktop);
    assert!(
        all.iter()
            .filter(|j| j.kind == kinds::FULLTEXT_EXTRACT)
            .all(|j| j.state == JobState::Succeeded),
        "{all:?}"
    );
    let states: Vec<ExtractionState> = papers.iter().map(|p| state(&mut desktop, *p)).collect();
    assert_eq!(
        states,
        vec![
            ExtractionState::Complete,
            ExtractionState::NoText,
            ExtractionState::Encrypted,
            ExtractionState::Malformed
        ]
    );
    let corpus = papers[0];

    // 3+ scalars: trigram + unicode61, page hits with 0-based index and PDF labels.
    let english = search(&mut desktop, SearchRequest::new("\"coordinate proof\""));
    let mut pages: Vec<(u32, Option<String>)> = english
        .hits
        .iter()
        .map(|h| {
            assert_eq!(h.paper_id, corpus);
            assert_eq!(h.source, DocumentSource::Page);
            (h.page_index.unwrap(), h.page_label.clone())
        })
        .collect();
    pages.sort();
    assert_eq!(pages[0], (0, Some("iv".into())));
    assert_eq!(pages[1], (1, Some("S12".into())));
    assert_eq!(pages.len(), 4, "{pages:?}");
    assert_eq!(
        english.coverage.unsearched,
        vec![UnsearchedScope::NoExtractableText { attachments: 3 }]
    );
    let japanese = search(&mut desktop, SearchRequest::new("日本語"));
    assert_eq!(japanese.hits.len(), 4);
    assert_eq!(japanese.coverage.scan, ScanStatus::NotNeeded);
    // A 2-scalar whole unicode61 token is still found through the token index.
    assert_eq!(search(&mut desktop, SearchRequest::new("αβ")).hits.len(), 4);
    // 2 scalars inside a CJK run: unscoped search reports PDF pages as unsearched instead
    // of zero hits...
    let unscoped = search(&mut desktop, SearchRequest::new("日本"));
    assert!(unscoped.hits.is_empty());
    assert!(
        unscoped
            .coverage
            .unsearched
            .contains(&UnsearchedScope::PdfPagesForShortTerms)
    );
    assert!(!unscoped.coverage.complete);
    // ...and an explicit scope searches the bounded page candidates.
    let mut scoped = SearchRequest::new("日本");
    scoped.papers = vec![corpus];
    let scoped_page = search(&mut desktop, scoped.clone());
    assert_eq!(scoped_page.hits.len(), 4);
    assert_eq!(scoped_page.coverage.scan, ScanStatus::Complete);

    // Small-scale latency (warm, same connection), FTS and short-term series separately.
    let mut timings = |request: SearchRequest| {
        let mut samples: Vec<f64> = (0..31)
            .map(|_| {
                let t = Instant::now();
                search(&mut desktop, request.clone());
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        (percentile(&samples, 50), percentile(&samples, 95))
    };
    let fts = timings(SearchRequest::new("日本語"));
    let short = timings(scoped);
    println!(
        "{}",
        json!({
            "extraction_wall_ms_including_import_jobs": extraction_ms,
            "ipc_search_fts_ms": {"p50": fts.0, "p95": fts.1},
            "ipc_search_short_scoped_ms": {"p50": short.0, "p95": short.1},
            "runs": 31,
        })
    );
}
