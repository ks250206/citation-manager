//! State regressions only; these do not claim native GUI acceptance.
use cm_desktop::{
    attachments::{OperationCancels, PdfForm},
    details,
    import_dialogs::{Review, ReviewSessions},
};
use cm_protocol::{attachments::*, import::*, library::ServiceError};

fn file(hash: u8) -> PdfFile {
    let mut draft = PaperDraft::titled("PDF title");
    draft.bibliography.doi = Some("10.1234/pdf".into());
    PdfFile {
        path: "/private/tmp/synthetic-original.pdf".into(),
        name: "synthetic-original.pdf".into(),
        sha256: Some([hash; 32]),
        bytes: 42,
        draft: Some(draft),
        messages: vec![],
        disposition: FileDisposition::Ready,
        paper: None,
        attachment: None,
    }
}
fn preview(id: &str) -> ImportPreview {
    ImportPreview {
        id: PreviewId(id.into()),
        candidates: vec![PreviewCandidate {
            draft: PaperDraft::titled("Reviewed candidate"),
            existing_match: None,
            matches: vec![],
            batch_matches: vec![],
            diagnostics: vec![],
        }],
        warnings: vec![],
    }
}
fn file_review() -> Review {
    let mut review = Review {
        from_file: true,
        total: 1000,
        batch_start: 200,
        committed: 180,
        skipped: 20,
        ..Default::default()
    };
    review.set_preview(preview("file-next-batch"));
    review.choose_valid_creates();
    review
}

#[test]
fn pdf_form_survives_library_selection_and_new_manual_without_borrowing_their_metadata() {
    let source = file(1);
    let mut pdf = PdfForm::default();
    assert!(pdf.bind(0, Some(&source)));
    let mut pdf_fields = pdf.values();
    pdf_fields[0] = "Reviewed PDF edit".into();
    // The separate library editor may now hold paper X, then Cmd-N's empty manual draft.
    for library in [
        PaperDraft::titled("Unrelated paper X"),
        PaperDraft::default(),
    ] {
        let library_fields = details::values(&library);
        assert!(
            !pdf.bind(0, Some(&source)),
            "same origin must retain PDF edits"
        );
        let explicit = pdf.edited_draft(0, &source, &pdf_fields).unwrap();
        assert_eq!(explicit.title, "Reviewed PDF edit");
        assert_eq!(explicit.bibliography.doi.as_deref(), Some("10.1234/pdf"));
        assert_eq!(library_fields[0], library.title);
    }
    assert!(pdf.edited_draft(1, &source, &pdf_fields).is_err());
    let changed = file(2);
    assert!(pdf.edited_draft(0, &changed, &pdf_fields).is_err());
    assert!(pdf.bind(0, Some(&changed)));
    assert_eq!(pdf.values()[0], "PDF title");
    let mut moved = changed.clone();
    moved.path = "/private/tmp/another-original.pdf".into();
    assert!(pdf.edited_draft(0, &moved, &pdf_fields).is_err());
    moved.sha256 = None;
    assert!(pdf.bind(0, Some(&moved)));
    assert!(pdf.edited_draft(0, &moved, &pdf_fields).is_err());
}

#[test]
fn pdf_reports_preserve_unrelated_previews_and_paused_batch_progress_requires_new_approval() {
    let mut sessions = ReviewSessions {
        active: file_review(),
        ..Default::default()
    };
    sessions
        .finish_pdf(|_| panic!("unrelated file preview must remain retained"))
        .unwrap();
    assert!(sessions.active.commit().is_some());
    let mut cancelled = vec![];
    sessions
        .pause_file(|id| {
            cancelled.push(id.clone());
            Ok(())
        })
        .unwrap();
    assert_eq!(cancelled, vec![PreviewId("file-next-batch".into())]);
    for n in 0..8 {
        sessions.active.pdf_index = Some(0);
        sessions.active.set_preview(preview(&format!("pdf-{n}")));
        sessions
            .pause_file(|id| {
                cancelled.push(id.clone());
                Ok(())
            })
            .unwrap();
    }
    sessions
        .resume_file(|_| panic!("already released PDF preview"))
        .unwrap();
    assert_eq!(
        (
            sessions.active.batch_start,
            sessions.active.total,
            sessions.active.committed,
            sessions.active.skipped
        ),
        (200, 1000, 180, 20)
    );
    assert!(sessions.active.preview.is_none());
    assert!(sessions.active.commit().is_none());
    sessions.active.set_preview(preview("fresh-file-preview"));
    assert!(
        sessions.active.commit().is_none(),
        "old decisions must not approve a new preview"
    );
}

#[test]
fn a_failed_cancel_submission_keeps_the_original_handle_and_decisions() {
    let mut sessions = ReviewSessions {
        active: file_review(),
        ..Default::default()
    };
    assert!(
        sessions
            .pause_file(|_| Err(ServiceError::Unavailable))
            .is_err()
    );
    assert_eq!(
        sessions.active.preview.as_ref().unwrap().id.0,
        "file-next-batch"
    );
    assert!(sessions.active.commit().is_some());
    assert!(sessions.paused_file.is_none());
    sessions.pause_file(|_| Ok(())).unwrap();
    sessions.replace_file(|_| Ok(())).unwrap();
    let summary = sessions.replaced_file.unwrap();
    assert!(summary.contains("200 / 1000"));
    assert!(summary.contains("180 committed, 20 skipped"));
    assert!(sessions.paused_file.is_none());
}

#[test]
fn editing_or_cancelling_doi_never_cancels_attach_retry_or_commit_and_reverse_is_isolated() {
    let mut operations = OperationCancels::default();
    // Attach, RetryAttachment and CommitNew each start a fresh PDF token in production.
    for _ in 0..3 {
        let doi = operations.start_doi();
        let pdf = operations.start_pdf();
        operations.cancel_doi();
        assert!(doi.is_cancelled());
        assert!(!pdf.is_cancelled());
        let doi_retry = operations.start_doi();
        assert!(
            !operations.cancel_pdf(false),
            "idle PDF cancel has no effect"
        );
        assert!(!doi_retry.is_cancelled());
        assert!(!pdf.is_cancelled());
        assert!(operations.cancel_pdf(true));
        assert!(pdf.is_cancelled());
        assert!(!doi_retry.is_cancelled());
        let pdf_retry = operations.start_pdf();
        operations.start_doi();
        assert!(doi_retry.is_cancelled());
        assert!(!pdf_retry.is_cancelled());
    }
}
