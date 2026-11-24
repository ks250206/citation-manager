//! #20 DB -> annotated PDF -> re-read through the real worker, storage and pinned PDFium.
//! Requires `CM_PDFIUM_LIBRARY` and the explicit `pdfium-runtime-tests` feature.
use cm_daemon::{desktop_worker::InProcessLibraryClient, fulltext::pdf_runtime};
use cm_pdf::worker::{Cancellation as PdfCancellation, Operation, Output, Source as PdfSource};
use cm_protocol::{
    annotations::*,
    attachments::*,
    domain::{AttachmentId, PaperId},
    geometry::Rect,
    library::*,
};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

fn call<T: Send + 'static>(submit: impl FnOnce(Reply<T>) -> Result<(), ServiceError>) -> T {
    let (tx, rx) = mpsc::channel();
    submit(Box::new(move |r| {
        let _ = tx.send(r);
    }))
    .unwrap();
    rx.recv_timeout(Duration::from_secs(120)).unwrap().unwrap()
}
fn paper(c: &InProcessLibraryClient, title: &str) -> PaperId {
    let LibraryReply::Paper(p) = call(|done| {
        c.submit(
            LibraryRequest::Create(Box::new(NewPaper::titled(title))),
            done,
        )
    }) else {
        panic!()
    };
    p.id
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
    }) else {
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
    }) else {
        panic!()
    };
    r.files[0].attachment.expect("attached")
}
fn create(c: &InProcessLibraryClient, input: NewAnnotation) -> AnnotationId {
    let AnnotationReply::Edit(edit) =
        call(|d| c.annotations(AnnotationRequest::Create(Box::new(input)), d))
    else {
        panic!()
    };
    let Edit::Created(a) = *edit else { panic!() };
    a.id
}
fn export(
    c: &InProcessLibraryClient,
    attachment: AttachmentId,
    destination: PathBuf,
    replace_existing: bool,
    allow_omissions: bool,
) -> Result<Box<PdfExportReport>, PdfExportFailure> {
    match call(|d| {
        c.pdf(
            PdfRequest::Export(PdfExportRequest {
                attachment,
                destination,
                replace_existing,
                allow_omissions,
                cancel: Cancellation::default(),
            }),
            Box::new(|_| {}),
            d,
        )
    }) {
        PdfReply::Exported(result) => result,
        _ => panic!("unexpected reply"),
    }
}
fn highlight(attachment: AttachmentId, page: u32, comment: &str) -> NewAnnotation {
    let line = |bottom: f32| {
        Rect {
            left: 44.,
            bottom,
            right: 330.,
            top: bottom + 20.,
        }
        .quad()
    };
    NewAnnotation {
        attachment_id: attachment,
        kind: AnnotationKind::Highlight,
        source: Source::Human,
        anchor: Anchor::Resolved {
            page_index: page,
            page_label: None,
            quads: vec![line(406.), line(378.)],
        },
        quote: "Unicode 日本語 αβ café second line".into(),
        context_before: None,
        context_after: None,
        color: Some(["yellow", "green", "blue", "pink"][page as usize % 4].into()),
        comment: Some(comment.into()),
    }
}
fn note(attachment: AttachmentId, page: u32, text: &str) -> NewAnnotation {
    NewAnnotation {
        kind: AnnotationKind::Note,
        anchor: Anchor::Resolved {
            page_index: page,
            page_label: None,
            quads: vec![],
        },
        quote: String::new(),
        color: None,
        comment: Some(text.into()),
        ..highlight(attachment, page, "")
    }
}
fn stored_blobs(library: &Path) -> Vec<(PathBuf, [u8; 32])> {
    let mut out = Vec::new();
    let mut stack = vec![library.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "pdf") {
                let hash = Sha256::digest(std::fs::read(&path).unwrap()).into();
                out.push((path, hash));
            }
        }
    }
    out.sort();
    out
}
fn names(path: &Path) -> Vec<(u32, String, Option<String>, Option<String>)> {
    let worker = pdf_runtime::shared(None).unwrap();
    let Output::Annotations(snapshot) = worker
        .submit(
            Operation::Annotations {
                source: PdfSource {
                    path: path.into(),
                    password: None,
                },
            },
            PdfCancellation::default(),
            Duration::from_secs(30),
        )
        .unwrap()
        .wait(Duration::from_secs(31))
        .unwrap()
    else {
        panic!()
    };
    snapshot
        .annotations
        .into_iter()
        .map(|a| (a.page_index, a.subtype, a.name, a.contents))
        .collect()
}

#[test]
fn library_annotations_export_to_a_verified_copy_and_the_original_is_untouched() {
    let library_path = PathBuf::from(
        std::env::var_os("CM_PDFIUM_LIBRARY").expect("explicit pinned runtime path required"),
    );
    pdf_runtime::shared(Some(&library_path)).expect("pinned PDFium loads");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let library = root.join("library");
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let client = InProcessLibraryClient::start(library.clone()).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pdf")
        .canonicalize()
        .unwrap();
    let corpus = fixtures.join("corpus.pdf");
    let corpus_hash: [u8; 32] = Sha256::digest(std::fs::read(&corpus).unwrap()).into();
    let paper_id = paper(&client, "Annotated");
    let attachment = attach(&client, paper_id, &corpus);
    let mut ids = Vec::new();
    for page in 0..4 {
        ids.push(create(
            &client,
            highlight(attachment, page, &format!("検証コメント αβ café {page}")),
        ));
        ids.push(create(
            &client,
            note(attachment, page, &format!("独立ノート 日本語 {page}")),
        ));
    }
    let unresolved = create(
        &client,
        NewAnnotation {
            anchor: Anchor::Unresolved {
                reason: "Copied from a replaced PDF; confirm its position".into(),
            },
            ..highlight(attachment, 0, "unplaced")
        },
    );
    let blobs = stored_blobs(&library);
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].1, corpus_hash);

    let copy = out.join("Annotated – 注釈付き.pdf");
    match export(&client, attachment, copy.clone(), false, false) {
        Err(PdfExportFailure::Unexportable(omitted)) => {
            assert_eq!(omitted.len(), 1);
            assert_eq!(omitted[0].id, unresolved);
        }
        other => panic!("{other:?}"),
    }
    assert!(!copy.exists());

    let report = export(&client, attachment, copy.clone(), false, true).unwrap();
    assert_eq!(
        (report.highlights, report.notes, report.preserved),
        (4, 4, 0)
    );
    assert!(!report.complete());
    assert_eq!(report.omitted[0].id, unresolved);
    assert_eq!(
        report.original_sha256,
        corpus_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert!(report.max_position_error <= 0.01);
    assert!(!report.replaced);
    // Independent re-read of the copy: every DB annotation by /NM, with its Unicode text.
    let read = names(&copy);
    assert_eq!(read.len(), 8);
    for (i, id) in ids.iter().enumerate() {
        let (page, subtype, _, contents) = read
            .iter()
            .find(|a| a.2.as_deref() == Some(id.0.to_string().as_str()))
            .unwrap_or_else(|| panic!("{id:?} missing"));
        assert_eq!(*page as usize, i / 2);
        assert_eq!(subtype, if i % 2 == 0 { "Highlight" } else { "Text" });
        assert!(contents.as_deref().unwrap().contains(if i % 2 == 0 {
            "検証コメント αβ café"
        } else {
            "独立ノート 日本語"
        }));
    }
    // Existing destination: refused unless the person confirmed replacing it.
    match export(&client, attachment, copy.clone(), false, true) {
        Err(PdfExportFailure::Destination(reason)) => assert!(reason.contains("already exists")),
        other => panic!("{other:?}"),
    }
    let replaced = export(&client, attachment, copy.clone(), true, true).unwrap();
    assert!(replaced.replaced);
    // Only the requested file exists; no temporary/partial files remain.
    assert_eq!(
        std::fs::read_dir(&out).unwrap().count(),
        1,
        "{:?}",
        std::fs::read_dir(&out).unwrap().collect::<Vec<_>>()
    );

    // The copy as a new source: its annotations are preserved and new ones appended.
    let second = paper(&client, "Second round");
    let copied = attach(&client, second, &copy);
    create(&client, note(copied, 2, "round two"));
    let again = export(&client, copied, out.join("again.pdf"), false, false).unwrap();
    assert_eq!((again.highlights, again.notes, again.preserved), (0, 1, 8));
    assert!(again.complete());

    // Encrypted and damaged sources fail explicitly and write nothing.
    let locked = paper(&client, "Locked");
    let encrypted = attach(&client, locked, &fixtures.join("encrypted.pdf"));
    assert_eq!(
        export(&client, encrypted, out.join("locked.pdf"), false, false).unwrap_err(),
        PdfExportFailure::Encrypted
    );
    assert!(!out.join("locked.pdf").exists());

    // Cancelling while the export runs: either it completed first, or nothing was written.
    let cancel = Cancellation::default();
    let cancelled = out.join("cancelled.pdf");
    let (tx, rx) = mpsc::channel();
    client
        .pdf(
            PdfRequest::Export(PdfExportRequest {
                attachment,
                destination: cancelled.clone(),
                replace_existing: false,
                allow_omissions: true,
                cancel: cancel.clone(),
            }),
            Box::new(|_| {}),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
    cancel.cancel();
    match rx.recv_timeout(Duration::from_secs(120)).unwrap().unwrap() {
        PdfReply::Exported(Err(PdfExportFailure::Cancelled)) => assert!(!cancelled.exists()),
        PdfReply::Exported(Ok(report)) => assert!(report.path.exists()),
        other => panic!("{other:?}"),
    }
    let leftovers: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Stored originals are byte-identical after every export.
    let after = stored_blobs(&library);
    assert_eq!(after[..].iter().filter(|b| b.1 == corpus_hash).count(), 1);
    assert_eq!(
        after.iter().find(|b| b.0 == blobs[0].0).unwrap().1,
        corpus_hash
    );
    assert_eq!(
        <[u8; 32]>::from(Sha256::digest(std::fs::read(&corpus).unwrap())),
        corpus_hash
    );
}
