//! #17 render path through the real worker/storage, independent of the GUI. The default
//! test build has no PDF runtime, so successful rendering is covered by the native/Xvfb
//! checklist and the cm-pdf runtime tests; here we check validation, storage verification
//! and that a render request never blocks ordinary library work.
use cm_daemon::desktop_worker::InProcessLibraryClient;
use cm_protocol::{attachments::*, library::*};
use std::{sync::mpsc, time::Duration};

fn call<T: Send + 'static>(submit: impl FnOnce(Reply<T>) -> Result<(), ServiceError>) -> T {
    let (tx, rx) = mpsc::channel();
    submit(Box::new(move |r| {
        let _ = tx.send(r);
    }))
    .unwrap();
    rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap()
}

fn render(
    client: &InProcessLibraryClient,
    attachment: cm_protocol::domain::AttachmentId,
    zoom: f32,
) -> Result<Box<RenderedPage>, RenderFailure> {
    let reply = call(|done| {
        client.pdf(
            PdfRequest::Render(RenderRequest {
                attachment,
                page: 0,
                zoom,
                cancel: Cancellation::default(),
            }),
            Box::new(|_| {}),
            done,
        )
    });
    match reply {
        PdfReply::Page(result) => result,
        _ => panic!("unexpected reply"),
    }
}

#[test]
fn render_requests_validate_verify_storage_and_report_runtime_absence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let client = InProcessLibraryClient::start(root.join("library")).unwrap();
    let LibraryReply::Paper(paper) = call(|done| {
        client.submit(
            LibraryRequest::Create(Box::new(NewPaper::titled("Viewer"))),
            done,
        )
    }) else {
        panic!()
    };
    let corpus = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pdf/corpus.pdf")
        .canonicalize()
        .unwrap();
    let PdfReply::Report(_) = call(|done| {
        client.pdf(
            PdfRequest::Inspect {
                selection: Selection::Files(vec![corpus]),
                intent: PdfIntent::AttachExisting(paper.id),
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    }) else {
        panic!()
    };
    let PdfReply::Report(report) = call(|done| {
        client.pdf(
            PdfRequest::Attach {
                indices: vec![0],
                paper: paper.id,
                cancel: Default::default(),
            },
            Box::new(|_| {}),
            done,
        )
    }) else {
        panic!()
    };
    let attachment = report.files[0].attachment.expect("attached");

    // Out-of-range zoom is rejected before any work.
    assert!(matches!(
        render(&client, attachment, 0.0),
        Err(RenderFailure::Other(_))
    ));
    assert!(matches!(
        render(&client, attachment, f32::NAN),
        Err(RenderFailure::Other(_))
    ));
    // Unknown attachment.
    let unknown =
        cm_protocol::domain::AttachmentId("00000000-0000-0000-0000-000000000099".parse().unwrap());
    assert_eq!(
        render(&client, unknown, 1.0).unwrap_err(),
        RenderFailure::MissingOrCorrupt
    );
    // A cancelled request does no work.
    let cancel = Cancellation::default();
    cancel.cancel();
    let PdfReply::Page(cancelled) = call(|done| {
        client.pdf(
            PdfRequest::Render(RenderRequest {
                attachment,
                page: 0,
                zoom: 1.0,
                cancel,
            }),
            Box::new(|_| {}),
            done,
        )
    }) else {
        panic!()
    };
    assert_eq!(cancelled.unwrap_err(), RenderFailure::Cancelled);
    // Valid request: this build/test binary has no bundled PDF runtime.
    let result = render(&client, attachment, 1.0);
    #[cfg(not(feature = "pdf-metadata"))]
    assert!(matches!(result, Err(RenderFailure::RuntimeUnavailable(_))));
    #[cfg(feature = "pdf-metadata")]
    assert!(
        matches!(result, Err(RenderFailure::RuntimeUnavailable(_))),
        "test binaries are not in a bin/ bundle"
    );

    // Corrupting the stored blob is detected before PDFium ever sees it.
    let stored = walk(&root.join("library"))
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "pdf"))
        .expect("stored blob");
    let mut permissions = std::fs::metadata(&stored).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&stored, permissions).unwrap();
    std::fs::write(&stored, b"tampered").unwrap();
    assert_eq!(
        render(&client, attachment, 1.0).unwrap_err(),
        RenderFailure::MissingOrCorrupt
    );
    // Library work continues normally afterwards.
    let LibraryReply::Paper(again) =
        call(|done| client.submit(LibraryRequest::Get(paper.id), done))
    else {
        panic!()
    };
    assert_eq!(again.title, "Viewer");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
