//! #20 annotated PDF export through the real worker and storage, without a PDF runtime:
//! destination rules, explicit omissions, cancellation and stored-blob verification all
//! fail closed before PDFium is involved, and nothing is ever written. The PDFium round trip
//! is `pdf_export_runtime` (feature `pdfium-runtime-tests`) and cm-pdf's `export_runtime`.
use cm_daemon::desktop_worker::InProcessLibraryClient;
use cm_protocol::{
    annotations::*,
    attachments::*,
    domain::{AttachmentId, PaperId},
    geometry::Rect,
    library::*,
};
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
    rx.recv_timeout(Duration::from_secs(30)).unwrap().unwrap()
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
    r.files[0].attachment.unwrap()
}
fn create(c: &InProcessLibraryClient, input: NewAnnotation) {
    let AnnotationReply::Edit(_) =
        call(|d| c.annotations(AnnotationRequest::Create(Box::new(input)), d))
    else {
        panic!()
    };
}
fn export(
    c: &InProcessLibraryClient,
    attachment: AttachmentId,
    destination: PathBuf,
    replace_existing: bool,
    allow_omissions: bool,
    cancel: Cancellation,
) -> Result<Box<PdfExportReport>, PdfExportFailure> {
    match call(|d| {
        c.pdf(
            PdfRequest::Export(PdfExportRequest {
                attachment,
                destination,
                replace_existing,
                allow_omissions,
                cancel,
            }),
            Box::new(|_| {}),
            d,
        )
    }) {
        PdfReply::Exported(result) => result,
        _ => panic!("unexpected reply"),
    }
}
fn destination_error(result: Result<Box<PdfExportReport>, PdfExportFailure>, needle: &str) {
    match result {
        Err(PdfExportFailure::Destination(reason)) => {
            assert!(reason.contains(needle), "{reason}")
        }
        other => panic!("expected destination error ({needle}): {other:?}"),
    }
}
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn export_requests_fail_closed_before_any_file_is_written() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let library = root.join("library");
    let client = InProcessLibraryClient::start(library.clone()).unwrap();
    let LibraryReply::Paper(paper) = call(|done| {
        client.submit(
            LibraryRequest::Create(Box::new(NewPaper::titled("Export"))),
            done,
        )
    }) else {
        panic!()
    };
    let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pdf/corpus.pdf")
        .canonicalize()
        .unwrap();
    let attachment = attach(&client, paper.id, &corpus);
    let resolved = |page_index, quads| Anchor::Resolved {
        page_index,
        page_label: None,
        quads,
    };
    let base = NewAnnotation {
        attachment_id: attachment,
        kind: AnnotationKind::Highlight,
        source: Source::Human,
        anchor: resolved(
            0,
            vec![
                Rect {
                    left: 44.,
                    bottom: 406.,
                    right: 330.,
                    top: 426.,
                }
                .quad(),
            ],
        ),
        quote: "Unicode 日本語".into(),
        context_before: None,
        context_after: None,
        color: Some("green".into()),
        comment: Some("コメント".into()),
    };
    create(&client, base.clone());
    create(
        &client,
        NewAnnotation {
            kind: AnnotationKind::Note,
            anchor: resolved(1, vec![]),
            quote: String::new(),
            comment: Some("ノート".into()),
            ..base.clone()
        },
    );
    create(
        &client,
        NewAnnotation {
            anchor: Anchor::Unresolved {
                reason: "Copied from a replaced PDF; confirm its position".into(),
            },
            ..base.clone()
        },
    );
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let go = |path: PathBuf, replace, allow| {
        export(
            &client,
            attachment,
            path,
            replace,
            allow,
            Cancellation::default(),
        )
    };

    destination_error(go("relative.pdf".into(), false, true), "absolute");
    destination_error(
        go(library.join("copy.pdf"), false, true),
        "outside the library",
    );
    destination_error(
        go(root.join("missing/x.pdf"), false, true),
        "Cannot inspect",
    );
    let existing = out.join("existing.pdf");
    std::fs::write(&existing, b"keep me").unwrap();
    destination_error(go(existing.clone(), false, true), "already exists");
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
    #[cfg(unix)]
    {
        let link = out.join("link.pdf");
        std::os::unix::fs::symlink(&existing, &link).unwrap();
        destination_error(go(link, true, true), "ordinary file");
        let hard = out.join("hard.pdf");
        std::fs::hard_link(&existing, &hard).unwrap();
        destination_error(go(hard.clone(), true, true), "ordinary file");
        std::fs::remove_file(hard).unwrap();
    }
    let folder = out.join("folder.pdf");
    std::fs::create_dir(&folder).unwrap();
    destination_error(go(folder, true, true), "ordinary file");
    // Even a confirmed replacement only ever replaces a .pdf.
    let config = out.join("settings.toml");
    std::fs::write(&config, b"keep = true").unwrap();
    destination_error(go(config.clone(), true, true), ".pdf");
    assert_eq!(std::fs::read(&config).unwrap(), b"keep = true");

    // An unresolved annotation is never silently dropped: nothing is written unless the
    // person explicitly accepts an incomplete copy.
    match go(out.join("copy.pdf"), false, false) {
        Err(PdfExportFailure::Unexportable(omitted)) => {
            assert_eq!(omitted.len(), 1);
            assert!(omitted[0].reason.contains("replaced PDF"));
        }
        other => panic!("{other:?}"),
    }
    let cancel = Cancellation::default();
    cancel.cancel();
    assert_eq!(
        export(
            &client,
            attachment,
            out.join("copy.pdf"),
            false,
            true,
            cancel
        )
        .unwrap_err(),
        PdfExportFailure::Cancelled
    );
    let unknown = AttachmentId("00000000-0000-0000-0000-000000000099".parse().unwrap());
    assert_eq!(
        export(
            &client,
            unknown,
            out.join("copy.pdf"),
            false,
            true,
            Cancellation::default()
        )
        .unwrap_err(),
        PdfExportFailure::MissingOrCorrupt
    );
    // Valid request: this test binary has no bundled PDF runtime.
    assert!(matches!(
        go(out.join("copy.pdf"), false, true),
        Err(PdfExportFailure::RuntimeUnavailable(_))
    ));

    // A tampered stored original is refused before PDFium ever reads it.
    let stored = walk(&library)
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "pdf"))
        .expect("stored blob");
    let mut permissions = std::fs::metadata(&stored).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&stored, permissions).unwrap();
    std::fs::write(&stored, b"tampered").unwrap();
    assert_eq!(
        go(out.join("copy.pdf"), false, true).unwrap_err(),
        PdfExportFailure::MissingOrCorrupt
    );
    // Nothing besides the person's own files was ever created.
    let mut expected = vec![
        "existing.pdf".to_owned(),
        "folder.pdf".to_owned(),
        "settings.toml".to_owned(),
    ];
    if cfg!(unix) {
        expected.push("link.pdf".to_owned());
    }
    expected.sort();
    assert_eq!(entries(&out), expected);
    let LibraryReply::Paper(again) =
        call(|done| client.submit(LibraryRequest::Get(paper.id), done))
    else {
        panic!()
    };
    assert_eq!(again.title, "Export");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
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

/// The library refusal compares directory identity, so another path to the same directory
/// (here a bind mount, like a case-insensitive spelling or a mount alias) is refused too.
/// Needs permission to bind-mount (root in a container); elsewhere it reports the skip.
#[cfg(target_os = "linux")]
#[test]
fn an_alias_path_to_the_library_is_still_refused() {
    struct Unmount(PathBuf);
    impl Drop for Unmount {
        fn drop(&mut self) {
            let _ = std::process::Command::new("umount").arg(&self.0).status();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let library = root.join("library");
    let client = InProcessLibraryClient::start(library.clone()).unwrap();
    let LibraryReply::Paper(paper) = call(|done| {
        client.submit(
            LibraryRequest::Create(Box::new(NewPaper::titled("Alias"))),
            done,
        )
    }) else {
        panic!()
    };
    let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pdf/corpus.pdf")
        .canonicalize()
        .unwrap();
    let attachment = attach(&client, paper.id, &corpus);
    let alias = root.join("alias");
    std::fs::create_dir(&alias).unwrap();
    let mounted = std::process::Command::new("mount")
        .arg("--bind")
        .arg(&library)
        .arg(&alias)
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !mounted {
        eprintln!("SKIPPED: bind mount not permitted here; identity helper is unit-tested");
        return;
    }
    let _guard = Unmount(alias.clone());
    assert!(!alias.starts_with(&library), "no lexical prefix to catch");
    let export = |path: PathBuf| {
        export(
            &client,
            attachment,
            path,
            true,
            true,
            Cancellation::default(),
        )
    };
    destination_error(export(alias.join("copy.pdf")), "outside the library");
    let store = std::fs::read_dir(alias.join("attachments"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    destination_error(export(store.join("copy.pdf")), "outside the library");
    assert_eq!(
        walk(&library.join("attachments")).len(),
        1,
        "nothing written into the store"
    );
}
