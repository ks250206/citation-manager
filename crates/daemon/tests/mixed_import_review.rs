//! Actual retained coordinator/SQLite regression, separate from native GUI acceptance.
#![cfg(all(target_os = "macos", feature = "desktop-bootstrap"))]
use cm_daemon::desktop_worker::InProcessLibraryClient;
use cm_desktop::{attachments::PdfForm, import_dialogs::ReviewSessions};
use cm_protocol::{attachments::*, exchange::*, import::*, library::*, library_query::*};
use std::{sync::mpsc, time::Duration};
fn call<T: Send + 'static>(
    submit: impl FnOnce(Reply<T>) -> Result<(), ServiceError>,
) -> Result<T, ServiceError> {
    let (tx, rx) = mpsc::channel();
    submit(Box::new(move |r| {
        let _ = tx.send(r);
    }))?;
    rx.recv_timeout(Duration::from_secs(30))
        .expect("worker reply")
}
fn imp(c: &InProcessLibraryClient, request: ImportRequestUi) -> ImportReply {
    call(|done| c.import(request, done)).unwrap()
}
fn preview(reply: ImportReply) -> ImportPreview {
    let ImportReply::Preview(p) = reply else {
        panic!("preview expected");
    };
    p
}
fn release(c: &InProcessLibraryClient, id: &PreviewId) -> Result<(), ServiceError> {
    let result = call(|done| c.import(ImportRequestUi::Cancel(id.clone()), done))?;
    assert!(matches!(result, ImportReply::Cancelled));
    Ok(())
}
#[test]
fn repeated_mixed_pdf_manual_and_bibliography_reviews_release_real_slots_and_resume_after_prior_commit()
 {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let c = InProcessLibraryClient::start(root.join("library")).unwrap();
    let source = root.join("synthetic.bib");
    std::fs::write(&source, (0..6).map(|n| format!("@article{{key{n},title={{Distinct material sample number {n}}},doi={{10.1234/review-{n}}}}}\n")).collect::<String>()).unwrap();
    assert!(matches!(
        imp(
            &c,
            ImportRequestUi::Parse {
                path: source,
                format: Format::Bibtex
            }
        ),
        ImportReply::Parsed(_)
    ));
    let mut sessions = ReviewSessions::default();
    sessions.active.from_file = true;
    sessions.active.total = 6;
    sessions.active.set_preview(preview(imp(
        &c,
        ImportRequestUi::PreviewFile {
            start: 0,
            count: 2,
            destination: Default::default(),
        },
    )));
    sessions.active.choose_valid_creates();
    let ImportReply::Committed(outcome) = imp(
        &c,
        ImportRequestUi::Commit(sessions.active.commit().unwrap()),
    ) else {
        panic!()
    };
    assert_eq!(outcome.created.len(), 2);
    sessions.active.preview = None;
    sessions.active.decisions.clear();
    sessions.active.batch_start = 2;
    sessions.active.committed = 2;
    let next_file = || {
        preview(imp(
            &c,
            ImportRequestUi::PreviewFile {
                start: 2,
                count: 1,
                destination: Default::default(),
            },
        ))
    };
    sessions.active.set_preview(next_file());
    // An unrelated Attach/Retry report must not discard this live file preview.
    sessions
        .finish_pdf(|_| panic!("must preserve unrelated preview"))
        .unwrap();
    let old = sessions.active.preview.as_ref().unwrap().id.clone();
    sessions.pause_file(|id| release(&c, id)).unwrap();
    assert!(
        call(|done| c.import(
            ImportRequestUi::Commit(ImportCommit {
                preview_id: old,
                decisions: vec![]
            }),
            done
        ))
        .is_err()
    );
    let pdf = root.join("original.pdf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/library/info.pdf"),
        &pdf,
    )
    .unwrap();
    let PdfReply::Report(report) = call(|done| {
        c.pdf(
            PdfRequest::Inspect {
                selection: Selection::Files(vec![pdf]),
                intent: PdfIntent::RegisterNew,
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap() else {
        panic!()
    };
    assert_eq!(report.files.len(), 1);
    let mut pdf_form = PdfForm::default();
    assert!(pdf_form.bind(0, report.files.first()));
    let mut pdf_fields = pdf_form.values();
    pdf_fields[0] = "Reviewed PDF after library navigation".into();
    pdf_fields[2] = "10.1234/pdf-reviewed".into();
    pdf_fields[6] = "PDF Author".into();
    for round in 0..12 {
        sessions.pause_file(|id| release(&c, id)).unwrap();
        if round % 2 == 0 {
            // Library selection and Cmd-N replace only the separate library form.
            let library = if round % 4 == 0 {
                PaperDraft::titled("Unrelated paper X")
            } else {
                PaperDraft::default()
            };
            assert_ne!(library.title, pdf_fields[0]);
            assert!(!pdf_form.bind(0, report.files.first()));
            let explicit = pdf_form
                .edited_draft(0, &report.files[0], &pdf_fields)
                .unwrap();
            let PdfReply::Preview(p) = call(|done| {
                c.pdf(
                    PdfRequest::PreviewNew {
                        index: 0,
                        explicit: Some(Box::new(explicit)),
                        destination: Default::default(),
                    },
                    Box::new(|_| {}),
                    done,
                )
            })
            .unwrap() else {
                panic!()
            };
            assert_eq!(
                p.candidates[0].draft.title,
                "Reviewed PDF after library navigation"
            );
            assert_eq!(
                p.candidates[0].draft.bibliography.doi.as_deref(),
                Some("10.1234/pdf-reviewed")
            );
            assert_eq!(
                p.candidates[0].draft.bibliography.authors[0]
                    .literal_name
                    .as_deref(),
                Some("PDF Author")
            );
            sessions.active.pdf_index = Some(0);
            sessions.active.set_preview(p);
        } else {
            sessions.active.set_preview(preview(imp(
                &c,
                ImportRequestUi::Preview(ImportRequest {
                    candidates: vec![PaperDraft::titled("Manual unsaved preview")],
                    destination: Default::default(),
                }),
            )));
        }
    }
    sessions.resume_file(|id| release(&c, id)).unwrap();
    assert_eq!(
        (
            sessions.active.batch_start,
            sessions.active.total,
            sessions.active.committed
        ),
        (2, 6, 2)
    );
    assert!(sessions.active.commit().is_none());
    sessions.active.set_preview(next_file());
    assert_eq!(
        sessions.active.preview.as_ref().unwrap().candidates[0]
            .draft
            .title,
        "Distinct material sample number 2"
    );
    assert!(sessions.active.commit().is_none());
    sessions.active.choose_valid_creates();
    assert!(matches!(
        imp(
            &c,
            ImportRequestUi::Commit(sessions.active.commit().unwrap())
        ),
        ImportReply::Committed(_)
    ));
    let page = call(|done| c.query(LibraryQuery::default(), done)).unwrap();
    assert_eq!(
        page.papers.len(),
        3,
        "prior commits remain; previews never create papers"
    );
}
