//! #18 real pinned PDFium evidence for `Operation::Text`: page ranges, labels, the
//! image-only page, per-page character cap, encrypted/damaged errors and admission limits.
use cm_pdf::{extract::TextBatch, worker::*};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};

#[test]
fn bounded_page_text_extraction_on_the_pinned_runtime() {
    let library = PathBuf::from(
        std::env::var_os("CM_PDFIUM_LIBRARY")
            .expect("explicit pinned runtime fixture path required"),
    );
    let worker = PdfWorker::start(library).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .unwrap();
    let text = |path: PathBuf, password: Option<&str>, first: u32, max: u32, chars: u32| {
        worker
            .submit(
                Operation::Text {
                    source: Source {
                        path,
                        password: password.map(str::to_owned),
                    },
                    first_page: first,
                    max_pages: max,
                    max_chars: chars,
                },
                Cancellation::default(),
                Duration::from_secs(10),
            )?
            .wait(Duration::from_secs(12))
            .map(|output| match output {
                Output::Text(batch) => *batch,
                _ => panic!("expected text batch"),
            })
    };
    let corpus = root.join("pdf/corpus.pdf");
    let all: TextBatch = text(corpus.clone(), None, 0, 64, 100_000).unwrap();
    assert_eq!(all.page_count, 6);
    assert_eq!(all.pages.len(), 6);
    assert_eq!(
        all.sha256,
        <[u8; 32]>::from(Sha256::digest(std::fs::read(&corpus).unwrap()))
    );
    for (i, page) in all.pages.iter().enumerate() {
        assert_eq!(page.page_index, i as u32);
        assert!(!page.truncated);
    }
    for page in &all.pages[..4] {
        assert!(
            page.text.contains("Unicode 日本語 αβ café"),
            "{:?}",
            page.text
        );
        assert!(page.text.contains("second line: coordinate proof"));
    }
    assert_eq!(all.pages[0].page_label.as_deref(), Some("iv"));
    assert_eq!(all.pages[1].page_label.as_deref(), Some("S12"));
    assert!(
        all.pages[4].text.trim().is_empty(),
        "image-only page has no text layer"
    );
    assert!(!all.pages[5].text.trim().is_empty());

    let range = text(corpus.clone(), None, 4, 10, 100_000).unwrap();
    assert_eq!(
        range.pages.iter().map(|p| p.page_index).collect::<Vec<_>>(),
        vec![4, 5]
    );
    let past = text(corpus.clone(), None, 6, 1, 100_000).unwrap();
    assert!(past.pages.is_empty());
    let cut = text(corpus.clone(), None, 0, 1, 5).unwrap();
    assert!(cut.pages[0].truncated);
    assert_eq!(cut.pages[0].text.chars().count(), 5);

    let scanned = text(root.join("library/scanned.pdf"), None, 0, 64, 100_000).unwrap();
    assert_eq!(scanned.page_count, 1);
    assert!(scanned.pages[0].text.trim().is_empty());

    let encrypted = root.join("pdf/encrypted.pdf");
    assert_eq!(
        text(encrypted.clone(), None, 0, 1, 100).unwrap_err(),
        PdfError::PasswordRequiredOrIncorrect
    );
    assert_eq!(
        text(encrypted, Some("test-password"), 0, 1, 100_000)
            .unwrap()
            .page_count,
        1
    );
    assert_eq!(
        text(root.join("pdf/malformed.pdf"), None, 0, 1, 100).unwrap_err(),
        PdfError::Malformed
    );
    for (max, chars) in [(0, 10), (65, 10), (1, 0), (1, 1_000_001)] {
        assert!(matches!(
            text(corpus.clone(), None, 0, max, chars),
            Err(PdfError::LimitExceeded)
        ));
    }
    worker.shutdown().unwrap();
}
