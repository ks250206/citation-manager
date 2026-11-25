//! #20 real pinned PDFium evidence for annotated export: DB-shaped plan -> `Export` (with
//! Rust-generated `/AP /N`) -> written file -> `Annotations` re-read -> `pdf_export::verify`,
//! on the rotated/CropBox/Unicode corpus and on a source that already has annotations.
//! Also leaves `export.pdf`, `legacy-export.pdf` and `expected.json` under
//! `target/pdf-export-test-<pid>/` for `scripts/pdfium/validate_export.py` (pypdf + poppler).
use cm_core::{
    annotations::{AnnotationId, rgb},
    geometry::{Origin, Point, Quad, Rect, Rotation, Viewport},
    pdf_export::*,
};
use cm_domain::PageIndex;
use cm_pdf::worker::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(30);

fn id(n: u128) -> AnnotationId {
    serde_json::from_value(json!(format!("00000000-0000-7000-8000-{n:012}"))).unwrap()
}
fn source(path: &Path) -> Source {
    Source {
        path: path.to_path_buf(),
        password: None,
    }
}
fn call(w: &PdfWorker, op: Operation) -> Result<Output, PdfError> {
    w.submit(op, Cancellation::default(), WAIT)?.wait(WAIT)
}
fn page(w: &PdfWorker, path: &Path, i: u32, zoom: Option<f32>) -> PageSnapshot {
    match call(
        w,
        Operation::Page {
            source: source(path),
            page_index: PageIndex::try_from(i).unwrap(),
            zoom,
        },
    )
    .unwrap()
    {
        Output::Page(p) => *p,
        _ => panic!("expected page"),
    }
}
fn read(w: &PdfWorker, path: &Path) -> AnnotationsSnapshot {
    match call(
        w,
        Operation::Annotations {
            source: source(path),
        },
    )
    .unwrap()
    {
        Output::Annotations(a) => *a,
        _ => panic!("expected annotations"),
    }
}
/// Same conversion as the daemon (`desktop_worker::pdf_export`).
fn ops(plan: &ExportPlan) -> (Vec<Highlight>, Vec<Note>) {
    (
        plan.highlights
            .iter()
            .map(|h| Highlight {
                name: pdf_name(h.id),
                page_index: PageIndex::try_from(h.page_index).unwrap(),
                quads: h.quads.clone(),
                comment: h.contents.clone(),
                color: h.color,
                opacity: h.opacity,
            })
            .collect(),
        plan.notes
            .iter()
            .map(|n| Note {
                name: pdf_name(n.id),
                page_index: PageIndex::try_from(n.page_index).unwrap(),
                bounds: n.at,
                contents: n.contents.clone(),
                color: n.color,
            })
            .collect(),
    )
}
fn export(w: &PdfWorker, path: &Path, plan: &ExportPlan) -> Result<NativeExport, PdfError> {
    let (highlights, notes) = ops(plan);
    match call(
        w,
        Operation::Export {
            source: source(path),
            highlights,
            notes,
        },
    )? {
        Output::Export(e) => Ok(e),
        _ => panic!("expected export"),
    }
}
fn sha(path: &Path) -> [u8; 32] {
    Sha256::digest(fs::read(path).unwrap()).into()
}
fn write_new(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
/// One axis-aligned line box per text line around baseline `y`, from PDFium's glyph boxes.
fn line_quad(p: &PageSnapshot, y: f32) -> Quad {
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
fn degrees(r: Rotation) -> u32 {
    match r {
        Rotation::R0 => 0,
        Rotation::R90 => 90,
        Rotation::R180 => 180,
        Rotation::R270 => 270,
    }
}
fn quad_json(q: &Quad) -> serde_json::Value {
    json!(q.0.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>())
}
fn rect_json(r: Rect) -> serde_json::Value {
    json!([r.left, r.bottom, r.right, r.top])
}
fn expected_json(plan: &ExportPlan, notes: &[Rect]) -> serde_json::Value {
    json!({
        "highlights": plan.highlights.iter().map(|h| json!({
            "name": pdf_name(h.id), "page": h.page_index, "contents": h.contents,
            "color": h.color, "opacity": h.opacity,
            "quads": h.quads.iter().map(quad_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "notes": plan.notes.iter().zip(notes).map(|(n, r)| json!({
            "name": pdf_name(n.id), "page": n.page_index, "contents": n.contents,
            "color": n.color, "rect": rect_json(*r),
        })).collect::<Vec<_>>(),
    })
}
/// Share of pixels in `area` (PDF space) whose colour is within 24 of `expected`.
fn coverage(p: &PageSnapshot, area: Rect, expected: [f32; 3]) -> f32 {
    let b = p.bitmap.as_ref().unwrap();
    let v = Viewport::new(p.crop, p.rotation, 1.0, Origin::TopLeft).unwrap();
    let corners = [
        v.to_display(Point {
            x: area.left,
            y: area.bottom,
        }),
        v.to_display(Point {
            x: area.right,
            y: area.top,
        }),
    ];
    let (x0, x1) = (
        corners[0].x.min(corners[1].x),
        corners[0].x.max(corners[1].x),
    );
    let (y0, y1) = (
        corners[0].y.min(corners[1].y),
        corners[0].y.max(corners[1].y),
    );
    let (mut hits, mut total) = (0, 0);
    for y in (y0.ceil() as u32 + 1)..(y1.floor() as u32).saturating_sub(1) {
        for x in (x0.ceil() as u32 + 1)..(x1.floor() as u32).saturating_sub(1) {
            let i = ((y * b.width + x) * 4) as usize;
            total += 1;
            if (0..3).all(|c| (f32::from(b.rgba[i + c]) - expected[c]).abs() <= 24.0) {
                hits += 1;
            }
        }
    }
    hits as f32 / total.max(1) as f32
}

#[test]
fn annotated_export_round_trips_on_the_pinned_runtime() {
    let library = PathBuf::from(
        std::env::var_os("CM_PDFIUM_LIBRARY")
            .expect("explicit pinned runtime fixture path required"),
    );
    let w = PdfWorker::start(library).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pdf")
        .canonicalize()
        .unwrap();
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/pdf-export-test-{}",
        std::process::id()
    ));
    fs::create_dir_all(&out).unwrap();
    let out = out.canonicalize().unwrap();
    let corpus = root.join("corpus.pdf");
    let corpus_hash = sha(&corpus);

    // DB-shaped plan: two-line highlights with Unicode comments on the four rotated pages
    // (CropBox [20,30,380,470]), page-level notes stacked from the visible corner, one note
    // anchored at a position.
    let palette = ["yellow", "green", "blue", "pink"];
    let mut plan = ExportPlan::default();
    let mut pages = Vec::new();
    for i in 0..4u32 {
        let p = page(&w, &corpus, i, None);
        pages.push(json!({"index": i, "rotation": degrees(p.rotation), "crop": rect_json(p.crop)}));
        plan.highlights.push(PlannedHighlight {
            id: id(u128::from(i) + 1),
            page_index: i,
            quads: vec![line_quad(&p, 410.), line_quad(&p, 382.)],
            color: rgb(Some(palette[i as usize])),
            opacity: HIGHLIGHT_OPACITY,
            contents: format!("検証コメント αβ café 頁{}", i + 1),
        });
        plan.notes.push(PlannedNote {
            id: id(u128::from(i) + 11),
            page_index: i,
            at: None,
            color: rgb(Some("orange")),
            contents: format!("独立ノート 日本語 {}", i + 1),
        });
    }
    plan.notes.push(PlannedNote {
        id: id(21),
        page_index: 0,
        at: None,
        color: rgb(None),
        contents: "二つ目のノート".into(),
    });
    plan.notes.push(PlannedNote {
        id: id(22),
        page_index: 1,
        // Partly beyond the crop's right edge: shifted inside, never dropped.
        at: Some(Rect {
            left: 370.,
            bottom: 200.,
            right: 394.,
            top: 224.,
        }),
        color: rgb(Some("purple")),
        contents: "anchored note ✓".into(),
    });

    let before = read(&w, &corpus);
    assert!(before.annotations.is_empty());
    assert_eq!(before.sha256, corpus_hash);
    let exported = export(&w, &corpus, &plan).unwrap();
    assert_eq!(
        exported.appearance,
        AppearanceStatus::NormalAppearanceGenerated
    );
    // Auto-placed notes: visible top-left of each (rotated) page, stacked per page.
    let p0 = page(&w, &corpus, 0, None);
    assert_eq!(
        exported.note_rects[0],
        note_slot(p0.crop, p0.rotation, 0).unwrap()
    );
    assert_eq!(
        exported.note_rects[4],
        note_slot(p0.crop, p0.rotation, 1).unwrap()
    );
    for i in 1..4 {
        let p = page(&w, &corpus, i, None);
        assert_eq!(
            exported.note_rects[i as usize],
            note_slot(p.crop, p.rotation, 0).unwrap()
        );
    }
    assert_eq!(
        exported.note_rects[5],
        Rect {
            left: 356.,
            bottom: 200.,
            right: 380.,
            top: 224.
        }
    );
    let path = out.join("export.pdf");
    write_new(&path, &exported.bytes);
    let after = read(&w, &path);
    assert_eq!(after.page_count, 6);
    let verified = verify(
        &plan,
        &exported.note_rects,
        &before.annotations,
        &after.annotations,
    )
    .unwrap_or_else(|problems| panic!("{problems:#?}"));
    assert_eq!(
        (verified.highlights, verified.notes, verified.preserved),
        (4, 6, 0)
    );
    assert!(verified.max_position_error <= POSITION_TOLERANCE);
    // A deliberately wrong expectation is caught by the same re-read (no vacuous pass).
    let mut wrong = plan.clone();
    wrong.highlights[1].quads[0].0[0].x += 1.0;
    assert!(
        verify(
            &wrong,
            &exported.note_rects,
            &before.annotations,
            &after.annotations
        )
        .is_err()
    );

    // The persisted appearances are what PDFium draws: highlight colour at 50% over white,
    // note icons in their colour, at the same display positions on every rotation.
    for i in 0..4u32 {
        let p = page(&w, &path, i, Some(1.0));
        let h = &plan.highlights[i as usize];
        let blend = h.color.map(|c| 255.0 - (255.0 - f32::from(c)) * h.opacity);
        let shade =
            coverage(&p, h.quads[0].bounds(), blend).min(coverage(&p, h.quads[1].bounds(), blend));
        assert!(shade > 0.2, "page {i} highlight shade {shade}");
        let note = exported.note_rects[i as usize];
        let icon = coverage(&p, note, plan.notes[i as usize].color.map(f32::from));
        assert!(icon > 0.4, "page {i} note icon {icon}");
        let original = page(&w, &corpus, i, Some(1.0));
        assert!(coverage(&original, note, plan.notes[i as usize].color.map(f32::from)) < 0.01);
    }

    // A source that already has annotations: a Highlight without /AP and a Square with one,
    // on a /Rotate 90 page. Both must come back byte-for-byte equivalent (no /AP added, no
    // recolouring) with the new annotations appended after them.
    let legacy = out.join("legacy.pdf");
    write_new(&legacy, &legacy_pdf());
    let legacy_hash = sha(&legacy);
    let legacy_before = read(&w, &legacy);
    assert_eq!(legacy_before.annotations.len(), 2);
    assert!(legacy_before.annotations[0].appearance.is_none());
    assert!(legacy_before.annotations[1].appearance.is_some());
    let legacy_plan = ExportPlan {
        highlights: vec![PlannedHighlight {
            id: id(31),
            page_index: 0,
            quads: vec![
                Rect {
                    left: 40.,
                    bottom: 200.,
                    right: 140.,
                    top: 214.,
                }
                .quad(),
            ],
            color: rgb(Some("green")),
            opacity: HIGHLIGHT_OPACITY,
            contents: String::new(),
        }],
        notes: vec![PlannedNote {
            id: id(32),
            page_index: 0,
            at: None,
            color: rgb(None),
            contents: "legacy note".into(),
        }],
        omitted: vec![],
    };
    let legacy_export = export(&w, &legacy, &legacy_plan).unwrap();
    let legacy_path = out.join("legacy-export.pdf");
    write_new(&legacy_path, &legacy_export.bytes);
    let legacy_after = read(&w, &legacy_path);
    let v = verify(
        &legacy_plan,
        &legacy_export.note_rects,
        &legacy_before.annotations,
        &legacy_after.annotations,
    )
    .unwrap_or_else(|problems| panic!("{problems:#?}"));
    assert_eq!(v.preserved, 2);
    assert_eq!(legacy_after.annotations[0], legacy_before.annotations[0]);
    assert_eq!(legacy_after.annotations[1], legacy_before.annotations[1]);
    // Exporting the export again keeps the first round's annotations (now with /AP) intact.
    let second = ExportPlan {
        highlights: vec![],
        notes: vec![PlannedNote {
            id: id(41),
            page_index: 2,
            at: None,
            color: rgb(Some("blue")),
            contents: "round two".into(),
        }],
        omitted: vec![],
    };
    let again = export(&w, &path, &second).unwrap();
    let again_path = out.join("export-again.pdf");
    write_new(&again_path, &again.bytes);
    let v = verify(
        &second,
        &again.note_rects,
        &after.annotations,
        &read(&w, &again_path).annotations,
    )
    .unwrap_or_else(|problems| panic!("{problems:#?}"));
    assert_eq!(v.preserved, 10);

    // Explicit failures, never a partial success.
    let encrypted = root.join("encrypted.pdf");
    assert_eq!(
        export(&w, &encrypted, &legacy_plan).err(),
        Some(PdfError::PasswordRequiredOrIncorrect)
    );
    assert_eq!(
        export(&w, &root.join("malformed.pdf"), &legacy_plan).err(),
        Some(PdfError::Malformed)
    );
    let mut off_page = legacy_plan.clone();
    off_page.highlights[0].page_index = 9;
    assert_eq!(
        export(&w, &corpus, &off_page).err(),
        Some(PdfError::InvalidPage)
    );
    let mut outside = legacy_plan.clone();
    outside.notes[0].at = Some(Rect {
        left: 1.,
        bottom: 1.,
        right: 10.,
        top: 10.,
    });
    assert_eq!(
        export(&w, &corpus, &outside).err(),
        Some(PdfError::InvalidGeometry)
    );
    let cancel = Cancellation::default();
    cancel.cancel();
    let (highlights, notes) = ops(&plan);
    assert!(matches!(
        w.submit(
            Operation::Export {
                source: source(&corpus),
                highlights,
                notes
            },
            cancel,
            WAIT
        )
        .unwrap()
        .wait(WAIT),
        Err(PdfError::Cancelled)
    ));

    // Originals are only ever read.
    assert_eq!(sha(&corpus), corpus_hash);
    assert_eq!(sha(&legacy), legacy_hash);
    fs::write(
        out.join("expected.json"),
        serde_json::to_vec_pretty(&json!({
            "source": corpus, "source_sha256": format!("{:x}", Sha256::digest(fs::read(&corpus).unwrap())),
            "pages": pages,
            "export": expected_json(&plan, &exported.note_rects),
            "legacy": {
                "source": legacy,
                "export": expected_json(&legacy_plan, &legacy_export.note_rects),
                "preserved": ["legacy-highlight", "legacy-square"],
            },
        }))
        .unwrap(),
    )
    .unwrap();
    w.shutdown().unwrap();
}

/// Minimal hand-written PDF (CC0, generated here): /Rotate 90 page with an AP-less blue
/// Highlight and a red Square that has its own appearance.
fn legacy_pdf() -> Vec<u8> {
    let square = "0 0 1 rg 150 150 50 50 re f\n";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Rotate 90 /Annots [4 0 R 5 0 R] >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /Highlight /Rect [10 10 100 30] /QuadPoints [10 30 100 30 10 10 100 10] /C [0 0 1] /CA 1 /Contents (legacy) /NM (legacy-highlight) /F 4 >>".to_owned(),
        "<< /Type /Annot /Subtype /Square /Rect [150 150 200 200] /C [1 0 0] /NM (legacy-square) /F 4 /AP << /N 6 0 R >> >>".to_owned(),
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [150 150 200 200] /Length {} >>\nstream\n{square}endstream",
            square.len()
        ),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}
