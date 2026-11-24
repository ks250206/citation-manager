//! #19 through the real worker and SQLite: persistence across reopen, undo/redo, AI review,
//! PDF replacement. Viewer overlays/list are covered by tests/e2e/annotations.
use cm_daemon::desktop_worker::InProcessLibraryClient;
use cm_protocol::{
    annotations::*,
    attachments::*,
    domain::{AttachmentId, PaperId},
    geometry::Rect,
    library::*,
};
use std::{path::Path, sync::mpsc, time::Duration};

fn call<T: Send + 'static>(
    submit: impl FnOnce(Reply<T>) -> Result<(), ServiceError>,
) -> Result<T, ServiceError> {
    let (tx, rx) = mpsc::channel();
    submit(Box::new(move |r| {
        let _ = tx.send(r);
    }))?;
    rx.recv_timeout(Duration::from_secs(30)).unwrap()
}
fn ann(c: &InProcessLibraryClient, r: AnnotationRequest) -> Result<AnnotationReply, ServiceError> {
    call(|done| c.annotations(r, done))
}
fn list(c: &InProcessLibraryClient, a: AttachmentId) -> Vec<Annotation> {
    match ann(c, AnnotationRequest::List(a)).unwrap() {
        AnnotationReply::List(v) => v,
        _ => panic!(),
    }
}
fn edit(reply: AnnotationReply) -> Edit {
    match reply {
        AnnotationReply::Edit(e) => *e,
        _ => panic!(),
    }
}
fn attach(c: &InProcessLibraryClient, paper: PaperId, file: &Path) -> AttachmentId {
    let PdfReply::Report(_) = call(|d| {
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
    .unwrap() else {
        panic!()
    };
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
fn highlight(attachment: AttachmentId, source: Source) -> NewAnnotation {
    let quad = |l: f32, b: f32| {
        Rect {
            left: l,
            bottom: b,
            right: l + 120.0,
            top: b + 12.0,
        }
        .quad()
    };
    NewAnnotation {
        attachment_id: attachment,
        kind: AnnotationKind::Highlight,
        source,
        anchor: Anchor::Resolved {
            page_index: 1,
            page_label: Some("ii".into()),
            // Two lines, one rotated page's worth of coordinates: stored verbatim.
            quads: vec![quad(72.5, 700.25), quad(72.5, 686.0)],
        },
        quote: "Unicode 日本語 αβ café\nsecond line".into(),
        context_before: Some("前文".into()),
        context_after: Some("後文".into()),
        color: Some("#33aa55".into()),
        comment: Some("コメント".into()),
    }
}

#[test]
fn annotations_persist_undo_redo_review_and_carry_over() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let library = root.join("library");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pdf")
        .canonicalize()
        .unwrap();
    let (paper_id, attachment, replacement, created) = {
        let c = InProcessLibraryClient::start(library.clone()).unwrap();
        let LibraryReply::Paper(paper) = call(|d| {
            c.submit(
                LibraryRequest::Create(Box::new(NewPaper::titled("Annotated"))),
                d,
            )
        })
        .unwrap() else {
            panic!()
        };
        let attachment = attach(&c, paper.id, &fixtures.join("corpus.pdf"));
        let replacement = attach(&c, paper.id, &fixtures.join("inherited.pdf"));
        let Edit::Created(created) = edit(
            ann(
                &c,
                AnnotationRequest::Create(Box::new(highlight(attachment, Source::Human))),
            )
            .unwrap(),
        ) else {
            panic!()
        };
        // A page note on another page.
        let note = NewAnnotation {
            kind: AnnotationKind::Note,
            anchor: Anchor::Resolved {
                page_index: 0,
                page_label: None,
                quads: vec![],
            },
            quote: String::new(),
            comment: Some("独立ノート".into()),
            color: None,
            ..highlight(attachment, Source::Human)
        };
        edit(ann(&c, AnnotationRequest::Create(Box::new(note))).unwrap());
        // Invalid input never reaches SQLite.
        let mut bad = highlight(attachment, Source::Human);
        bad.color = Some("javascript:".into());
        assert!(ann(&c, AnnotationRequest::Create(Box::new(bad))).is_err());
        (paper.id, attachment, replacement, created)
    };

    // Reopen: same rows, positions and text, page order.
    let c = InProcessLibraryClient::start(library).unwrap();
    let rows = list(&c, attachment);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].kind, AnnotationKind::Note);
    assert_eq!(rows[1], created);

    // Update → undo → redo through the same service.
    let update = edit(
        ann(
            &c,
            AnnotationRequest::Update(
                created.id,
                AnnotationPatch {
                    color: Some(Some("blue".into())),
                    comment: Some(None),
                },
            ),
        )
        .unwrap(),
    );
    ann(&c, AnnotationRequest::Undo(Box::new(update.clone()))).unwrap();
    assert_eq!(list(&c, attachment)[1], created);
    ann(&c, AnnotationRequest::Redo(Box::new(update.clone()))).unwrap();
    let updated = list(&c, attachment)[1].clone();
    assert_eq!(updated.color.as_deref(), Some("blue"));
    assert_eq!(updated.comment, None);
    // Delete → undo restores the identical row.
    let deleted = edit(ann(&c, AnnotationRequest::Delete(created.id)).unwrap());
    assert_eq!(list(&c, attachment).len(), 1);
    ann(&c, AnnotationRequest::Undo(Box::new(deleted))).unwrap();
    assert_eq!(list(&c, attachment)[1], updated);
    // Undo of a stale edit is refused rather than clobbering newer state: another change
    // (e.g. an AI agent or second window) lands after the recorded update.
    edit(
        ann(
            &c,
            AnnotationRequest::Update(
                created.id,
                AnnotationPatch {
                    color: None,
                    comment: Some(Some("newer".into())),
                },
            ),
        )
        .unwrap(),
    );
    assert_eq!(
        ann(&c, AnnotationRequest::Undo(Box::new(update))).unwrap_err(),
        ServiceError::Conflict
    );

    // AI proposal: distinct source, confirm keeps provenance; confirming a human one fails.
    let Edit::Created(ai) = edit(
        ann(
            &c,
            AnnotationRequest::Create(Box::new(highlight(attachment, Source::Ai))),
        )
        .unwrap(),
    ) else {
        panic!()
    };
    assert!(ann(&c, AnnotationRequest::ConfirmAi(created.id)).is_err());
    let Edit::Updated { after, .. } = edit(ann(&c, AnnotationRequest::ConfirmAi(ai.id)).unwrap())
    else {
        panic!()
    };
    assert_eq!(after.source, Source::Ai);
    assert!(after.confirmed_at.is_some());

    // PDF replaced: copies arrive unresolved; nothing placed automatically.
    let AnnotationReply::Edits(copies) = ann(
        &c,
        AnnotationRequest::CarryOver {
            from: attachment,
            to: replacement,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(copies.len(), 3);
    assert!(
        list(&c, replacement)
            .iter()
            .all(|a| matches!(a.anchor, Anchor::Unresolved { .. }))
    );
    let copy = list(&c, replacement)[0].clone();
    // Proposal needs the PDF runtime; tests run without the bundle.
    assert!(ann(&c, AnnotationRequest::Propose(copy.id)).is_err());
    // Explicit placement after review.
    let quads = match &created.anchor {
        Anchor::Resolved { quads, .. } => quads.clone(),
        _ => unreachable!(),
    };
    ann(
        &c,
        AnnotationRequest::Resolve {
            id: copy.id,
            page_index: 0,
            page_label: None,
            quads,
        },
    )
    .unwrap();
    assert!(
        list(&c, replacement)
            .iter()
            .any(|a| matches!(a.anchor, Anchor::Resolved { page_index: 0, .. }))
    );
    // Paper-wide listing spans both PDFs.
    let AnnotationReply::List(all) = ann(&c, AnnotationRequest::ForPaper(paper_id)).unwrap() else {
        panic!()
    };
    assert_eq!(all.len(), 6);
}
