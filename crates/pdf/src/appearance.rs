//! #20 persistent normal appearances (`/AP /N`) and names (`/NM`) for annotations this adapter
//! creates. PDFium 8125 saves new annotations without an appearance stream, and pdfium-render
//! 0.9.5 exposes `FPDFAnnot_SetAP` / `FPDFAnnot_SetStringValue` only as `unsafe` bindings with
//! no safe wrapper, so this module is the crate's single, audited `unsafe` exception
//! (see docs/ADR/pdf-export.md). Everything runs on the cm-pdf-serial thread; no handle escapes.
#![allow(unsafe_code)]
use crate::{
    geometry::{Quad, Rect},
    worker::PdfError,
};
use pdfium_render::prelude::*;
use std::fmt::Write;

/// `FPDF_ANNOT_APPEARANCEMODE_NORMAL` from fpdf_annot.h (pinned chromium/8125).
const NORMAL: FPDF_ANNOT_APPEARANCEMODE = 0;

fn n(v: f32) -> String {
    // Fixed precision keeps the stream ASCII and stable; 0.001 pt is far below tolerance.
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}
fn rgb(color: [u8; 3]) -> String {
    color.map(|c| n(f32::from(c) / 255.0)).join(" ")
}

/// Filled quads in page space (form BBox = annotation Rect, identity Matrix). `alpha_state`
/// selects `/GS`, the ExtGState `FPDFAnnot_SetAP` adds from `/CA` when it is below 1.
pub(crate) fn highlight(quads: &[Quad], color: [u8; 3], alpha_state: bool) -> String {
    let mut s = String::from("q\n");
    if alpha_state {
        s.push_str("/GS gs\n");
    }
    let _ = writeln!(s, "{} rg", rgb(color));
    for q in quads {
        // Z order TL, TR, BL, BR -> polygon TL, TR, BR, BL.
        let [tl, tr, bl, br] = q.0;
        let _ = writeln!(
            s,
            "{} {} m {} {} l {} {} l {} {} l h f",
            n(tl.x),
            n(tl.y),
            n(tr.x),
            n(tr.y),
            n(br.x),
            n(br.y),
            n(bl.x),
            n(bl.y)
        );
    }
    s.push_str("Q\n");
    s
}

/// A note icon filling `r`: coloured square, dark border and three text lines.
pub(crate) fn note(r: Rect, color: [u8; 3]) -> String {
    let (w, h) = (r.width(), r.height());
    let mut s = format!(
        "q\n{} rg\n{} {} {} {} re f\n0.2 G 1 w\n{} {} {} {} re S\n",
        rgb(color),
        n(r.left),
        n(r.bottom),
        n(w),
        n(h),
        n(r.left + 0.5),
        n(r.bottom + 0.5),
        n(w - 1.0),
        n(h - 1.0)
    );
    for i in 1..=3 {
        let y = r.bottom + h * i as f32 / 4.0;
        let _ = writeln!(
            s,
            "{} {} m {} {} l S",
            n(r.left + w * 0.2),
            n(y),
            n(r.right - w * 0.2),
            n(y)
        );
    }
    s.push_str("Q\n");
    s
}

/// Set `/AP /N` (and `/NM` when `name` is not empty) on the page's annotation `index`.
/// Call after the high-level wrapper for that annotation has been dropped and after its
/// colour is set: `FPDFAnnot_SetColor` refuses annotations that already have an appearance.
pub(crate) fn finish(
    page: &PdfPage<'_>,
    index: usize,
    name: &str,
    content: &str,
) -> Result<(), PdfError> {
    let bindings = page.bindings();
    let page_handle = bindings.get_handle_from_page(page);
    let index = i32::try_from(index).map_err(|_| PdfError::LimitExceeded)?;
    // SAFETY: `page_handle` belongs to the live `page` borrowed for this whole call on the
    // serial PDFium thread. `FPDFPage_GetAnnot` returns a new, owned annotation context (or
    // null, checked) that is closed exactly once below and never stored. The `_str` helpers
    // pass NUL-terminated UTF-16LE buffers that outlive each call; PDFium copies the values.
    unsafe {
        let annot = bindings.FPDFPage_GetAnnot(page_handle, index);
        if annot.is_null() {
            return Err(PdfError::NativeFailure);
        }
        let named =
            name.is_empty() || bindings.FPDFAnnot_SetStringValue_str(annot, "NM", name) != 0;
        let drawn = bindings.FPDFAnnot_SetAP_str(annot, NORMAL, content) != 0;
        bindings.FPDFPage_CloseAnnot(annot);
        if named && drawn {
            Ok(())
        } else {
            Err(PdfError::NativeFailure)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appearance_streams_are_ascii_polygons_in_page_space() {
        let q = Rect {
            left: 40.,
            bottom: 380.5,
            right: 300.25,
            top: 392.,
        }
        .quad();
        let s = highlight(&[q], [255, 255, 0], true);
        assert_eq!(
            s,
            "q\n/GS gs\n1 1 0 rg\n40 392 m 300.25 392 l 300.25 380.5 l 40 380.5 l h f\nQ\n"
        );
        assert!(!highlight(&[q], [0, 0, 0], false).contains("/GS"));
        let icon = note(
            Rect {
                left: 300.,
                bottom: 300.,
                right: 324.,
                top: 324.,
            },
            [255, 204, 0],
        );
        assert!(icon.starts_with("q\n1 0.8 0 rg\n300 300 24 24 re f\n"));
        assert!(icon.is_ascii() && icon.ends_with("Q\n"));
        assert_eq!(n(-0.0001), "0");
    }
}
