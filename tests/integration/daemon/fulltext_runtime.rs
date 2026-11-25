//! #18 PDF -> pinned PDFium text extraction job -> FTS search -> page jump data, over real
//! papersd IPC; #85 a real page render through the binary IPC frame. Requires
//! `CM_PDFIUM_LIBRARY` (absolute path of the pinned library).
mod support;
use cm_daemon::{fulltext::PdfiumExtractor, ipc::start};
use cm_protocol::{
    attachments::{PdfReply, RenderedPage},
    client::{Session, handshake},
    domain::PaperId,
    ipc::{ClientFrame, Command, PdfCommand, Reply, ServerFrame, frame, read_frame, write_frame},
    jobs::{JobCommand, JobReply, JobState, JobStatus, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest, NewPaper},
    search::*,
};
use serde_json::json;
use std::{
    io::Read,
    os::unix::net::UnixStream,
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
    render_travels_as_a_binary_frame(&env, &mut desktop, corpus);

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

/// #85: a real PDFium page render reaches the client intact, with its RGBA pixels as the raw
/// binary payload of a protocol-2 frame (no base64 text in the JSON part).
fn render_travels_as_a_binary_frame(env: &Env, desktop: &mut Session, paper: PaperId) {
    let attachment = match ok(desktop.call(Command::Pdf(PdfCommand::List(paper)))) {
        Reply::Pdf(PdfReply::Attachments(list)) => list[0].0.id,
        other => panic!("{other:?}"),
    };
    let render = PdfCommand::Render {
        attachment,
        page: 0,
        zoom: 1.0,
    };
    let page: Box<RenderedPage> = match ok(desktop.call(Command::Pdf(render.clone()))) {
        Reply::Pdf(PdfReply::Page(Ok(page))) => page,
        other => panic!("{other:?}"),
    };
    assert_eq!(page.page, 0);
    assert!(page.width > 100 && page.height > 100);
    assert_eq!(
        page.rgba.len(),
        page.width as usize * page.height as usize * 4
    );
    // Straight RGBA, opaque, with drawn (non-white) text pixels.
    let pixels = page.rgba.as_chunks::<4>().0;
    assert!(pixels.iter().all(|p| p[3] == 255));
    assert!(
        pixels
            .iter()
            .any(|p| p[0] < 128 && p[1] < 128 && p[2] < 128)
    );

    // The same request on a raw connection: one binary frame whose payload is the bitmap.
    let mut stream = UnixStream::connect(env.paths().socket).unwrap();
    let welcome = handshake(&mut stream, env.connector("desktop").hello().unwrap()).unwrap();
    assert_eq!(welcome.protocol_version, 2);
    let request = ClientFrame::Request {
        id: 7,
        command: Command::Pdf(render),
    };
    write_frame(&mut stream, &request, welcome.max_request_frame).unwrap();
    let mut head = [0u8; 13];
    stream.read_exact(&mut head).unwrap();
    let length = u32::from_be_bytes(head[..4].try_into().unwrap()) as usize;
    assert_eq!(head[4], frame::BINARY_TAG);
    let json_len = u32::from_be_bytes(head[5..9].try_into().unwrap()) as usize;
    assert_eq!(u32::from_be_bytes(head[9..13].try_into().unwrap()), 1);
    let mut rest = vec![0u8; length - 9];
    stream.read_exact(&mut rest).unwrap();
    let blob_len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
    assert_eq!(blob_len, page.rgba.len());
    assert_eq!(9 + 4 + json_len + blob_len, length);
    let json = std::str::from_utf8(&rest[4..4 + json_len]).unwrap();
    assert!(json.contains(r#""rgba":{"blob":0}"#), "{json}");
    assert!(json.len() < 64 * 1024, "the JSON part carries no pixels");
    assert_eq!(&rest[4 + json_len..], page.rgba.as_slice());
    // And the standard reader turns it back into the same page.
    let mut framed = head.to_vec();
    framed.extend_from_slice(&rest);
    match read_frame::<_, ServerFrame>(&mut framed.as_slice(), u32::MAX).unwrap() {
        Some(ServerFrame::Response {
            id: 7,
            result: Ok(Reply::Pdf(PdfReply::Page(Ok(again)))),
        }) => assert_eq!(again.rgba, page.rgba),
        other => panic!("{other:?}"),
    }
}
