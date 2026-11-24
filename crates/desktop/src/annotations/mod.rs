//! Framework-free helpers that turn a viewer selection into an annotation (#19).
use cm_protocol::{
    annotations::{Anchor, AnnotationKind, MAX_CONTEXT_CHARS, NewAnnotation, Source},
    attachments::{PageChar, Quad, Rect},
    domain::AttachmentId,
};

/// One quad per text line: consecutive characters whose vertical extents overlap by at least
/// half of the smaller glyph are merged, so a long highlight stores a few line rectangles.
pub fn line_quads(chars: &[PageChar]) -> Vec<Quad> {
    let mut lines: Vec<Rect> = Vec::new();
    for c in chars {
        let b = c.quad.bounds();
        if !b.valid() {
            continue;
        }
        match lines.last_mut() {
            Some(line) if same_line(*line, b) => {
                line.left = line.left.min(b.left);
                line.right = line.right.max(b.right);
                line.bottom = line.bottom.min(b.bottom);
                line.top = line.top.max(b.top);
            }
            _ => lines.push(b),
        }
    }
    lines.into_iter().map(Rect::quad).collect()
}

fn same_line(a: Rect, b: Rect) -> bool {
    let overlap = a.top.min(b.top) - a.bottom.max(b.bottom);
    overlap >= 0.5 * a.height().min(b.height())
}

/// Up to `MAX_CONTEXT_CHARS` characters before/after an inclusive range, for re-anchoring.
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
