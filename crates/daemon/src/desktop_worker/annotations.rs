//! #19 annotation requests on the library worker (single writer).
//!
//! `Propose` reads the replacement PDF page by page. Only the row lookups run on the library
//! worker (`begin_propose`); checksum verification and the page reads run on a helper thread
//! (`PendingProposal::finish`) under an overall deadline and the caller's cancellation, so a
//! long PDF never blocks library reads and writes.
use super::{Worker, files::invalid};
use cm_core::{
    annotations::{AnchorSearch, AnnotationService, Edit, PageText},
    geometry::Quad,
};
use cm_protocol::{
    annotations::*, attachments::Cancellation, domain::Attachment, library::ServiceError,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// Re-anchoring refuses PDFs with more pages than this (explicitly, not by truncating).
pub(super) const MAX_PROPOSAL_PAGES: u32 = 2000;
/// Upper bound for one whole search, whatever the page count.
pub(super) const PROPOSAL_DEADLINE: Duration = Duration::from_secs(120);

/// Text-only snapshot of one page (no bitmap is rendered).
pub(super) struct PageSnapshot {
    pub(super) page_count: u32,
    pub(super) label: Option<String>,
    pub(super) chars: Vec<(Option<char>, Quad)>,
}
/// Reads pages for `Propose` off the library thread.
pub(super) trait PageReader: Send {
    fn read(
        &mut self,
        path: &Path,
        page: u32,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<PageSnapshot, ServiceError>;
}

/// A resolved `Propose`; `finish` blocks, so it runs on a helper thread.
pub(super) struct PendingProposal {
    annotation: Annotation,
    attachment: Attachment,
    store: cm_storage::attachments::ManagedStore,
    pages: Box<dyn PageReader>,
    cancel: Cancellation,
    deadline: Instant,
}
impl PendingProposal {
    pub(super) fn finish(mut self) -> Result<Option<Proposal>, ServiceError> {
        let cancelled = || invalid("Search cancelled");
        if self.cancel.is_cancelled() {
            return Err(cancelled());
        }
        let path = self
            .store
            .verified_path(&self.attachment)
            .map_err(|_| invalid("The PDF is missing or does not match its checksum"))?;
        let mut search = AnchorSearch::new(&self.annotation);
        let mut count = 1u32;
        let mut index = 0u32;
        while index < count && !search.done() {
            if self.cancel.is_cancelled() {
                return Err(cancelled());
            }
            if Instant::now() >= self.deadline {
                return Err(invalid(format!(
                    "Search stopped after {} s at page {} of {count}; nothing was placed",
                    PROPOSAL_DEADLINE.as_secs(),
                    index + 1
                )));
            }
            let page = self.pages.read(&path, index, &self.cancel, self.deadline)?;
            count = page.page_count;
            if count > MAX_PROPOSAL_PAGES {
                return Err(invalid(format!(
                    "This PDF has {count} pages; finding positions supports at most {MAX_PROPOSAL_PAGES}"
                )));
            }
            search.page(&PageText {
                page_index: index,
                page_label: page.label,
                chars: &page.chars,
            });
            index += 1;
        }
        Ok(search.finish())
    }
}

impl Worker<'_> {
    pub(super) fn annotations(
        &mut self,
        request: AnnotationRequest,
    ) -> Result<AnnotationReply, ServiceError> {
        use AnnotationRequest::*;
        if let Propose { .. } = request {
            return Err(invalid("Propose requests are not queued here"));
        }
        // The confirmation ledger outlives each request (see `Confirmations`).
        let mut service = AnnotationService::with_confirmations(
            self.db.annotations(),
            std::mem::take(&mut self.confirmations),
        );
        let reply = (|| {
            Ok(match request {
                List(id) => AnnotationReply::List(service.list(id)?),
                ForPaper(id) => AnnotationReply::List(service.for_paper(id)?),
                Create(input) => edit(service.create(*input)?),
                Update(id, patch) => edit(service.update(id, patch)?),
                Delete(id) => edit(service.delete(id)?),
                ConfirmAi(id) => edit(service.confirm_ai(id)?),
                Undo(e) => {
                    service.undo(&e)?;
                    AnnotationReply::Done
                }
                Redo(e) => {
                    service.redo(&e)?;
                    AnnotationReply::Done
                }
                CarryOver { from, to } => AnnotationReply::Edits(service.carry_over(from, to)?),
                Resolve {
                    id,
                    page_index,
                    page_label,
                    quads,
                } => edit(service.resolve(id, page_index, page_label, quads)?),
                Propose { .. } => unreachable!("handled above"),
            })
        })();
        self.confirmations = service.into_confirmations();
        reply
    }
    /// Library-thread part of `Propose`: look up rows and hand out a page reader. No file IO.
    pub(super) fn begin_propose(
        &mut self,
        id: AnnotationId,
        cancel: Cancellation,
    ) -> Result<PendingProposal, ServiceError> {
        let annotation = AnnotationService::new(self.db.annotations())
            .get(id)?
            .ok_or(ServiceError::NotFound)?;
        let attachment = cm_core::ports::AttachmentRepository::get(
            &self.db.attachments(),
            annotation.attachment_id,
        )?
        .ok_or(ServiceError::NotFound)?;
        let store = cm_storage::attachments::ManagedStore::new(&self.directory)
            .map_err(|_| invalid("The library folder is unavailable"))?;
        let pages = self.page_reader()?;
        Ok(PendingProposal {
            annotation,
            attachment,
            store,
            pages,
            cancel,
            deadline: Instant::now() + PROPOSAL_DEADLINE,
        })
    }
    fn page_reader(&mut self) -> Result<Box<dyn PageReader>, ServiceError> {
        #[cfg(test)]
        if let Some(factory) = &mut self.test_pages {
            return Ok(factory());
        }
        self.pdf_page_reader()
    }
    #[cfg(feature = "pdf-metadata")]
    fn pdf_page_reader(&mut self) -> Result<Box<dyn PageReader>, ServiceError> {
        let submitter = self
            .pdf_worker()
            .map_err(|e| invalid(&e))?
            .submitter()
            .map_err(|e| invalid(format!("PDF runtime unavailable ({e})")))?;
        Ok(Box::new(PdfPages(submitter)))
    }
    #[cfg(not(feature = "pdf-metadata"))]
    fn pdf_page_reader(&mut self) -> Result<Box<dyn PageReader>, ServiceError> {
        Err(invalid(
            "This build has no PDF runtime to search the replacement PDF",
        ))
    }
}

/// Text-only page snapshots through the shared serial PDF worker.
#[cfg(feature = "pdf-metadata")]
struct PdfPages(cm_pdf::worker::Submitter);
#[cfg(feature = "pdf-metadata")]
impl PageReader for PdfPages {
    fn read(
        &mut self,
        path: &Path,
        page: u32,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<PageSnapshot, ServiceError> {
        use cm_pdf::worker::{self as pdf, Operation, Output, PdfError};
        let page_index = cm_protocol::domain::PageIndex::try_from(page)
            .map_err(|_| invalid("Page out of range"))?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if cancel.is_cancelled() {
                return Err(invalid("Search cancelled"));
            }
            if left.is_zero() {
                return Err(invalid("Search took too long; nothing was placed"));
            }
            let budget = left.min(Duration::from_secs(20));
            // zoom: None returns text and geometry without rendering a bitmap.
            let submitted = self.0.submit(
                Operation::Page {
                    source: pdf::Source {
                        path: path.into(),
                        password: None,
                    },
                    page_index,
                    zoom: None,
                },
                pdf::Cancellation::default(),
                budget,
            );
            let ticket = match submitted {
                // Viewer renders share the queue; wait for a slot rather than fail.
                Err(PdfError::QueueFull) => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                other => other.map_err(|e| invalid(format!("Cannot read the PDF ({e})")))?,
            };
            let output = ticket
                .wait_unless(budget + Duration::from_secs(1), || cancel.is_cancelled())
                .map_err(|e| match e {
                    PdfError::Cancelled => invalid("Search cancelled"),
                    e => invalid(format!("Cannot read the PDF ({e})")),
                })?;
            let Output::Page(snapshot) = output else {
                return Err(invalid("Unexpected PDF worker reply"));
            };
            return Ok(PageSnapshot {
                page_count: u32::try_from(snapshot.page_count).unwrap_or(0),
                label: snapshot.page_label,
                chars: snapshot
                    .characters
                    .into_iter()
                    .map(|c| (c.unicode, c.quad))
                    .collect(),
            });
        }
    }
}

fn edit(edit: Edit) -> AnnotationReply {
    AnnotationReply::Edit(Box::new(edit))
}

#[cfg(test)]
mod tests {
    //! `Propose` with a controllable page source: the library worker keeps serving while a
    //! search is blocked in the PDF, and searches end by cancellation or an explicit page cap.
    use super::*;
    use crate::desktop_worker::InProcessLibraryClient;
    use cm_core::geometry::Rect;
    use cm_protocol::{
        attachments::*,
        domain::{AttachmentId, PaperId},
        library::*,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        mpsc,
    };

    /// Blocks every page read until `release` is set (or the search is cancelled).
    struct Blocked {
        started: Arc<AtomicUsize>,
        release: Arc<AtomicBool>,
        pages: Arc<AtomicU32>,
    }
    impl PageReader for Blocked {
        fn read(
            &mut self,
            _path: &Path,
            _page: u32,
            cancel: &Cancellation,
            deadline: Instant,
        ) -> Result<PageSnapshot, ServiceError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                if cancel.is_cancelled() {
                    return Err(invalid("Search cancelled"));
                }
                if Instant::now() >= deadline {
                    return Err(invalid("deadline"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let chars = "hay needle hay"
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    let left = 10.0 + i as f32 * 6.0;
                    (
                        Some(c),
                        Rect {
                            left,
                            bottom: 100.0,
                            right: left + 6.0,
                            top: 110.0,
                        }
                        .quad(),
                    )
                })
                .collect();
            Ok(PageSnapshot {
                page_count: self.pages.load(Ordering::SeqCst),
                label: None,
                chars,
            })
        }
    }

    fn call<T: Send + 'static>(
        submit: impl FnOnce(Reply<T>) -> Result<(), ServiceError>,
    ) -> Result<T, ServiceError> {
        let (tx, rx) = mpsc::channel();
        submit(Box::new(move |r| {
            let _ = tx.send(r);
        }))?;
        rx.recv_timeout(Duration::from_secs(30)).unwrap()
    }
    fn attach(c: &InProcessLibraryClient, paper: PaperId, file: &Path) -> AttachmentId {
        call(|d| {
            c.pdf(
                PdfRequest::Inspect {
                    selection: Selection::Files(vec![file.into()]),
                    intent: PdfIntent::AttachExisting(paper),
                    cancel: Default::default(),
                },
                Box::new(|_| {}),
                d,
            )
        })
        .unwrap();
        let PdfReply::Report(r) = call(|d| {
            c.pdf(
                PdfRequest::Attach {
                    indices: vec![0],
                    paper,
                    cancel: Default::default(),
                },
                Box::new(|_| {}),
                d,
            )
        })
        .unwrap() else {
            panic!()
        };
        r.files[0].attachment.unwrap()
    }
    fn propose(
        c: &InProcessLibraryClient,
        id: AnnotationId,
        cancel: Cancellation,
    ) -> mpsc::Receiver<Result<AnnotationReply, ServiceError>> {
        let (tx, rx) = mpsc::channel();
        c.annotations(
            AnnotationRequest::Propose { id, cancel },
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
        rx
    }
    fn wait_for(started: &AtomicUsize, at_least: usize) {
        let until = Instant::now() + Duration::from_secs(20);
        while started.load(Ordering::SeqCst) < at_least {
            assert!(Instant::now() < until, "search never reached the PDF");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn propose_runs_off_the_library_worker_with_cancellation_and_page_cap() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/pdf/corpus.pdf")
            .canonicalize()
            .unwrap();
        let c = InProcessLibraryClient::start(root.join("library")).unwrap();
        let LibraryReply::Paper(paper) = call(|d| {
            c.submit(
                LibraryRequest::Create(Box::new(NewPaper::titled("Search"))),
                d,
            )
        })
        .unwrap() else {
            panic!()
        };
        let attachment = attach(&c, paper.id, &fixture);
        let AnnotationReply::Edit(created) = call(|d| {
            c.annotations(
                AnnotationRequest::Create(Box::new(NewAnnotation {
                    attachment_id: attachment,
                    kind: AnnotationKind::Highlight,
                    source: Source::Human,
                    anchor: Anchor::Unresolved {
                        reason: "copied".into(),
                    },
                    quote: "needle".into(),
                    context_before: Some("hay ".into()),
                    context_after: Some(" hay".into()),
                    color: None,
                    comment: None,
                })),
                d,
            )
        })
        .unwrap() else {
            panic!()
        };
        let Edit::Created(annotation) = *created else {
            panic!()
        };
        let (started, release, pages) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU32::new(3)),
        );
        let factory = {
            let (started, release, pages) = (started.clone(), release.clone(), pages.clone());
            move || -> Box<dyn PageReader> {
                Box::new(Blocked {
                    started: started.clone(),
                    release: release.clone(),
                    pages: pages.clone(),
                })
            }
        };
        call(|d| {
            c.enqueue(d, move |w| {
                w.test_pages = Some(Box::new(factory));
                Ok(())
            })
        })
        .unwrap();

        // 1. A search blocked inside the PDF does not block the library worker.
        let pending = propose(&c, annotation.id, Cancellation::default());
        wait_for(&started, 1);
        let begun = Instant::now();
        let AnnotationReply::List(rows) =
            call(|d| c.annotations(AnnotationRequest::List(attachment), d)).unwrap()
        else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
        assert!(begun.elapsed() < Duration::from_secs(5));
        assert!(
            pending.try_recv().is_err(),
            "the search is still blocked while List completed"
        );
        release.store(true, Ordering::SeqCst);
        let AnnotationReply::Proposal(Some(found)) = pending
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(found.page_index, 0);
        assert!(found.context_match);
        assert_eq!(found.matches, 3, "one per page");
        assert_eq!(found.quads.len(), 1);

        // 2. Cancellation ends a blocked search promptly.
        release.store(false, Ordering::SeqCst);
        let before = started.load(Ordering::SeqCst);
        let cancel = Cancellation::default();
        let pending = propose(&c, annotation.id, cancel.clone());
        wait_for(&started, before + 1);
        cancel.cancel();
        let result = pending.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(result, Err(ServiceError::InvalidInput(m)) if m.contains("cancelled")));

        // 3. Too many pages is an explicit error, not a silently truncated search.
        release.store(true, Ordering::SeqCst);
        pages.store(MAX_PROPOSAL_PAGES + 1, Ordering::SeqCst);
        let result = propose(&c, annotation.id, Cancellation::default())
            .recv_timeout(Duration::from_secs(20))
            .unwrap();
        assert!(matches!(result, Err(ServiceError::InvalidInput(m)) if m.contains("pages")));
        // Nothing was placed by any search.
        let AnnotationReply::List(rows) =
            call(|d| c.annotations(AnnotationRequest::List(attachment), d)).unwrap()
        else {
            panic!()
        };
        assert_eq!(rows[0], annotation);
    }
}
