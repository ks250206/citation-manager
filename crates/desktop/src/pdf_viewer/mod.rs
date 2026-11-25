//! Framework-free PDF viewer state (#17): render cache, layout/zoom math, user rotation and
//! character-based text selection. Geometry is the shared `cm_core::geometry` used by the
//! PDF worker, so display and PDF coordinates round-trip the same way on both sides.
pub use cm_protocol::geometry::{Origin, Point, Viewport};
use cm_protocol::{
    attachments::{PageChar, Quad, Rect, Rotation},
    domain::AttachmentId,
};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Zoom steps offered by −/+ (logical pixels per PDF point; 1.0 = 100%).
pub const ZOOM_STEPS: &[f32] = &[0.5, 0.67, 0.8, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];
pub const MIN_SCALE: f32 = 0.25;
pub const MAX_SCALE: f32 = 4.0;
/// Pages kept rendered around the visible range.
pub const PREFETCH: u32 = 1;
/// Decoded bitmaps kept in memory (RGBA bytes).
pub const CACHE_BUDGET_BYTES: usize = 192 * 1024 * 1024;
pub const CACHE_MAX_ENTRIES: usize = 48;
/// Gap between pages in logical pixels.
pub const PAGE_GAP: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ZoomMode {
    Scale(f32),
    FitWidth,
    FitPage,
}

/// Quarter turns clockwise.
pub fn rotate(rotation: Rotation, quarter_turns: u8) -> Rotation {
    const ORDER: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];
    let index = ORDER.iter().position(|r| *r == rotation).unwrap_or(0);
    ORDER[(index + quarter_turns as usize) % 4]
}

/// Page size in PDF points after rotation (width, height).
pub fn rotated_points(crop: Rect, rotation: Rotation) -> (f32, f32) {
    match rotation {
        Rotation::R0 | Rotation::R180 => (crop.width(), crop.height()),
        Rotation::R90 | Rotation::R270 => (crop.height(), crop.width()),
    }
}

/// Logical pixels per PDF point for a mode, given a page (rotated points) and the pane size.
pub fn scale_for(mode: ZoomMode, page: (f32, f32), pane: (f32, f32)) -> f32 {
    let margin = 24.0;
    let scale = match mode {
        ZoomMode::Scale(s) => s,
        ZoomMode::FitWidth => (pane.0 - margin) / page.0,
        ZoomMode::FitPage => ((pane.0 - margin) / page.0).min((pane.1 - margin) / page.1),
    };
    if scale.is_finite() {
        scale.clamp(MIN_SCALE, MAX_SCALE)
    } else {
        1.0
    }
}

/// Scale to lay pages out with. An explicit zoom applies at once, even before any page has
/// rendered (a first page too large at 100% must be recoverable by zooming out); fit modes need
/// the reference page size and a measured pane, and keep `current` until both are known.
pub fn layout_scale(
    mode: ZoomMode,
    reference: Option<(f32, f32)>,
    pane: (f32, f32),
    current: f32,
) -> f32 {
    match (mode, reference) {
        (ZoomMode::Scale(_), _) => scale_for(mode, (1.0, 1.0), pane),
        (_, Some(page)) if pane.0 > 1.0 => scale_for(mode, page, pane),
        _ => current,
    }
}

/// Next/previous zoom step from the current scale.
pub fn step_zoom(scale: f32, up: bool) -> f32 {
    if up {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|s| *s > scale + 0.001)
            .unwrap_or(MAX_SCALE)
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < scale - 0.001)
            .unwrap_or(MIN_SCALE)
    }
}

/// Render zoom (device pixels per point) bucketed so tiny layout changes reuse bitmaps.
pub fn render_zoom(scale: f32, device_scale: f32) -> f32 {
    let zoom = (scale * device_scale).clamp(0.1, 8.0);
    (zoom * 20.0).round() / 20.0
}

/// Largest render zoom (device px per point) that keeps a page within the worker's bitmap
/// limits, bucketed downwards so the result never exceeds them.
pub fn capped_zoom(zoom: f32, page_points: (f32, f32)) -> f32 {
    use cm_protocol::geometry::{MAX_RENDER_PIXELS, MAX_RENDER_SIDE};
    let (w, h) = page_points;
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return zoom;
    }
    let by_area = (MAX_RENDER_PIXELS as f32 / (w * h)).sqrt();
    let by_side = MAX_RENDER_SIDE / w.max(h);
    let cap = by_area.min(by_side) * 0.98; // ceil() + rounding headroom
    if zoom <= cap {
        zoom
    } else {
        ((cap * 20.0).floor() / 20.0).max(0.1)
    }
}

/// Vertical layout of a continuous page column: top offset of each page.
/// Unknown page sizes use the last known size (or US Letter) as an estimate.
pub fn page_offsets(count: u32, sizes: &HashMap<u32, (f32, f32)>, scale: f32) -> Vec<(f32, f32)> {
    let mut estimate = sizes.get(&0).copied().unwrap_or((612.0, 792.0));
    let mut top = PAGE_GAP;
    (0..count)
        .map(|page| {
            let size = sizes.get(&page).copied().unwrap_or(estimate);
            estimate = size;
            let entry = (top, size.1 * scale);
            top += size.1 * scale + PAGE_GAP;
            entry
        })
        .collect()
}

/// Pages intersecting `[top, top + height)` in a layout from `page_offsets`.
pub fn visible_pages(layout: &[(f32, f32)], top: f32, height: f32) -> std::ops::Range<u32> {
    let bottom = top + height.max(1.0);
    let first = layout
        .iter()
        .position(|(t, h)| t + h >= top)
        .unwrap_or(layout.len());
    let last = layout
        .iter()
        .rposition(|(t, _)| *t <= bottom)
        .map_or(first, |i| i + 1)
        .max(first);
    first as u32..last as u32
}

/// Visible pages first (in order), then neighbours within `PREFETCH`.
pub fn wanted_pages(visible: std::ops::Range<u32>, count: u32) -> Vec<u32> {
    let mut pages: Vec<u32> = visible.clone().collect();
    if count == 0 {
        return pages;
    }
    for d in 1..=PREFETCH {
        if let Some(before) = visible.start.checked_sub(d) {
            pages.push(before);
        }
        let after = visible.end.saturating_sub(1) + d;
        if after < count && !visible.contains(&after) {
            pages.push(after);
        }
    }
    pages.retain(|p| *p < count);
    pages.dedup();
    pages
}

/// Rotate a straight-RGBA bitmap clockwise by quarter turns.
pub fn rotate_rgba(width: u32, height: u32, rgba: &[u8], quarter_turns: u8) -> (u32, u32, Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let turns = quarter_turns % 4;
    if turns == 0 || rgba.len() != w * h * 4 {
        return (width, height, rgba.to_vec());
    }
    let (nw, nh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match turns {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let src = (y * w + x) * 4;
            let dst = (ny * nw + nx) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    (nw as u32, nh as u32, out)
}

/// Display mapping for one page: logical pixels, top-left origin, effective rotation.
pub fn viewport(crop: Rect, rotation: Rotation, scale: f32) -> Option<Viewport> {
    Viewport::new(crop, rotation, scale, Origin::TopLeft)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderKey {
    pub attachment: AttachmentId,
    pub page: u32,
    /// Render zoom × 1000.
    pub zoom_milli: u32,
}
impl RenderKey {
    pub fn new(attachment: AttachmentId, page: u32, zoom: f32) -> Self {
        Self {
            attachment,
            page,
            zoom_milli: (zoom * 1000.0).round() as u32,
        }
    }
}

/// LRU of decoded pages bounded by bytes and entry count.
pub struct RenderCache<V> {
    entries: HashMap<RenderKey, (V, usize)>,
    order: VecDeque<RenderKey>,
    bytes: usize,
    budget: usize,
    max_entries: usize,
}
impl<V> Default for RenderCache<V> {
    fn default() -> Self {
        Self::new(CACHE_BUDGET_BYTES, CACHE_MAX_ENTRIES)
    }
}
impl<V> RenderCache<V> {
    pub fn new(budget: usize, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget,
            max_entries: max_entries.max(1),
        }
    }
    pub fn get(&mut self, key: &RenderKey) -> Option<&V> {
        if self.entries.contains_key(key) {
            self.touch(key);
        }
        self.entries.get(key).map(|(v, _)| v)
    }
    /// Reads without refreshing recency. Building the page column touches every page, so it
    /// must not reorder the LRU; only the visible/prefetch working set is refreshed (`get`).
    pub fn peek(&self, key: &RenderKey) -> Option<&V> {
        self.entries.get(key).map(|(v, _)| v)
    }
    pub fn contains(&self, key: &RenderKey) -> bool {
        self.entries.contains_key(key)
    }
    fn touch(&mut self, key: &RenderKey) {
        if let Some(i) = self.order.iter().position(|k| k == key) {
            self.order.remove(i);
        }
        self.order.push_back(*key);
    }
    /// Inserts and evicts least-recently-used entries over budget, returning everything that
    /// left the cache (so GPU textures can be released). An entry larger than the whole budget
    /// is not stored and is returned as well.
    pub fn insert(&mut self, key: RenderKey, value: V, bytes: usize) -> Vec<V> {
        if bytes > self.budget {
            return vec![value];
        }
        let mut evicted = Vec::new();
        if let Some((old, size)) = self.entries.remove(&key) {
            self.bytes -= size;
            evicted.push(old);
        }
        self.entries.insert(key, (value, bytes));
        self.bytes += bytes;
        self.touch(&key);
        while self.bytes > self.budget || self.entries.len() > self.max_entries {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((old, size)) = self.entries.remove(&oldest) {
                self.bytes -= size;
                evicted.push(old);
            }
        }
        evicted
    }
    /// Drops everything for other documents (or all, with `None`) and returns it.
    pub fn clear_except(&mut self, keep: Option<AttachmentId>) -> Vec<V> {
        let drop: Vec<RenderKey> = self
            .order
            .iter()
            .copied()
            .filter(|k| Some(k.attachment) != keep)
            .collect();
        let mut evicted = Vec::new();
        for key in drop {
            if let Some((old, size)) = self.entries.remove(&key) {
                self.bytes -= size;
                evicted.push(old);
            }
        }
        self.order.retain(|k| self.entries.contains_key(k));
        evicted
    }
    /// Empties the cache, returning all values.
    pub fn drain(&mut self) -> Vec<V> {
        self.order.clear();
        self.bytes = 0;
        self.entries.drain().map(|(_, (v, _))| v).collect()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A caret position in document text order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub page: u32,
    pub index: usize,
}

/// Character under a PDF-space point; otherwise the nearest character on the same text line
/// (vertical overlap), so dragging past a line end still selects to its end.
pub fn hit_char(chars: &[PageChar], p: Point) -> Option<usize> {
    let contains = |q: &Quad| {
        let b = q.bounds();
        p.x >= b.left && p.x <= b.right && p.y >= b.bottom && p.y <= b.top
    };
    if let Some(i) = chars.iter().position(|c| contains(&c.quad)) {
        return Some(i);
    }
    chars
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            let b = c.quad.bounds();
            p.y >= b.bottom && p.y <= b.top
        })
        .min_by(|(_, a), (_, b)| {
            let d = |c: &PageChar| {
                let b = c.quad.bounds();
                if p.x < b.left {
                    b.left - p.x
                } else {
                    p.x - b.right
                }
            };
            d(a).total_cmp(&d(b))
        })
        .map(|(i, _)| i)
}

/// Inclusive selection between two caret positions in text order.
pub fn ordered(a: TextPos, b: TextPos) -> (TextPos, TextPos) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Selected index range on one page, if the selection touches it.
pub fn page_range(
    page: u32,
    len: usize,
    a: TextPos,
    b: TextPos,
) -> Option<std::ops::RangeInclusive<usize>> {
    let (start, end) = ordered(a, b);
    if len == 0 || page < start.page || page > end.page {
        return None;
    }
    let first = if page == start.page { start.index } else { 0 };
    let last = if page == end.page {
        end.index.min(len - 1)
    } else {
        len - 1
    };
    (first <= last).then_some(first..=last)
}

/// Unicode text of a selection across pages; pages are separated by a newline.
/// Characters PDFium could not map to Unicode are skipped, never guessed.
pub fn selected_text(pages: &BTreeMap<u32, &[PageChar]>, a: TextPos, b: TextPos) -> String {
    let mut out = String::new();
    for (page, chars) in pages {
        let Some(range) = page_range(*page, chars.len(), a, b) else {
            continue;
        };
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.extend(chars[range].iter().filter_map(|c| c.unicode));
    }
    out.replace("\r\n", "\n").replace('\r', "\n")
}
