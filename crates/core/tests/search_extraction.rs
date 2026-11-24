//! #18 pure-core evidence: extraction orchestration (batching, resume, failure states,
//! cancellation, retryable errors, runaway guard, page limit) and snippet bounds.
use cm_core::{
    ServiceError,
    metadata_resolve::Cancellation,
    search::{
        extraction::*,
        service::snippet,
        types::{SearchRequest, SnippetRange},
    },
};
use cm_domain::{AttachmentId, PaperId};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Default)]
struct Store {
    targets: Vec<ExtractTarget>,
    stored: Vec<(AttachmentId, Vec<u32>, u32, Option<bool>)>,
    failed: Vec<(AttachmentId, ExtractionState)>,
    resets: Vec<AttachmentId>,
}
impl FulltextStore for Store {
    fn targets(
        &mut self,
        _: ExtractScope,
        _: bool,
        limit: u32,
    ) -> Result<Vec<ExtractTarget>, ServiceError> {
        Ok(self.targets.iter().take(limit as usize).cloned().collect())
    }
    fn reset(&mut self, attachment: AttachmentId) -> Result<(), ServiceError> {
        self.resets.push(attachment);
        Ok(())
    }
    fn store(&mut self, batch: StoreBatch<'_>) -> Result<ExtractionState, ServiceError> {
        self.stored.push((
            batch.target.attachment_id,
            batch.pages.iter().map(|p| p.page_index).collect(),
            batch.next_page,
            batch.finish,
        ));
        Ok(match batch.finish {
            None => ExtractionState::Extracting,
            Some(true) => ExtractionState::Partial,
            Some(false) => ExtractionState::Complete,
        })
    }
    fn fail(
        &mut self,
        target: &ExtractTarget,
        state: ExtractionState,
        _: &str,
    ) -> Result<(), ServiceError> {
        self.failed.push((target.attachment_id, state));
        Ok(())
    }
}

/// Path name selects behaviour: "pages-N", "locked", "busy", "stuck", "hash".
struct Fake(Mutex<Vec<(String, u32)>>);
impl PageTextExtractor for Fake {
    fn extract(
        &self,
        path: &Path,
        first: u32,
        max: u32,
        _: u32,
        cancel: &Cancellation,
    ) -> Result<PageBatch, ExtractError> {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        self.0.lock().unwrap().push((name.clone(), first));
        if cancel.is_cancelled() {
            return Err(ExtractError::Cancelled);
        }
        let count: u32 = match name.as_str() {
            "locked" => return Err(ExtractError::Encrypted),
            "busy" => return Err(ExtractError::Busy("worker busy".into())),
            "stuck" => {
                return Ok(PageBatch {
                    page_count: 10,
                    sha256: [1; 32],
                    pages: vec![],
                });
            }
            n => n.trim_start_matches("pages-").parse().unwrap_or(0),
        };
        Ok(PageBatch {
            page_count: count,
            sha256: if name == "hash" { [9; 32] } else { [1; 32] },
            pages: (first..(first + max).min(count))
                .map(|i| ExtractedPage {
                    page_index: i,
                    page_label: None,
                    text: "t".into(),
                    truncated: false,
                })
                .collect(),
        })
    }
}
fn target(name: &str, resume_from: u32) -> ExtractTarget {
    ExtractTarget {
        attachment_id: AttachmentId(Uuid::now_v7()),
        paper_id: PaperId(Uuid::now_v7()),
        sha256: [1; 32],
        path: PathBuf::from("/managed").join(name),
        resume_from,
        reset: false,
    }
}

#[test]
fn batches_resume_and_unreadable_documents_do_not_stop_the_queue() {
    let mut store = Store {
        targets: vec![
            target("pages-100", 0),
            target("locked", 0),
            target("pages-40", 32),
            target("stuck", 0),
            target("hash", 0),
        ],
        ..Default::default()
    };
    let fake = Fake(Mutex::new(Vec::new()));
    let summary = run(
        &mut store,
        &fake,
        ExtractScope::Pending,
        false,
        &Cancellation::default(),
        &mut |_| true,
    )
    .unwrap();
    assert_eq!(summary.attachments, 5);
    assert_eq!(
        (summary.complete, summary.unreadable, summary.pages),
        (2, 3, 108)
    );
    // 100 pages: one commit at 64 pages, the rest at the end.
    let first = &store.stored[0];
    assert_eq!((first.1.len(), first.2, first.3), (COMMIT_PAGES, 64, None));
    assert_eq!((store.stored[1].2, store.stored[1].3), (100, Some(false)));
    // Resume starts at the committed page, not at 0.
    let calls = fake.0.lock().unwrap();
    assert!(calls.contains(&("pages-40".into(), 32)));
    assert!(!calls.contains(&("pages-40".into(), 0)));
    let failures: Vec<ExtractionState> = store.failed.iter().map(|f| f.1).collect();
    assert_eq!(
        failures,
        vec![
            ExtractionState::Encrypted,
            ExtractionState::Failed,
            ExtractionState::Failed
        ]
    );
}

#[test]
fn cancellation_and_transient_errors_stop_without_marking_attachments() {
    let mut store = Store {
        targets: vec![target("pages-100", 0)],
        ..Default::default()
    };
    let fake = Fake(Mutex::new(Vec::new()));
    let mut calls = 0;
    let result = run(
        &mut store,
        &fake,
        ExtractScope::Pending,
        false,
        &Cancellation::default(),
        &mut |_| {
            calls += 1;
            calls < 6
        },
    );
    assert_eq!(result, Err(RunError::Cancelled));
    assert_eq!(store.stored.len(), 1, "the committed batch survives");
    assert!(store.failed.is_empty());
    let mut store = Store {
        targets: vec![target("busy", 0)],
        ..Default::default()
    };
    assert!(matches!(
        run(
            &mut store,
            &fake,
            ExtractScope::Pending,
            false,
            &Cancellation::default(),
            &mut |_| true
        ),
        Err(RunError::Retryable(_))
    ));
    assert!(store.failed.is_empty());
}

#[test]
fn exhausted_time_budget_commits_buffer_and_reports_more_pending() {
    let mut store = Store {
        targets: vec![target("pages-100", 0), target("pages-10", 0)],
        ..Default::default()
    };
    let fake = Fake(Mutex::new(Vec::new()));
    let summary = run_with_budget(
        &mut store,
        &fake,
        ExtractScope::Pending,
        false,
        &Cancellation::default(),
        &mut |_| true,
        std::time::Duration::ZERO,
    )
    .unwrap();
    assert!(summary.more_pending);
    assert_eq!(summary.pages, PAGES_PER_CALL);
    assert_eq!(store.stored.len(), 1);
    assert_eq!(
        (store.stored[0].2, store.stored[0].3),
        (PAGES_PER_CALL, None)
    );
    assert_eq!(
        fake.0.lock().unwrap().len(),
        1,
        "second attachment left for the next run"
    );
}

#[test]
fn page_limit_marks_partial() {
    let mut store = Store {
        targets: vec![target(
            &format!("pages-{}", MAX_PAGES_PER_DOCUMENT + 7),
            MAX_PAGES_PER_DOCUMENT - 3,
        )],
        ..Default::default()
    };
    let fake = Fake(Mutex::new(Vec::new()));
    let summary = run(
        &mut store,
        &fake,
        ExtractScope::Pending,
        false,
        &Cancellation::default(),
        &mut |_| true,
    )
    .unwrap();
    assert_eq!(summary.partial, 1);
    let last = store.stored.last().unwrap();
    assert_eq!((last.2, last.3), (MAX_PAGES_PER_DOCUMENT, Some(true)));
}

#[test]
fn snippets_are_bounded_and_highlight_every_occurrence() {
    let text = format!(
        "{} The Lithium anode.\nlithium again {}",
        "x".repeat(500),
        "y".repeat(500)
    );
    let (s, highlights, truncated) = snippet(&text, &["lithium".to_string()], 60);
    assert!(truncated);
    assert!(s.starts_with('\u{2026}') && s.ends_with('\u{2026}'));
    assert!(s.chars().count() <= 62);
    assert!(!s.contains('\n'));
    assert_eq!(highlights.len(), 2);
    for SnippetRange { start, end } in highlights {
        assert_eq!(s[start..end].to_ascii_lowercase(), "lithium");
    }
    let (s, h, truncated) = snippet("全固体電池の研究", &["電池".to_string()], 160);
    assert!(!truncated);
    assert_eq!(&s[h[0].start..h[0].end], "電池");
}

#[test]
fn request_limits_are_enforced() {
    let mut r = SearchRequest::new("x");
    assert!(r.validate().is_ok());
    for bad in [0, 101] {
        r.limit = bad;
        assert!(r.validate().is_err());
    }
    r.limit = 10;
    r.candidate_limit = 20_001;
    assert!(r.validate().is_err());
    r.candidate_limit = 10;
    r.snippet_chars = 10;
    assert!(r.validate().is_err());
    r.snippet_chars = 100;
    r.sources.bibliographic = false;
    r.sources.annotations = false;
    r.sources.pages = false;
    assert!(r.validate().is_err());
}
