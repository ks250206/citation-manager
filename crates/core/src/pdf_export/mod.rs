//! #20 annotated PDF export: which #19 annotations become which standard PDF annotations, and
//! whether a re-read export matches that plan.
//!
//! Pure rules only. The PDF engine (cm-pdf) writes and re-reads PDFs; the daemon owns files.
//! Positions are quads/rects in unrotated PDF user space (points), like `crate::annotations`.
//! Nothing here treats a partial or unverified output as success: `plan` lists every annotation
//! it cannot export and `verify` lists every mismatch it finds.
use crate::{
    annotations::{Anchor, Annotation, AnnotationId, AnnotationKind, rgb},
    geometry::{Origin, Point, Quad, Rect, Rotation, Viewport},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Constant highlight opacity (`/CA`); the DB stores colour only.
pub const HIGHLIGHT_OPACITY: f32 = 0.5;
/// Side of a note (Text annotation) icon, matching the 24 pt PDFKit normalisation seen in M0.
pub const NOTE_ICON: f32 = 24.0;
/// Inset of auto-placed note icons from the visible page corner, and the gap between them.
pub const NOTE_MARGIN: f32 = 8.0;
/// Largest coordinate difference accepted between the plan and the re-read PDF (points).
pub const POSITION_TOLERANCE: f32 = 0.01;
/// Largest per-channel difference accepted for appearance colours (0–255).
pub const COLOR_TOLERANCE: u8 = 1;
/// Largest difference accepted for appearance fill alpha (0–255).
pub const ALPHA_TOLERANCE: u8 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedHighlight {
    pub id: AnnotationId,
    pub page_index: u32,
    /// Axis-aligned Z-order quads (TL, TR, BL, BR), one per line.
    pub quads: Vec<Quad>,
    pub color: [u8; 3],
    pub opacity: f32,
    /// The highlight's comment as `/Contents` (empty when it has none).
    pub contents: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedNote {
    pub id: AnnotationId,
    pub page_index: u32,
    /// Requested icon rectangle; `None` (page-level note) lets the engine place it in the
    /// visible top-left corner of the page (`note_slot`).
    pub at: Option<Rect>,
    pub color: [u8; 3],
    pub contents: String,
}
/// An annotation that is not in the exported PDF, and why. Always shown to the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Omitted {
    pub id: AnnotationId,
    pub kind: AnnotationKind,
    pub reason: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportPlan {
    pub highlights: Vec<PlannedHighlight>,
    pub notes: Vec<PlannedNote>,
    pub omitted: Vec<Omitted>,
}
impl ExportPlan {
    pub fn is_empty(&self) -> bool {
        self.highlights.is_empty() && self.notes.is_empty()
    }
}

/// The PDF annotation name (`/NM`) used for a DB annotation, so the re-read file can be
/// matched to the plan without relying on order.
pub fn pdf_name(id: AnnotationId) -> String {
    id.0.to_string()
}

fn axis_aligned(q: &Quad) -> bool {
    q.valid() && *q == q.bounds().quad()
}

/// Map the attachment's annotations (#19) to standard PDF annotations. Unresolved anchors
/// and shapes the export cannot represent faithfully are listed in `omitted`, never dropped.
pub fn plan(annotations: &[Annotation]) -> ExportPlan {
    let mut plan = ExportPlan::default();
    for a in annotations {
        let omit = |reason: String| Omitted {
            id: a.id,
            kind: a.kind,
            reason,
        };
        let (page_index, quads) = match &a.anchor {
            Anchor::Resolved {
                page_index, quads, ..
            } => (*page_index, quads),
            Anchor::Unresolved { reason } => {
                plan.omitted
                    .push(omit(format!("Not placed on this PDF: {reason}")));
                continue;
            }
        };
        let contents = a.comment.clone().unwrap_or_default();
        match a.kind {
            AnnotationKind::Highlight => {
                if quads.is_empty() {
                    plan.omitted.push(omit("Highlight has no position".into()));
                } else if !quads.iter().all(axis_aligned) {
                    plan.omitted.push(omit(
                        "Highlight shape is not an axis-aligned line box; not exported".into(),
                    ));
                } else {
                    plan.highlights.push(PlannedHighlight {
                        id: a.id,
                        page_index,
                        quads: quads.clone(),
                        color: rgb(a.color.as_deref()),
                        opacity: HIGHLIGHT_OPACITY,
                        contents,
                    });
                }
            }
            AnnotationKind::Note => {
                if contents.trim().is_empty() {
                    plan.omitted.push(omit("Note has no text".into()));
                    continue;
                }
                let at = quads
                    .iter()
                    .map(|q| q.bounds())
                    .reduce(union)
                    .filter(|b| b.valid())
                    .map(|b| Rect {
                        left: b.left,
                        top: b.top,
                        right: b.left + NOTE_ICON,
                        bottom: b.top - NOTE_ICON,
                    });
                plan.notes.push(PlannedNote {
                    id: a.id,
                    page_index,
                    at,
                    color: rgb(a.color.as_deref()),
                    contents,
                });
            }
        }
    }
    plan
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.min(b.left),
        bottom: a.bottom.min(b.bottom),
        right: a.right.max(b.right),
        top: a.top.max(b.top),
    }
}

/// Icon rectangle of the `slot`-th auto-placed note on a page: a column of icons from the
/// *visible* top-left corner (after the page's /Rotate), wrapping into further columns.
/// `None` when the visible page is too small for even one icon.
///
/// One icon size is kept free towards the far edges: viewers that draw Text icons upright
/// (poppler; NoRotate semantics) pin them at the rect's unrotated upper-left corner, up to
/// one icon away from where PDFium/PDFKit draw them, and both must stay on the page.
pub fn note_slot(crop: Rect, rotation: Rotation, slot: usize) -> Option<Rect> {
    let viewport = Viewport::new(crop, rotation, 1.0, Origin::TopLeft)?;
    let (width, height) = viewport.size();
    let step = NOTE_ICON + NOTE_MARGIN;
    let rows = ((height - NOTE_MARGIN - NOTE_ICON) / step).floor();
    let columns = ((width - NOTE_MARGIN - NOTE_ICON) / step).floor();
    if rows < 1.0 || columns < 1.0 {
        return None;
    }
    let (rows, columns) = (rows as usize, columns as usize);
    let (row, column) = (slot % rows, (slot / rows) % columns);
    let x = NOTE_MARGIN + column as f32 * step;
    let y = NOTE_MARGIN + row as f32 * step;
    let a = viewport.to_pdf(Point { x, y });
    let b = viewport.to_pdf(Point {
        x: x + NOTE_ICON,
        y: y + NOTE_ICON,
    });
    Some(Rect {
        left: a.x.min(b.x),
        bottom: a.y.min(b.y),
        right: a.x.max(b.x),
        top: a.y.max(b.y),
    })
}

/// One annotation as re-read from a PDF by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadAnnotation {
    pub page_index: u32,
    /// Engine subtype name, e.g. `Highlight`, `Text`.
    pub subtype: String,
    /// `/NM`.
    pub name: Option<String>,
    pub contents: Option<String>,
    pub rect: Rect,
    pub quads: Vec<Quad>,
    /// Parsed normal appearance (`/AP /N`), `None` when absent.
    pub appearance: Option<ReadAppearance>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAppearance {
    /// Page objects in the appearance form.
    pub objects: usize,
    /// RGBA fill of the first filled path, including the graphics-state fill alpha.
    pub fill: Option<[u8; 4]>,
}
/// Where the engine put each planned note (same order as `ExportPlan::notes`).
pub type NotePlacements = Vec<Rect>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    pub highlights: usize,
    pub notes: usize,
    /// Annotations the source already had, re-read unchanged.
    pub preserved: usize,
    /// Largest QuadPoints/Rect difference seen (points).
    pub max_position_error: f32,
}

fn near(a: f32, b: f32, error: &mut f32) -> bool {
    let d = (a - b).abs();
    *error = error.max(d);
    d <= POSITION_TOLERANCE
}
fn near_rect(a: Rect, b: Rect, error: &mut f32) -> bool {
    near(a.left, b.left, error)
        & near(a.bottom, b.bottom, error)
        & near(a.right, b.right, error)
        & near(a.top, b.top, error)
}
fn near_quads(a: &[Quad], b: &[Quad], error: &mut f32) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.0.iter()
                .zip(b.0)
                .all(|(p, q)| near(p.x, q.x, error) & near(p.y, q.y, error))
        })
}
fn near_color(fill: Option<[u8; 4]>, rgb: [u8; 3], alpha: u8) -> bool {
    fill.is_some_and(|f| {
        f[..3]
            .iter()
            .zip(rgb)
            .all(|(a, b)| a.abs_diff(b) <= COLOR_TOLERANCE)
            && f[3].abs_diff(alpha) <= ALPHA_TOLERANCE
    })
}
fn same_existing(a: &ReadAnnotation, b: &ReadAnnotation) -> bool {
    let mut error = 0.0;
    a.subtype == b.subtype
        && a.name == b.name
        && a.contents == b.contents
        && near_rect(a.rect, b.rect, &mut error)
        && near_quads(&a.quads, &b.quads, &mut error)
        && a.appearance == b.appearance
}
fn by_page(list: &[ReadAnnotation]) -> BTreeMap<u32, Vec<&ReadAnnotation>> {
    let mut pages: BTreeMap<u32, Vec<&ReadAnnotation>> = BTreeMap::new();
    for a in list {
        pages.entry(a.page_index).or_default().push(a);
    }
    pages
}

/// Compare the re-read export (`after`) with the source (`before`) and the plan: every source
/// annotation unchanged and first on its page, every planned annotation present exactly once
/// with its subtype, contents, position, colour/opacity and a persistent normal appearance,
/// and nothing else added. Returns every mismatch found.
pub fn verify(
    plan: &ExportPlan,
    placements: &[Rect],
    before: &[ReadAnnotation],
    after: &[ReadAnnotation],
) -> Result<Verification, Vec<String>> {
    let mut problems = Vec::new();
    let mut error = 0.0f32;
    if placements.len() != plan.notes.len() {
        problems.push("Note placements do not match the planned notes".to_owned());
    }
    let before_pages = by_page(before);
    let after_pages = by_page(after);
    let mut added: Vec<&ReadAnnotation> = Vec::new();
    let mut preserved = 0;
    for (page, old) in &before_pages {
        let new = after_pages.get(page).map_or(&[][..], Vec::as_slice);
        if new.len() < old.len() || !old.iter().zip(new).all(|(a, b)| same_existing(a, b)) {
            problems.push(format!(
                "Existing annotations on page {} were not preserved unchanged",
                page + 1
            ));
        } else {
            preserved += old.len();
        }
    }
    for (page, new) in &after_pages {
        let skip = before_pages.get(page).map_or(0, Vec::len);
        added.extend(new.iter().skip(skip));
    }
    let expected = plan.highlights.len() + plan.notes.len();
    if added.len() != expected {
        problems.push(format!(
            "Expected {expected} new annotation(s) in the output, found {}",
            added.len()
        ));
    }
    let mut take = |name: &str| -> Option<&ReadAnnotation> {
        let found: Vec<usize> = added
            .iter()
            .enumerate()
            .filter(|(_, a)| a.name.as_deref() == Some(name))
            .map(|(i, _)| i)
            .collect();
        (found.len() == 1).then(|| added.swap_remove(found[0]))
    };
    for h in &plan.highlights {
        let label = format!("Highlight on page {}", h.page_index + 1);
        let Some(a) = take(&pdf_name(h.id)) else {
            problems.push(format!("{label} is missing from the output"));
            continue;
        };
        let alpha = (h.opacity * 255.0).round() as u8;
        let bounds = h.quads.iter().map(|q| q.bounds()).reduce(union);
        if a.page_index != h.page_index || a.subtype != "Highlight" {
            problems.push(format!(
                "{label} was written as {} on page {}",
                a.subtype,
                a.page_index + 1
            ));
        }
        if a.contents.as_deref().unwrap_or("") != h.contents {
            problems.push(format!("{label}: comment differs after re-reading"));
        }
        if !near_quads(&a.quads, &h.quads, &mut error) {
            problems.push(format!("{label}: QuadPoints differ after re-reading"));
        }
        if bounds.is_none_or(|b| {
            let mut ignored = 0.0;
            !near_rect(a.rect, b, &mut ignored)
        }) {
            problems.push(format!("{label}: Rect does not enclose its quads"));
        }
        match &a.appearance {
            Some(ap) if ap.objects > 0 && near_color(ap.fill, h.color, alpha) => {}
            Some(_) => problems.push(format!("{label}: appearance colour/opacity differs")),
            None => problems.push(format!("{label} has no persistent appearance")),
        }
    }
    for (i, n) in plan.notes.iter().enumerate() {
        let label = format!("Note on page {}", n.page_index + 1);
        let Some(a) = take(&pdf_name(n.id)) else {
            problems.push(format!("{label} is missing from the output"));
            continue;
        };
        if a.page_index != n.page_index || a.subtype != "Text" {
            problems.push(format!(
                "{label} was written as {} on page {}",
                a.subtype,
                a.page_index + 1
            ));
        }
        if a.contents.as_deref() != Some(n.contents.as_str()) {
            problems.push(format!("{label}: text differs after re-reading"));
        }
        if placements
            .get(i)
            .is_none_or(|r| !near_rect(a.rect, *r, &mut error))
        {
            problems.push(format!("{label}: position differs after re-reading"));
        }
        match &a.appearance {
            Some(ap) if ap.objects > 0 && near_color(ap.fill, n.color, 255) => {}
            Some(_) => problems.push(format!("{label}: appearance colour differs")),
            None => problems.push(format!("{label} has no persistent appearance")),
        }
    }
    for a in added {
        problems.push(format!(
            "Unexpected {} annotation on page {}",
            a.subtype,
            a.page_index + 1
        ));
    }
    if problems.is_empty() {
        Ok(Verification {
            highlights: plan.highlights.len(),
            notes: plan.notes.len(),
            preserved,
            max_position_error: error,
        })
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests;
