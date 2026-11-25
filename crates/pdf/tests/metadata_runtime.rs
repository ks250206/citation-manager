//! Explicit real pinned ABI evidence; no GUI, env fallback in product or package gate.
use cm_pdf::worker::*;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};
#[test]
fn real_pdf_metadata_digest_errors_and_cooperative_cancel() {
    let library = PathBuf::from(
        std::env::var_os("CM_PDFIUM_LIBRARY")
            .expect("explicit pinned runtime fixture path required"),
    );
    let worker = PdfWorker::start(library).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pdf")
        .canonicalize()
        .unwrap();
    let op = |name: &str| Operation::Metadata {
        source: Source {
            path: root.join(name),
            password: None,
        },
    };
    let call = |op| {
        worker
            .submit(op, Cancellation::default(), Duration::from_secs(10))
            .unwrap()
            .wait(Duration::from_secs(12))
    };
    let Output::Metadata(metadata) = call(op("corpus.pdf")).unwrap() else {
        panic!("metadata DTO")
    };
    assert_eq!(metadata.page_count, 6);
    assert!(metadata.draft.title.is_empty());
    assert_eq!(
        metadata.sha256,
        <[u8; 32]>::from(Sha256::digest(
            std::fs::read(root.join("corpus.pdf")).unwrap()
        ))
    );
    let library_fixtures = root.parent().unwrap().join("library");
    let metadata_op = |name: &str| Operation::Metadata {
        source: Source {
            path: library_fixtures.join(name),
            password: None,
        },
    };
    let Output::Metadata(info) = call(metadata_op("info.pdf")).unwrap() else {
        panic!()
    };
    assert_eq!(info.draft.title, "Info suggested title");
    assert_eq!(info.draft.bibliography.doi.as_deref(), Some("10.1234/info"));
    let Output::Metadata(xmp) = call(metadata_op("metadata.pdf")).unwrap() else {
        panic!()
    };
    assert_eq!(xmp.draft.title, "XMP suggested synthetic title");
    assert_eq!(xmp.draft.bibliography.authors.len(), 2);
    assert!(
        xmp.warnings
            .iter()
            .any(|w| w.contains("Conflicting embedded Info DOI"))
    );
    let Output::Metadata(scanned) = call(metadata_op("scanned.pdf")).unwrap() else {
        panic!()
    };
    assert!(scanned.draft.title.is_empty());
    assert_eq!(scanned.page_count, 1);
    assert!(matches!(
        call(op("encrypted.pdf")),
        Err(PdfError::PasswordRequiredOrIncorrect)
    ));
    assert!(matches!(
        call(op("malformed.pdf")),
        Err(PdfError::Malformed)
    ));
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        worker
            .submit(op("corpus.pdf"), cancel, Duration::from_secs(1))
            .unwrap()
            .wait(Duration::from_secs(2)),
        Err(PdfError::Cancelled)
    ));
    let Output::Metadata(recovered) = call(op("corpus.pdf")).unwrap() else {
        panic!()
    };
    assert_eq!(recovered.sha256, metadata.sha256);
    worker.shutdown().unwrap();
}
