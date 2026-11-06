use cm_domain::PageIndex;
use cm_pdf::{
    geometry::{Origin, Point, Rect, Viewport},
    worker::*,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const WAIT: Duration = Duration::from_secs(30);
fn index(i: u32) -> PageIndex {
    PageIndex::try_from(i).unwrap()
}
fn source(path: &Path) -> Source {
    Source {
        path: path.to_path_buf(),
        password: None,
    }
}
fn page_op(path: &Path, i: u32, zoom: Option<f32>) -> Operation {
    Operation::Page {
        source: source(path),
        page_index: index(i),
        zoom,
    }
}
fn call(w: &PdfWorker, op: Operation) -> Result<Output, PdfError> {
    w.submit(op, Cancellation::default(), WAIT)?.wait(WAIT)
}
pub fn page(w: &PdfWorker, path: &Path, i: u32, zoom: Option<f32>) -> PageSnapshot {
    match call(w, page_op(path, i, zoom)).unwrap() {
        Output::Page(p) => *p,
        _ => panic!("expected page"),
    }
}
pub fn ppm(path: &Path, b: &Bitmap) {
    let mut f = fs::File::create(path).unwrap();
    write!(f, "P6\n{} {}\n255\n", b.width, b.height).unwrap();
    for p in b.rgba.as_chunks::<4>().0.iter() {
        f.write_all(&p[..3]).unwrap();
    }
}
fn line_quad(p: &PageSnapshot, y: f32) -> cm_pdf::geometry::Quad {
    p.characters
        .iter()
        .filter(|c| {
            c.unicode.is_some_and(|c| !c.is_control())
                && c.bounds.bottom > y - 8.
                && c.bounds.bottom < y + 10.
        })
        .map(|c| c.quad.bounds())
        .filter(|r| r.valid())
        .reduce(|a, b| Rect {
            left: a.left.min(b.left),
            right: a.right.max(b.right),
            bottom: a.bottom.min(b.bottom),
            top: a.top.max(b.top),
        })
        .unwrap()
        .quad()
}
fn write_new(path: &Path, bytes: &[u8]) {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    f.write_all(bytes).unwrap();
}

/// Shared by explicit runtime integration test and report-producing example; missing dylib fails.
pub fn run(library: PathBuf, out: &Path) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let corpus = root.join("fixtures/pdf/corpus.pdf");
    fs::create_dir_all(out).unwrap();
    let out = &out.canonicalize().unwrap();
    let original_hash = format!("{:x}", Sha256::digest(fs::read(&corpus).unwrap()));
    assert!(matches!(
        PdfWorker::start(PathBuf::from("relative.dylib")),
        Err(PdfError::LibraryPath)
    ));
    assert!(matches!(
        PdfWorker::start(corpus.clone()),
        Err(PdfError::LibraryChecksum)
    ));
    let w = PdfWorker::start(library.clone()).unwrap();
    assert!(matches!(
        PdfWorker::start(library),
        Err(PdfError::AlreadyStarted)
    ));
    let start = Instant::now();
    let mut measurements = Vec::new();
    let mut pages = Vec::new();
    let mut highlights = Vec::new();
    let mut notes = Vec::new();
    for i in 0..4 {
        for zoom in [0.5, 1., 2.] {
            let t = Instant::now();
            let p = page(&w, &corpus, i, Some(zoom));
            assert!(p.text.contains("Unicode 日本語 αβ café"), "{:?}", p.text);
            assert!(p.text.contains("second line: coordinate proof"));
            assert_eq!(p.physical_page, i + 1);
            assert_eq!(p.page_count, 6);
            assert_eq!(
                p.page_label.as_deref(),
                Some(if i == 0 { "iv" } else { "S12" })
            );
            assert_eq!(
                p.crop,
                Rect {
                    left: 20.,
                    bottom: 30.,
                    right: 380.,
                    top: 470.
                }
            );
            let v = Viewport::new(p.crop, p.rotation, zoom, Origin::TopLeft).unwrap();
            let b = p.bitmap.as_ref().unwrap();
            assert_eq!((b.width, b.height), (v.size().0 as u32, v.size().1 as u32));
            assert_eq!(b.rgba.len(), b.width as usize * b.height as usize * 4);
            // Actual PDFium rendered marker must align with common geometry, not only invert itself.
            let predicted = v.to_display(Point { x: 46., y: 54. });
            let offset = (predicted.y.floor() as usize * b.width as usize
                + predicted.x.floor() as usize)
                * 4;
            assert!(
                b.rgba[offset] > 245 && b.rgba[offset + 1] < 10 && b.rgba[offset + 2] < 10,
                "physical marker mismatch page {i} zoom {zoom}: {predicted:?} {:?}",
                &b.rgba[offset..offset + 4]
            );
            let mut red_count = 0;
            let mut sum_x = 0.;
            let mut sum_y = 0.;
            for (pixel, rgba) in b.rgba.as_chunks::<4>().0.iter().enumerate() {
                if rgba[0] > 245 && rgba[1] < 10 && rgba[2] < 10 {
                    red_count += 1;
                    sum_x += (pixel % b.width as usize) as f32 + 0.5;
                    sum_y += (pixel / b.width as usize) as f32 + 0.5;
                }
            }
            assert!(red_count > 0);
            let error = ((sum_x / red_count as f32 - predicted.x).powi(2)
                + (sum_y / red_count as f32 - predicted.y).powi(2))
            .sqrt();
            assert!(error < 1., "marker centroid error {error}");
            measurements.push(json!({"page_index":i,"zoom":zoom,"width":b.width,"height":b.height,"milliseconds":t.elapsed().as_secs_f64()*1000.,"red_pixels":red_count,"marker_centroid_error_px":error}));
            if zoom == 1. {
                ppm(&out.join(format!("original-{i}.ppm")), b);
                pages.push(serde_json::to_value(&p).unwrap());
                highlights.push(Highlight {
                    page_index: index(i),
                    quads: vec![line_quad(&p, 410.), line_quad(&p, 382.)],
                    comment: "検証コメント αβ café".into(),
                });
                notes.push(Note {
                    page_index: index(i),
                    bounds: Rect {
                        left: 300.,
                        bottom: 300.,
                        right: 324.,
                        top: 324.,
                    },
                    contents: "独立ノート 日本語".into(),
                });
            }
        }
    }
    let image = page(&w, &corpus, 4, Some(1.));
    assert_eq!(image.text_status, TextStatus::NoExtractableText);
    assert!(image.text.is_empty());
    assert!(page(&w, &corpus, 5, None).text.contains("bottom boundary"));
    assert!(matches!(
        call(&w, page_op(&corpus, 6, None)),
        Err(PdfError::InvalidPage)
    ));
    assert!(matches!(
        call(&w, page_op(&corpus, u32::MAX - 1, None)),
        Err(PdfError::InvalidPage)
    ));
    assert!(matches!(
        call(&w, page_op(&corpus, 0, Some(1000.))),
        Err(PdfError::LimitExceeded)
    ));
    assert!(matches!(
        call(&w, page_op(&corpus, 0, Some(f32::NAN))),
        Err(PdfError::InvalidGeometry)
    ));
    let malformed = root.join("fixtures/pdf/malformed.pdf");
    assert!(matches!(
        call(&w, page_op(&malformed, 0, None)),
        Err(PdfError::Malformed)
    ));
    let encrypted = root.join("fixtures/pdf/encrypted.pdf");
    assert!(matches!(
        call(&w, page_op(&encrypted, 0, None)),
        Err(PdfError::PasswordRequiredOrIncorrect)
    ));
    let mut wrong = source(&encrypted);
    wrong.password = Some("wrong".into());
    assert!(matches!(
        call(
            &w,
            Operation::Page {
                source: wrong,
                page_index: index(0),
                zoom: None
            }
        ),
        Err(PdfError::PasswordRequiredOrIncorrect)
    ));
    let mut unlocked = source(&encrypted);
    unlocked.password = Some("test-password".into());
    let Output::Page(p) = call(
        &w,
        Operation::Page {
            source: unlocked,
            page_index: index(0),
            zoom: None,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert!(p.text.contains("日本語"));
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        w.submit(page_op(&corpus, 0, None), cancel, WAIT)
            .unwrap()
            .wait(WAIT),
        Err(PdfError::Cancelled)
    ));
    assert!(matches!(
        w.submit(
            page_op(&corpus, 0, None),
            Cancellation::default(),
            Duration::ZERO
        )
        .unwrap()
        .wait(WAIT),
        Err(PdfError::Deadline)
    ));
    // Cancel queued work after submission, not only pre-cancelled tokens. At least one queued
    // request must observe cancellation; a request already completed is allowed to win the race.
    let cancel = Cancellation::default();
    let mut pending = Vec::new();
    for _ in 0..8 {
        pending.push(
            w.submit(page_op(&corpus, 0, Some(2.)), cancel.clone(), WAIT)
                .unwrap(),
        );
    }
    cancel.cancel();
    let mut cancelled = 0;
    for ticket in pending {
        match ticket.wait(WAIT) {
            Err(PdfError::Cancelled) => cancelled += 1,
            Ok(_) => {}
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(cancelled > 0);
    // Oversized input is rejected before parser load, and recovery afterwards remains functional.
    let oversized = out.join("oversized.pdf");
    let f = fs::File::create(&oversized).unwrap();
    f.set_len(MAX_INPUT_BYTES as u64 + 1).unwrap();
    drop(f);
    assert!(matches!(
        call(&w, page_op(&oversized, 0, None)),
        Err(PdfError::LimitExceeded)
    ));
    fs::remove_file(oversized).unwrap();
    // Distinct requests from multiple callers all return the correct owned result on this one worker.
    std::thread::scope(|s| {
        for i in 0..4 {
            let w = &w;
            let path = &corpus;
            s.spawn(move || {
                for _ in 0..5 {
                    assert!(page(w, path, i, None).text.contains("日本語"));
                }
            });
        }
    });
    let invalid = Highlight {
        page_index: index(0),
        quads: vec![
            Rect {
                left: -1.,
                bottom: 30.,
                right: 20.,
                top: 50.,
            }
            .quad(),
        ],
        comment: "outside".into(),
    };
    assert!(matches!(
        call(
            &w,
            Operation::Export {
                source: source(&corpus),
                highlights: vec![invalid],
                notes: vec![]
            }
        ),
        Err(PdfError::InvalidGeometry)
    ));
    let Output::Export(export) = call(
        &w,
        Operation::Export {
            source: source(&corpus),
            highlights: highlights.clone(),
            notes: notes.clone(),
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        export.appearance,
        AppearanceStatus::RequiresExportCompletion
    );
    let bytes = export.bytes;
    let exported = out.join("pdfium-export.pdf");
    write_new(&exported, &bytes);
    let mut exported_pages = Vec::new();
    for i in 0..4 {
        let p = page(&w, &exported, i, Some(1.));
        assert_eq!(p.annotations.len(), 2);
        let h = p
            .annotations
            .iter()
            .find(|a| a.kind == "Highlight")
            .unwrap();
        assert_eq!(h.contents.as_deref(), Some("検証コメント αβ café"));
        assert_eq!(h.quads.len(), 2);
        for (q, expected) in h.quads.iter().zip(&highlights[i as usize].quads) {
            for (a, b) in q.0.iter().zip(expected.0) {
                assert!((a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01);
            }
        }
        assert!(
            p.annotations
                .iter()
                .any(|a| a.contents.as_deref() == Some("独立ノート 日本語"))
        );
        ppm(
            &out.join(format!("pdfium-export-{i}.ppm")),
            p.bitmap.as_ref().unwrap(),
        );
        exported_pages.push(serde_json::to_value(p).unwrap());
    }
    // Repeated failures/successes above release document handles; originals remain untouched.
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&corpus).unwrap())),
        original_hash
    );
    w.shutdown().unwrap();
    json!({"build":PDFIUM_BUILD,"abi":PDFIUM_ABI,"library_sha256":PDFIUM_SHA256,
        "original_sha256":original_hash,"export_sha256":format!("{:x}",Sha256::digest(bytes)),
        "measurements":measurements,"pages":pages,"exported_pages":exported_pages,
        "highlights":highlights,"notes":notes,"elapsed_ms":start.elapsed().as_secs_f64()*1000.,
        "statuses_checked":["image-only","malformed","password-missing","password-wrong","password-correct","invalid-page","oversized-render","invalid-zoom","cancelled","expired","outside-quad","20 concurrent caller requests", "post-submission cancellation", "oversized-input"],
        "limits":"cooperative between native calls; bounded queue/input/output/bitmap/chars; native allocation/time not sandboxed; process-global dylib remains loaded"})
}
