//! Framework-free helpers that turn a viewer selection into an annotation (#19).
use cm_protocol::{
    annotations::{
        Anchor, Annotation, AnnotationId, AnnotationKind, Edit, History, MAX_CONTEXT_CHARS,
        NewAnnotation, Source, merge_line_quads,
    },
    attachments::{PageChar, Quad},
    domain::{AttachmentId, PaperId},
    geometry::{Point, Viewport},
    library::ServiceError,
};

/// One quad per text line (`merge_line_quads`, shared with re-anchoring), so a long highlight
/// stores a few line rectangles.
pub fn line_quads(chars: &[PageChar]) -> Vec<Quad> {
    merge_line_quads(chars.iter().map(|c| c.quad))
}

/// Up to `MAX_CONTEXT_CHARS` characters before/after an inclusive range, for re-anchoring.
/// Like the quote, only characters with a Unicode value are kept (the rule re-anchoring uses).
pub fn context(chars: &[PageChar], start: usize, end: usize) -> (Option<String>, Option<String>) {
    let text = |slice: &[PageChar]| -> Option<String> {
        let s: String = slice.iter().filter_map(|c| c.unicode).collect();
        (!s.is_empty()).then_some(s)
    };
    let before = &chars[start.saturating_sub(MAX_CONTEXT_CHARS).min(start)..start.min(chars.len())];
    let after_start = (end + 1).min(chars.len());
    let after = &chars[after_start..(after_start + MAX_CONTEXT_CHARS).min(chars.len())];
    (text(before), text(after))
}

/// A highlight on one page from an inclusive character range of that page.
pub fn highlight(
    attachment: AttachmentId,
    page_index: u32,
    page_label: Option<String>,
    chars: &[PageChar],
    start: usize,
    end: usize,
    color: &str,
) -> Option<NewAnnotation> {
    if chars.is_empty() || start > end || end >= chars.len() {
        return None;
    }
    let selected = &chars[start..=end];
    let quote: String = selected.iter().filter_map(|c| c.unicode).collect();
    let quads = line_quads(selected);
    if quote.trim().is_empty() || quads.is_empty() {
        return None;
    }
    let (context_before, context_after) = context(chars, start, end);
    Some(NewAnnotation {
        attachment_id: attachment,
        kind: AnnotationKind::Highlight,
        source: Source::Human,
        anchor: Anchor::Resolved {
            page_index,
            page_label,
            quads,
        },
        quote,
        context_before,
        context_after,
        color: Some(color.to_owned()),
        comment: None,
    })
}

/// Note marker size and spacing in display pixels.
pub const MARKER_W: f32 = 24.0;
pub const MARKER_H: f32 = 20.0;
pub const MARKER_GAP: f32 = 4.0;

pub fn quads_of(a: &Annotation) -> &[Quad] {
    match &a.anchor {
        Anchor::Resolved { quads, .. } => quads,
        Anchor::Unresolved { .. } => &[],
    }
}
/// Drawn as a note marker rather than highlighted text.
pub fn is_marker(a: &Annotation) -> bool {
    a.kind == AnnotationKind::Note || quads_of(a).is_empty()
}
/// Top-left (display space) of the `i`-th marker on a page: at its first quad when it has one,
/// otherwise stacked down the page's left margin so page notes never overlap.
pub fn marker_origin(vp: Viewport, a: &Annotation, i: usize) -> (f32, f32) {
    match quads_of(a).first() {
        Some(q) => {
            let b = vp.quad_to_display(*q).bounds();
            (b.left, b.bottom.min(b.top))
        }
        None => (6.0, 6.0 + i as f32 * (MARKER_H + MARKER_GAP)),
    }
}
/// The topmost annotation of one page under a display-space point. The viewer draws
/// highlights in list order and note markers above them, so markers are tested first and
/// both in reverse drawing order.
pub fn annotation_at(
    vp: Viewport,
    on_page: &[&Annotation],
    display: Point,
) -> Option<AnnotationId> {
    let markers: Vec<&Annotation> = on_page.iter().copied().filter(|a| is_marker(a)).collect();
    for (i, a) in markers.iter().enumerate().rev() {
        let (x, y) = marker_origin(vp, a, i);
        if display.x >= x
            && display.x <= x + MARKER_W
            && display.y >= y
            && display.y <= y + MARKER_H
        {
            return Some(a.id);
        }
    }
    let p = vp.hit_test(display)?;
    on_page
        .iter()
        .rev()
        .filter(|a| !is_marker(a))
        .find(|a| {
            quads_of(a).iter().any(|q| {
                let b = q.bounds();
                p.x >= b.left && p.x <= b.right && p.y >= b.bottom && p.y <= b.top
            })
        })
        .map(|a| a.id)
}

/// Direction of a history step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Undo,
    Redo,
}
/// What happened to a history step whose reply arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Applied,
    /// The annotation changed meanwhile: the step is dropped.
    Skipped,
    /// Failed for another reason: the step is kept so it can be retried.
    Retained,
    /// The reply belongs to a paper that is no longer open.
    Ignored,
}
/// The open paper's undo/redo history. Every request is tagged with the paper it was sent
/// for, so a late reply after a paper switch never touches the new paper's history, and only
/// one undo/redo is in flight at a time.
#[derive(Debug, Default)]
pub struct PaperHistory {
    paper: Option<PaperId>,
    history: History,
    busy: bool,
}
impl PaperHistory {
    pub fn paper(&self) -> Option<PaperId> {
        self.paper
    }
    pub fn is_current(&self, paper: PaperId) -> bool {
        self.paper == Some(paper)
    }
    /// Open another paper (or none). Returns true when it changed; history then starts empty.
    pub fn open(&mut self, paper: Option<PaperId>) -> bool {
        if self.paper == paper {
            return false;
        }
        self.paper = paper;
        self.history.clear();
        self.busy = false;
        true
    }
    pub fn busy(&self) -> bool {
        self.busy
    }
    pub fn can_undo(&self) -> bool {
        !self.busy && self.history.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        !self.busy && self.history.can_redo()
    }
    /// A new edit made for `paper`; false (not recorded) when another paper is open.
    pub fn record(&mut self, paper: PaperId, edit: Edit) -> bool {
        if self.is_current(paper) {
            self.history.record(edit);
        }
        self.is_current(paper)
    }
    /// The next step to send, marking the history busy until `finish` or `unsent`.
    /// None while another step is in flight, with no paper open, or with nothing to do.
    pub fn begin(&mut self, step: Step) -> Option<(PaperId, Edit)> {
        let paper = self.paper.filter(|_| !self.busy)?;
        let edit = match step {
            Step::Undo => self.history.take_undo(),
            Step::Redo => self.history.take_redo(),
        }?;
        self.busy = true;
        Some((paper, edit))
    }
    /// The request could not be queued: the step goes back where it was.
    pub fn unsent(&mut self, step: Step, edit: Edit) {
        self.busy = false;
        self.put_back(step, edit);
    }
    fn put_back(&mut self, step: Step, edit: Edit) {
        match step {
            Step::Undo => self.history.restore_undo(edit),
            Step::Redo => self.history.restore_redo(edit),
        }
    }
    pub fn finish(
        &mut self,
        step: Step,
        paper: PaperId,
        edit: Edit,
        result: Result<(), ServiceError>,
    ) -> StepOutcome {
        if !self.is_current(paper) {
            return StepOutcome::Ignored;
        }
        self.busy = false;
        match result {
            Ok(()) => {
                match step {
                    Step::Undo => self.history.undone(edit),
                    Step::Redo => self.history.redone(edit),
                }
                StepOutcome::Applied
            }
            Err(ServiceError::Conflict) => StepOutcome::Skipped,
            Err(_) => {
                self.put_back(step, edit);
                StepOutcome::Retained
            }
        }
    }
}

/// Display colour (RGB) for a stored colour name or `#RRGGBB`.
pub fn rgb(color: Option<&str>) -> u32 {
    match color.unwrap_or("yellow") {
        "yellow" => 0xffd84d,
        "green" => 0x7bd88f,
        "blue" => 0x6fa8ff,
        "pink" => 0xff8fc0,
        "purple" => 0xb48cff,
        "orange" => 0xffa94d,
        hex if hex.len() == 7 && hex.starts_with('#') => {
            u32::from_str_radix(&hex[1..], 16).unwrap_or(0xffd84d)
        }
        _ => 0xffd84d,
    }
}
