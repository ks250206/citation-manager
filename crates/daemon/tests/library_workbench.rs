//! Real worker/SQLite/adapter evidence, deliberately independent of native GUI claims.
use cm_daemon::desktop_worker::InProcessLibraryClient;
use cm_protocol::{
    attachments::*, exchange::*, import::*, library::*, library_query::*, organization::*,
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
fn call<T: Send + 'static>(
    submit: impl FnOnce(Reply<T>) -> Result<(), ServiceError>,
) -> Result<T, ServiceError> {
    call_with_watchdog("worker request", Duration::from_secs(30), submit)
}
fn call_with_watchdog<T: Send + 'static>(
    operation: &str,
    watchdog: Duration,
    submit: impl FnOnce(Reply<T>) -> Result<(), ServiceError>,
) -> Result<T, ServiceError> {
    let (tx, rx) = mpsc::channel();
    submit(Box::new(move |r| {
        let _ = tx.send(r);
    }))?;
    rx.recv_timeout(watchdog).unwrap_or_else(|error| {
        panic!("{operation}: worker callback {error:?} after watchdog {watchdog:?}")
    })
}
fn imp(c: &InProcessLibraryClient, request: ImportRequestUi) -> ImportReply {
    call(|done| c.import(request, done)).unwrap()
}
// These 100-row preview/commit requests scan an increasing real inventory in
// unoptimized test builds. This finite CI watchdog is not a product latency SLO.
// Keep ordinary requests on 30 seconds and retain all 1000 rows/ten approvals.
fn thousand_batch(
    c: &InProcessLibraryClient,
    start: usize,
    stage: &str,
    request: ImportRequestUi,
) -> ImportReply {
    let operation = format!(
        "1000-record batch {} ({start}..{}) {stage}",
        start / 100 + 1,
        start + 100
    );
    let started = Instant::now();
    eprintln!("Starting {operation}");
    let result = call_with_watchdog(&operation, Duration::from_secs(120), |done| {
        c.import(request, done)
    });
    eprintln!("Finished {operation} in {:?}", started.elapsed());
    result.unwrap()
}
fn preview(reply: ImportReply) -> ImportPreview {
    let ImportReply::Preview(p) = reply else {
        panic!("preview")
    };
    p
}
fn create(preview: ImportPreview) -> ImportCommit {
    ImportCommit {
        preview_id: preview.id,
        decisions: preview
            .candidates
            .iter()
            .enumerate()
            .map(|(candidate_index, _)| ImportDecision {
                candidate_index,
                action: ImportAction::Create,
            })
            .collect(),
    }
}
#[test]
fn thousand_file_ten_reviewed_batches_classification_query_export_and_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("library");
    let client = InProcessLibraryClient::start(path.clone()).unwrap();
    let source = root.join("thousand.bib");
    use sha2::Digest;
    let input = (0..1000).map(|n| {
        let digest = format!("{:x}", sha2::Sha256::digest(n.to_string().as_bytes()));
        format!("@article{{synthetic{n},title={{Synthetic material {n:04} {digest}}},author={{Doe, Example}},year={{2026}},doi={{10.1234/synthetic-{n}}}}}\n")
    }).collect::<String>();
    std::fs::write(&source, input).unwrap();
    let org = call(|done| {
        client.organization(
            OrganizationRequest::CreateCollection(CollectionInput::named("Research")),
            done,
        )
    })
    .unwrap();
    let collection = org.collections[0].id;
    let org = call(|done| {
        client.organization(
            OrganizationRequest::CreateTag(TagInput::named("Synthetic")),
            done,
        )
    })
    .unwrap();
    let tag = org.tags[0].id;
    let ImportReply::Parsed(parsed) = imp(
        &client,
        ImportRequestUi::Parse {
            path: source,
            format: Format::Bibtex,
        },
    ) else {
        panic!()
    };
    assert_eq!(parsed.records, 1000);
    let destination = ImportDestination {
        collection: Some(collection),
        tags: vec![tag],
    };
    let mut ids = Vec::new();
    for start in (0..1000).step_by(100) {
        let p = preview(thousand_batch(
            &client,
            start,
            "preview",
            ImportRequestUi::PreviewFile {
                start,
                count: 100,
                destination: destination.clone(),
            },
        ));
        assert_eq!(p.candidates.len(), 100);
        // Preview never writes, no UI-generated inventory or auto-approval layer.
        let before = call(|done| {
            client.query(
                LibraryQuery {
                    limit: 500,
                    ..Default::default()
                },
                done,
            )
        })
        .unwrap();
        assert_eq!(before.papers.len(), start.min(500));
        let ImportReply::Committed(outcome) =
            thousand_batch(&client, start, "commit", ImportRequestUi::Commit(create(p)))
        else {
            panic!()
        };
        assert_eq!(outcome.created.len(), 100);
        ids.extend(outcome.created);
    }
    let page = call(|done| {
        client.query(
            LibraryQuery {
                filter: LibraryFilter {
                    collection: Some(collection),
                    tags: vec![tag],
                    keyword: Some("10.1234/synthetic-523".into()),
                    year_from: Some(2026),
                    ..Default::default()
                },
                sort: LibrarySort {
                    field: SortField::Title,
                    direction: SortDirection::Desc,
                },
                ..Default::default()
            },
            done,
        )
    })
    .unwrap();
    assert_eq!(page.papers.len(), 1);
    let output = root.join("all.ris");
    let exported = call(|done| {
        client.export(
            ExportRequest {
                selection: ExportSelection::Collection(collection),
                format: Format::Ris,
                path: output.clone(),
            },
            done,
        )
    })
    .unwrap();
    assert_eq!(exported.records, 1000);
    assert!(!exported.diagnostics.is_empty());
    assert!(exported.migration_notice.contains("bibliography only"));
    assert_eq!(
        cm_metadata::ris::parse(&std::fs::read(output).unwrap())
            .unwrap()
            .candidates
            .len(),
        1000
    );
    let mut settings = cm_protocol::settings::Settings {
        details_tab: "PDF".into(),
        ..Default::default()
    };
    settings.columns[1].width = 180;
    settings.sort = LibrarySort {
        field: SortField::Year,
        direction: SortDirection::Desc,
    };
    call(|done| client.save_settings(settings.clone(), done)).unwrap();
    call(|done| client.lifecycle(ids[0], true, done)).unwrap();
    let trash = call(|done| {
        client.query(
            LibraryQuery {
                filter: LibraryFilter {
                    visibility: PaperVisibility::Deleted,
                    ..Default::default()
                },
                ..Default::default()
            },
            done,
        )
    })
    .unwrap();
    assert_eq!(trash.papers.len(), 1);
    call(|done| client.lifecycle(ids[0], false, done)).unwrap();
    drop(client);
    let reopened = InProcessLibraryClient::start(path).unwrap();
    assert_eq!(
        call(|done| reopened.load_settings(done)).unwrap().settings,
        settings
    );
    let page = call(|done| {
        reopened.query(
            LibraryQuery {
                limit: 500,
                ..Default::default()
            },
            done,
        )
    })
    .unwrap();
    assert_eq!(page.papers.len(), 500);
    assert!(page.next.is_some());
}
#[test]
fn diagnostics_error_rows_skip_cancel_stale_and_duplicate_merge_are_real() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let client = InProcessLibraryClient::start(root.join("library")).unwrap();
    let source = root.join("invalid.bib");
    std::fs::write(&source,"@article{a,title={Good},doi={10.1234/a}}\n@article{bad,title={Bad},year={nonsense}}\n@article{rejected,title={unterminated\n").unwrap();
    let ImportReply::Parsed(parsed) = imp(
        &client,
        ImportRequestUi::Parse {
            path: source,
            format: Format::Bibtex,
        },
    ) else {
        panic!()
    };
    assert!(!parsed.diagnostics.is_empty());
    let p = preview(imp(
        &client,
        ImportRequestUi::PreviewFile {
            start: 0,
            count: 2,
            destination: Default::default(),
        },
    ));
    assert!(p.candidates[1].diagnostics.iter().any(|d| d.error));
    assert!(call(|done| client.import(ImportRequestUi::Commit(create(p.clone())), done)).is_err());
    let ImportReply::Committed(outcome) = imp(
        &client,
        ImportRequestUi::Commit(ImportCommit {
            preview_id: p.id,
            decisions: vec![
                ImportDecision {
                    candidate_index: 0,
                    action: ImportAction::Create,
                },
                ImportDecision {
                    candidate_index: 1,
                    action: ImportAction::Skip,
                },
            ],
        }),
    ) else {
        panic!()
    };
    assert_eq!(outcome.created.len(), 1);
    let mut draft = PaperDraft::titled("Updated normalized title");
    draft.bibliography.doi = Some("10.1234/a".into());
    let p = preview(imp(
        &client,
        ImportRequestUi::Preview(ImportRequest {
            candidates: vec![draft.clone()],
            destination: Default::default(),
        }),
    ));
    assert_eq!(p.candidates[0].matches.len(), 1);
    assert!(
        p.candidates[0].matches[0]
            .differences
            .iter()
            .any(|d| d.field == "title")
    );
    imp(&client, ImportRequestUi::Cancel(p.id.clone()));
    assert!(call(|done| client.import(ImportRequestUi::Commit(create(p)), done)).is_err());
    let p = preview(imp(
        &client,
        ImportRequestUi::Preview(ImportRequest {
            candidates: vec![draft.clone()],
            destination: Default::default(),
        }),
    ));
    call(|done| {
        client.organization(
            OrganizationRequest::CreateTag(TagInput::named("Write invalidates preview")),
            done,
        )
    })
    .unwrap();
    assert_eq!(
        call(|done| client.import(
            ImportRequestUi::Commit(ImportCommit {
                preview_id: p.id.clone(),
                decisions: vec![ImportDecision {
                    candidate_index: 0,
                    action: ImportAction::Merge {
                        match_index: 0,
                        fields: vec!["title".into()]
                    }
                }]
            }),
            done
        ))
        .unwrap_err(),
        ServiceError::Conflict
    );
    imp(&client, ImportRequestUi::Cancel(p.id));
    let p = preview(imp(
        &client,
        ImportRequestUi::Preview(ImportRequest {
            candidates: vec![draft],
            destination: Default::default(),
        }),
    ));
    let ImportReply::Committed(outcome) = imp(
        &client,
        ImportRequestUi::Commit(ImportCommit {
            preview_id: p.id,
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action: ImportAction::Merge {
                    match_index: 0,
                    fields: vec!["title".into()],
                },
            }],
        }),
    ) else {
        panic!()
    };
    assert_eq!(outcome.merged.len(), 1);
}
#[test]
fn pdf_offline_registration_partial_attachment_retry_without_second_paper_and_scope_errors() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("library");
    let client = InProcessLibraryClient::start(path.clone()).unwrap();
    let folder = root.join("selected");
    std::fs::create_dir(&folder).unwrap();
    let source = folder.join("offline-provisional.pdf");
    std::fs::write(&source, b"synthetic corrupt PDF; original bytes").unwrap();
    std::fs::copy(&source, folder.join("duplicate.pdf")).unwrap();
    std::fs::write(folder.join("not-pdf.txt"), "excluded").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source, folder.join("link.pdf")).unwrap();
    let report = call(|done| {
        client.pdf(
            PdfRequest::Inspect {
                selection: Selection::Folder {
                    path: folder,
                    recursive: false,
                },
                intent: PdfIntent::RegisterNew,
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap();
    let PdfReply::Report(report) = report else {
        panic!()
    };
    assert!(
        report
            .files
            .iter()
            .any(|f| f.disposition == FileDisposition::DuplicateHash)
    );
    assert!(
        report
            .files
            .iter()
            .any(|f| f.disposition == FileDisposition::NonPdf)
    );
    assert!(
        report
            .files
            .iter()
            .any(|f| f.disposition == FileDisposition::UnsafeLink)
    );
    let index = report
        .files
        .iter()
        .position(|f| f.disposition == FileDisposition::Ready)
        .unwrap();
    assert!(
        report.files[index]
            .messages
            .iter()
            .any(|m| m.contains("unresolved"))
    );
    let p = call(|done| {
        client.pdf(
            PdfRequest::PreviewNew {
                index,
                explicit: None,
                destination: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap();
    let PdfReply::Preview(p) = p else { panic!() };
    // Real filesystem obstruction after preview; paper commit must survive attachment failure.
    std::fs::write(path.join("attachments"), b"synthetic obstruction").unwrap();
    let PdfReply::Report(report) = call(|done| {
        client.pdf(
            PdfRequest::CommitNew {
                index,
                commit: create(p),
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap() else {
        panic!()
    };
    assert!(report.files[index].paper.is_some());
    assert!(report.files[index].attachment.is_none());
    assert!(matches!(
        report.files[index].disposition,
        FileDisposition::Failed(_)
    ));
    let original_partial = report.files[index].clone();
    let PdfReply::Report(reinspected) = call(|done| {
        client.pdf(
            PdfRequest::Inspect {
                selection: Selection::Files(vec![original_partial.path.clone()]),
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
    assert_eq!(reinspected.files[0].paper, original_partial.paper);
    assert!(
        call(|done| client.pdf(
            PdfRequest::PreviewNew {
                index: 0,
                explicit: None,
                destination: Default::default()
            },
            Box::new(|_| {}),
            done
        ))
        .is_err()
    );
    let index = 0;
    std::fs::remove_file(path.join("attachments")).unwrap();
    let PdfReply::Report(report) = call(|done| {
        client.pdf(
            PdfRequest::RetryAttachment {
                index,
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap() else {
        panic!()
    };
    assert!(report.files[index].attachment.is_some());
    let paper = report.files[index].paper.unwrap();
    let page = call(|done| client.query(Default::default(), done)).unwrap();
    assert_eq!(page.papers.len(), 1);
    let PdfReply::Attachments(attachments) =
        call(|done| client.pdf(PdfRequest::List(paper), Box::new(|_| {}), done)).unwrap()
    else {
        panic!()
    };
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].1, AttachmentStatus::Present);
    let cancel = Cancellation::default();
    cancel.cancel();
    let PdfReply::Report(report) = call(|done| {
        client.pdf(
            PdfRequest::Inspect {
                selection: Selection::Files(vec![source]),
                intent: PdfIntent::RegisterNew,
                cancel,
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap() else {
        panic!()
    };
    assert!(report.cancelled);
    assert!(report.files[0].disposition == FileDisposition::Cancelled);
}
#[test]
fn invalid_settings_fall_back_without_overwriting_unsafe_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("library");
    let client = InProcessLibraryClient::start(path.clone()).unwrap();
    call(|done| client.load_settings(done)).unwrap();
    let file = path.join("ui-settings.json");
    std::fs::write(&file, b"not JSON").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let loaded = call(|done| client.load_settings(done)).unwrap();
    assert!(loaded.warning.is_some());
    assert_eq!(loaded.settings, Default::default());
    std::fs::remove_file(&file).unwrap();
    let outside = root.join("untouched");
    std::fs::write(&outside, b"sentinel").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, &file).unwrap();
        assert!(call(|done| client.save_settings(Default::default(), done)).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"sentinel");
    }
}

#[test]
fn explicit_attachment_preserves_bibliography_cancel_is_partial_and_reopen_edits_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("library");
    let client = InProcessLibraryClient::start(path.clone()).unwrap();
    let mut input = NewPaper::titled("Confirmed bibliography");
    input.bibliography.doi = Some("10.1234/confirmed".into());
    let LibraryReply::Paper(before) =
        call(|done| client.submit(LibraryRequest::Create(Box::new(input)), done)).unwrap()
    else {
        panic!()
    };
    call(|done| client.lifecycle(before.id, true, done)).unwrap();
    assert_eq!(
        call(|done| client.submit(LibraryRequest::Get(before.id), done)).unwrap_err(),
        ServiceError::NotFound
    );
    let before = Box::new(call(|done| client.lifecycle(before.id, false, done)).unwrap());
    assert_eq!(before.title, "Confirmed bibliography");
    let paths = (0..3)
        .map(|n| {
            let source = root.join(format!("source-{n}.pdf"));
            std::fs::write(&source, format!("original synthetic bytes {n}")).unwrap();
            source
        })
        .collect();
    let PdfReply::Report(report) = call(|done| {
        client.pdf(
            PdfRequest::Inspect {
                selection: Selection::Files(paths),
                intent: PdfIntent::AttachExisting(before.id),
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap() else {
        panic!()
    };
    assert!(report.files.iter().all(|f| f.draft.is_none()));
    let cancel = Cancellation::default();
    let stop = cancel.clone();
    let PdfReply::Report(partial) = call(|done| {
        client.pdf(
            PdfRequest::Attach {
                indices: vec![0, 1, 2],
                paper: before.id,
                cancel,
            },
            Box::new(move |report| {
                if report.files.iter().any(|f| f.attachment.is_some()) {
                    stop.cancel();
                }
            }),
            done,
        )
    })
    .unwrap() else {
        panic!()
    };
    assert!(partial.cancelled);
    assert_eq!(
        partial
            .files
            .iter()
            .filter(|f| f.attachment.is_some())
            .count(),
        1
    );
    let PdfReply::Report(completed) = call(|done| {
        client.pdf(
            PdfRequest::Attach {
                indices: vec![1, 2],
                paper: before.id,
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    })
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        completed
            .files
            .iter()
            .filter(|f| f.attachment.is_some())
            .count(),
        3
    );
    let LibraryReply::Paper(after) =
        call(|done| client.submit(LibraryRequest::Get(before.id), done)).unwrap()
    else {
        panic!()
    };
    assert_eq!(before, after);
    assert_eq!(
        call(|done| client.query(Default::default(), done))
            .unwrap()
            .papers
            .len(),
        1
    );
    drop(client);
    let reopened = InProcessLibraryClient::start(path).unwrap();
    let PdfReply::Attachments(attachments) =
        call(|done| reopened.pdf(PdfRequest::List(before.id), Box::new(|_| {}), done)).unwrap()
    else {
        panic!()
    };
    assert_eq!(attachments.len(), 3);
    assert!(
        attachments
            .iter()
            .all(|(_, status)| *status == AttachmentStatus::Present)
    );
    let mut edit = *before;
    edit.title = "Later metadata correction".into();
    let LibraryReply::Paper(saved) =
        call(|done| reopened.submit(LibraryRequest::Update(Box::new(edit)), done)).unwrap()
    else {
        panic!()
    };
    assert_eq!(saved.title, "Later metadata correction");
    assert_eq!(saved.bibliography.doi.as_deref(), Some("10.1234/confirmed"));
}
