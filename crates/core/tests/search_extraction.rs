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
    attempts: std::collections::HashMap<AttachmentId, u32>,
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
        self.attempts.remove(&batch.target.attachment_id);
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
    fn defer(&mut self, target: &ExtractTarget, _: &str) -> Result<u32, ServiceError> {
        let attempts = self.attempts.entry(target.attachment_id).or_insert(0);
        *attempts += 1;
        Ok(*attempts)
    }
}

/// Path name selects behaviour: "pages-N", "locked", "busy", "stuck", "hash", "slow"
/// (transient time-budget failure), "late-timeout" (100 pages, transient from page 64).
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
            "slow" => return Err(ExtractError::Transient("time budget".into())),
            "late-timeout" if first >= 64 => {
                return Err(ExtractError::Transient("time budget".into()));
            }
            "late-timeout" => 100,
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
            calls < 2
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

/// P2-4 regression: a batch time-budget or read error is not a permanent `Failed` state.
/// The attachment stays unfinished, other attachments continue, later runs retry it and
/// only `MAX_TRANSIENT_ATTEMPTS` consecutive transient failures mark it failed.
#[test]
fn transient_failures_are_retried_with_bounded_attempts() {
    let slow = target("slow", 0);
    let mut store = Store {
        targets: vec![slow.clone(), target("pages-3", 0)],
        ..Default::default()
    };
    let fake = Fake(Mutex::new(Vec::new()));
    let run_once = |store: &mut Store| {
        run(
            store,
            &fake,
            ExtractScope::Pending,
            false,
            &Cancellation::default(),
            &mut |_| true,
        )
        .unwrap()
    };
    let first = run_once(&mut store);
    assert_eq!(
        (first.deferred, first.attachments, first.complete),
        (1, 1, 1)
    );
    assert!(
        store.failed.is_empty(),
        "not marked failed on the first timeout"
    );
    store.targets.truncate(1);
    for attempt in 2..MAX_TRANSIENT_ATTEMPTS {
        let again = run_once(&mut store);
        assert_eq!(again.deferred, 1, "attempt {attempt} is deferred again");
        assert!(store.failed.is_empty());
    }
    let last = run_once(&mut store);
    assert_eq!((last.deferred, last.unreadable), (0, 1));
    assert_eq!(
        store.failed,
        vec![(slow.attachment_id, ExtractionState::Failed)]
    );
    assert_eq!(store.attempts[&slow.attachment_id], MAX_TRANSIENT_ATTEMPTS);
    // A transient failure mid-document first commits the pages extracted before it.
    let late = target("late-timeout", 0);
    let mut store = Store {
        targets: vec![late.clone()],
        ..Default::default()
    };
    let summary = run_once(&mut store);
    assert_eq!((summary.deferred, summary.pages), (1, 64));
    assert_eq!(store.stored.last().unwrap().2, 64, "resume point committed");
    assert_eq!(store.attempts[&late.attachment_id], 1);
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

/// P2-5 regression: the FTS phrase "solid electrolyte" matches across a PDF line break and
/// "muller" matches "Müller" (unicode61 folding); the excerpt is centred on that match and
/// highlights the original text, with offsets valid in the (control-free) snippet.
#[test]
fn snippets_find_phrases_across_line_breaks_and_folded_diacritics() {
    let text = format!(
        "{} the solid\r\nelectrolyte by Müller {}",
        "x".repeat(400),
        "y".repeat(400)
    );
    let (s, h, truncated) = snippet(
        &text,
        &["solid electrolyte".to_string(), "muller".to_string()],
        80,
    );
    assert!(truncated);
    assert_eq!(h.len(), 2, "{s:?} {h:?}");
    assert_eq!(&s[h[0].start..h[0].end], "solid  electrolyte");
    assert_eq!(&s[h[1].start..h[1].end], "Müller");
    // A combining diacritic and C1 control characters keep offsets consistent.
    let text = "Mu\u{0308}ller\u{0085}and\u{0085}Solid\u{0085}State";
    let (s, h, _) = snippet(text, &["müller".into(), "solid state".into()], 160);
    let marked: Vec<&str> = h.iter().map(|r| &s[r.start..r.end]).collect();
    assert_eq!(marked, vec!["Mu\u{0308}ller", "Solid State"]);
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
