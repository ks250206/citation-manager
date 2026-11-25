//! #17 PDF viewer in the Details › PDF slot: continuous pages, zoom/fit/rotate, page jump,
//! bounded render cache with cancellation, and character-based text selection.
use super::annotations_ui::page_of;
use super::*;
use crate::annotations::{MARKER_H, MARKER_W, is_marker, marker_origin, placement, quads_of};
use crate::pdf_viewer::{
    self, BusyBackoff, PageImage, RenderCache, RenderKey, TextPos, ZoomMode, capped_zoom, hit_char,
    layout_scale, page_offsets, page_range, render_zoom, rescale_offset, rotate, rotate_rgba,
    rotated_points, selected_text, step_zoom, visible_pages, wanted_pages,
};
use cm_protocol::annotations::{
    Annotation, AnnotationId, AnnotationKind, AnnotationPatch, NewAnnotation,
};
use std::collections::BTreeMap;

/// Concurrent page renders; the PDF worker itself is serial, so more only queues.
const MAX_IN_FLIGHT: usize = 3;
/// Pointer travel (px) above which a press is a drag, not a click.
const CLICK_SLOP: f32 = 3.0;

pub(super) struct Viewer {
    attachment: AttachmentId,
    page_count: Option<u32>,
    /// Crop box and the page's own rotation, from the first render of each page.
    geometry: HashMap<u32, (Rect, Rotation)>,
    chars: HashMap<u32, Vec<PageChar>>,
    labels: HashMap<u32, Option<String>>,
    has_text: HashMap<u32, bool>,
    failures: HashMap<u32, RenderFailure>,
    document_failure: Option<RenderFailure>,
    mode: ZoomMode,
    turns: u8,
    cache: RenderCache<PageImage<Arc<RenderImage>>>,
    /// Request serial + cancellation per key; a reply applies only if its serial matches,
    /// so a late reply for a cancelled request cannot consume a newer request's slot.
    in_flight: HashMap<RenderKey, (u64, Cancellation)>,
    /// `Busy` replies wait out their retry delay instead of being resubmitted every frame.
    busy: BusyBackoff,
    serial: u64,
    scroll: ScrollHandle,
    anchor: Option<TextPos>,
    head: Option<TextPos>,
    dragging: bool,
    /// Where the left button went down, and whether the pointer has since moved beyond the
    /// click slop: a press without a drag is a click (selects an annotation on mouse-up).
    press: Option<Point<Pixels>>,
    dragged: bool,
    /// Last computed layout inputs, used by event handlers between renders.
    scale: f32,
    device_scale: f32,
    /// Pane size the last layout used; a change (first layout, resize, reading mode)
    /// schedules one more frame so zoom and visible pages follow the real size.
    measured: (f32, f32),
}

impl Viewer {
    fn new(attachment: AttachmentId) -> Self {
        Self {
            attachment,
            page_count: None,
            geometry: HashMap::new(),
            chars: HashMap::new(),
            labels: HashMap::new(),
            has_text: HashMap::new(),
            failures: HashMap::new(),
            document_failure: None,
            mode: ZoomMode::FitWidth,
            turns: 0,
            cache: RenderCache::default(),
            in_flight: HashMap::new(),
            busy: BusyBackoff::default(),
            serial: 0,
            scroll: ScrollHandle::new(),
            anchor: None,
            head: None,
            dragging: false,
            press: None,
            dragged: false,
            scale: 1.0,
            device_scale: 1.0,
            measured: (0.0, 0.0),
        }
    }
    fn cancel_all(&mut self) {
        for (_, (_, cancel)) in self.in_flight.drain() {
            cancel.cancel();
        }
    }
    /// Zoom/rotation/fit changed: earlier per-page failures (e.g. too large) may now succeed.
    fn reset_attempts(&mut self) {
        self.cancel_all();
        self.failures.clear();
        self.busy.clear();
    }
    /// Size of a page in points after its own and the user's rotation.
    fn page_points(&self, page: u32) -> Option<(f32, f32)> {
        self.geometry
            .get(&page)
            .map(|(crop, r)| rotated_points(*crop, rotate(*r, self.turns)))
    }
    fn sizes(&self) -> HashMap<u32, (f32, f32)> {
        self.geometry
            .keys()
            .filter_map(|p| self.page_points(*p).map(|s| (*p, s)))
            .collect()
    }
    fn pane(&self) -> (f32, f32) {
        let b = self.scroll.bounds();
        (f32::from(b.size.width), f32::from(b.size.height))
    }
    fn layout(&self) -> Vec<(f32, f32)> {
        page_offsets(self.page_count.unwrap_or(1), &self.sizes(), self.scale)
    }
    /// Page under a window position and the point in that page's display space.
    fn locate(&self, position: Point<Pixels>) -> Option<(u32, pdf_viewer::Point)> {
        let bounds = self.scroll.bounds();
        let offset = self.scroll.offset();
        let (pane_w, _) = self.pane();
        let y = f32::from(position.y - bounds.origin.y - offset.y);
        let layout = self.layout();
        let page = layout
            .iter()
            .position(|(top, h)| y >= *top && y <= top + h)? as u32;
        let (w, _) = self.page_points(page)?;
        let page_w = w * self.scale;
        let left = ((pane_w - page_w) / 2.0).max(0.0);
        let x = f32::from(position.x - bounds.origin.x - offset.x) - left;
        Some((
            page,
            pdf_viewer::Point {
                x,
                y: y - layout[page as usize].0,
            },
        ))
    }
    fn caret_at(&self, position: Point<Pixels>) -> Option<TextPos> {
        let (page, p) = self.locate(position)?;
        let (crop, r) = *self.geometry.get(&page)?;
        let vp = pdf_viewer::viewport(crop, rotate(r, self.turns), self.scale)?;
        let pdf = vp.hit_test(p)?;
        let chars = self.chars.get(&page)?;
        hit_char(chars, pdf).map(|index| TextPos { page, index })
    }
    /// One highlight per page touched by the current selection.
    fn selection_annotations(&self, color: &str) -> Vec<NewAnnotation> {
        let (Some(a), Some(b)) = (self.anchor, self.head) else {
            return vec![];
        };
        let (start, end) = pdf_viewer::ordered(a, b);
        (start.page..=end.page)
            .filter_map(|page| {
                let chars = self.chars.get(&page)?;
                let range = page_range(page, chars.len(), a, b)?;
                crate::annotations::highlight(
                    self.attachment,
                    page,
                    self.labels.get(&page).cloned().flatten(),
                    chars,
                    *range.start(),
                    *range.end(),
                    color,
                )
            })
            .collect()
    }
    /// Topmost annotation under a window position: note markers (drawn last) first, then
    /// highlights in reverse drawing order.
    fn annotation_at<'a>(
        &self,
        annotations: impl Iterator<Item = &'a Annotation>,
        position: Point<Pixels>,
    ) -> Option<AnnotationId> {
        let (page, display) = self.locate(position)?;
        let (crop, r) = *self.geometry.get(&page)?;
        let vp = pdf_viewer::viewport(crop, rotate(r, self.turns), self.scale)?;
        let on_page: Vec<&Annotation> = annotations.filter(|a| page_of(a) == Some(page)).collect();
        crate::annotations::annotation_at(vp, &on_page, display)
    }
    fn selection_text(&self) -> Option<String> {
        let (a, b) = (self.anchor?, self.head?);
        let pages: BTreeMap<u32, &[PageChar]> =
            self.chars.iter().map(|(p, c)| (*p, c.as_slice())).collect();
        let text = selected_text(&pages, a, b);
        (!text.is_empty()).then_some(text)
    }
    pub(super) fn attachment_id(&self) -> AttachmentId {
        self.attachment
    }
    pub(super) fn scroll_by(&mut self, dy: f32) {
        let offset = self.scroll.offset();
        self.scroll
            .set_offset(point(offset.x, px((f32::from(offset.y) - dy).min(0.0))));
    }
    /// Jump to a page (clamped to the document).
    pub(super) fn jump(&mut self, page: u32) {
        self.scroll_to_page(page);
        let target = page.min(self.page_count.unwrap_or(1).saturating_sub(1));
        crate::frame_timing::begin(crate::frame_timing::Span::PageTurn, Some(target));
    }
    fn scroll_to_page(&mut self, page: u32) {
        let page = page.min(self.page_count.unwrap_or(1).saturating_sub(1));
        if let Some((top, _)) = self.layout().get(page as usize) {
            let x = self.scroll.offset().x;
            self.scroll
                .set_offset(point(x, px(-(top - pdf_viewer::PAGE_GAP).max(0.0))));
        }
    }
    /// Page label PDFium reported for a rendered page, if any.
    pub(super) fn page_label(&self, page: u32) -> Option<String> {
        self.labels.get(&page).cloned().flatten()
    }
    pub(super) fn current_page(&self) -> u32 {
        let (_, h) = self.pane();
        let top = -f32::from(self.scroll.offset().y);
        let visible = visible_pages(&self.layout(), top + h * 0.3, 1.0);
        visible.start
    }
}

/// GPUI images are BGRA; the worker returns straight RGBA. Converted in place: the buffer
/// received from the daemon becomes the image without another copy.
fn to_render_image(width: u32, height: u32, mut bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Pixel-less stand-in for a page image the GPU atlas already holds: same image id, so
/// drawing finds the existing texture and `drop_image` releases it (#85). It is never
/// uploaded itself, because its texture outlives it only while the page stays cached.
fn texture_only(image: &Arc<RenderImage>) -> Arc<RenderImage> {
    let mut stand_in = RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(1, 1))]);
    stand_in.id = image.id;
    Arc::new(stand_in)
}

/// Draws a cached page from its texture, uploading the pixels on the first draw, and
/// records that the GPU now holds them so the CPU copy can be released.
fn page_canvas(page: &PageImage<Arc<RenderImage>>, width: f32, height: f32) -> AnyElement {
    let image = page.image().clone();
    let uploaded = page.upload_flag();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
                return;
            }
            // Same call as `img()` with ObjectFit::Fill; on success the atlas has the tile
            // (and its own copy of the pixels, uploaded before the next frame).
            if window
                .paint_image(bounds, bounds, Corners::default(), image, 0, false)
                .is_ok()
            {
                uploaded.set(true);
            }
        },
    )
    .w(px(width))
    .h(px(height))
    .into_any_element()
}

impl LibraryView {
    /// Keep the viewer in step with the attachment chosen in the PDF host.
    fn sync_viewer(&mut self) {
        let wanted = self
            .host
            .selected
            .filter(|_| self.editor.is_some() && self.settings.details_tab == "PDF");
        match (&mut self.viewer, wanted) {
            (Some(v), Some(id)) if v.attachment == id => {}
            (viewer, wanted) => {
                if self.find.attachment.is_some() && self.find.attachment != wanted {
                    self.find.generation += 1;
                    self.find.query.clear();
                    self.find.pages.clear();
                    self.find.current = 0;
                    self.find.status.clear();
                    self.find.attachment = None;
                    self.find.loaded_only = false;
                }
                if self.pending_jump.is_some_and(|(a, _)| Some(a) != wanted) {
                    self.pending_jump = None;
                }
                if let Some(mut old) = viewer.take() {
                    old.cancel_all();
                    self.retired_images
                        .extend(old.cache.drain().into_iter().map(PageImage::into_image));
                }
                *viewer = wanted.map(Viewer::new);
                if viewer.is_some() {
                    crate::frame_timing::begin(crate::frame_timing::Span::PdfFirstPage, None);
                }
            }
        }
    }
    /// Runs from the message pump, outside any window update: frees the CPU pixels of pages
    /// the GPU already holds and releases the textures of pages that left the cache. (Inside
    /// a window update `drop_image(_, None)` cannot reach the window being updated, so the
    /// texture would stay in its atlas.)
    pub(super) fn release_page_memory(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = &mut self.viewer {
            v.cache.release_uploaded(texture_only);
        }
        for image in self.retired_images.drain(..) {
            cx.drop_image(image, None);
        }
    }
    /// Request the visible pages (± prefetch) that are not cached at the current zoom and
    /// cancel renders that scrolled out of range.
    fn schedule_renders(&mut self) {
        let Some(v) = &mut self.viewer else { return };
        if v.document_failure.is_some() {
            return;
        }
        let count = v.page_count.unwrap_or(1);
        let (pane_w, pane_h) = v.pane();
        // Before the first layout the pane has no size; assume a screenful so page 1 loads.
        let pane_h = if pane_h < 1.0 { 800.0 } else { pane_h };
        // Fit modes use the first page so the zoom does not jump while paging through
        // documents that mix page sizes/orientations.
        let (old_scale, old_layout) = (v.scale, v.layout());
        v.scale = layout_scale(v.mode, v.page_points(0), (pane_w, pane_h), v.scale);
        if (v.scale - old_scale).abs() > f32::EPSILON && v.page_count.is_some() {
            // The pane changed width (Expand, returning to the PDF tab at another size):
            // keep the same page position in view instead of a stale pixel offset.
            let top = rescale_offset(&old_layout, &v.layout(), -f32::from(v.scroll.offset().y));
            let x = v.scroll.offset().x;
            v.scroll.set_offset(point(x, px(-top)));
        }
        let base_zoom = render_zoom(v.scale, v.device_scale);
        let top = -f32::from(v.scroll.offset().y);
        let visible = visible_pages(&v.layout(), top, pane_h.max(1.0));
        let wanted: Vec<RenderKey> = wanted_pages(visible, count)
            .into_iter()
            .filter(|p| !v.failures.contains_key(p))
            .map(|p| {
                let size = v.page_points(p).unwrap_or((612.0, 792.0));
                RenderKey::new(v.attachment, p, capped_zoom(base_zoom, size))
            })
            .collect();
        // Refresh recency only for the working set, least important first, so visible pages
        // end up most recent and offscreen pages are the ones evicted.
        for key in wanted.iter().rev() {
            v.cache.get(key);
        }
        let now = std::time::Instant::now();
        v.busy.prune(now);
        v.in_flight.retain(|key, (_, cancel)| {
            let keep = wanted.contains(key);
            if !keep {
                cancel.cancel();
            }
            keep
        });
        for key in wanted {
            if v.in_flight.len() >= MAX_IN_FLIGHT {
                break;
            }
            if v.cache.contains(&key)
                || v.in_flight.contains_key(&key)
                || v.busy.is_deferred(&key, now)
            {
                continue;
            }
            let cancel = Cancellation::default();
            let sender = self.sender.clone();
            v.serial += 1;
            let serial = v.serial;
            let request = RenderRequest {
                attachment: key.attachment,
                page: key.page,
                zoom: key.zoom_milli as f32 / 1000.0,
                cancel: cancel.clone(),
            };
            let sent = self.client.pdf(
                PdfRequest::Render(request),
                Box::new(|_| {}),
                Box::new(move |result| {
                    let _ = sender.send(Message {
                        lane: Lane::Pdf,
                        ticket: 0,
                        event: Event::Render(key, serial, result),
                    });
                }),
            );
            if sent.is_ok() {
                v.in_flight.insert(key, (serial, cancel));
            }
        }
    }
    pub(super) fn render_reply(
        &mut self,
        key: RenderKey,
        serial: u64,
        result: Result<PdfReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        let Some(v) = &mut self.viewer else {
            return;
        };
        // Replies for another document, or renders cancelled since, are dropped.
        if v.attachment != key.attachment || v.in_flight.get(&key).map(|(s, _)| *s) != Some(serial)
        {
            return;
        }
        v.in_flight.remove(&key);
        match result {
            Ok(PdfReply::Page(Ok(page))) => {
                v.page_count = Some(page.page_count.max(1));
                v.geometry.insert(page.page, (page.crop, page.rotation));
                v.has_text.insert(page.page, page.has_text);
                v.labels.insert(page.page, page.label.clone());
                v.chars.insert(page.page, page.chars);
                v.failures.remove(&page.page);
                // The reader thread handed over the only reference: take the buffer as is.
                let rgba = Arc::try_unwrap(page.rgba).unwrap_or_else(|shared| (*shared).clone());
                let (w, h, rgba) = rotate_rgba(page.width, page.height, rgba, v.turns);
                let bytes = rgba.len();
                match to_render_image(w, h, rgba) {
                    Some(image) => {
                        let evicted = v.cache.insert(key, PageImage::new(image), bytes);
                        self.retired_images
                            .extend(evicted.into_iter().map(PageImage::into_image));
                    }
                    None => {
                        v.failures.insert(
                            key.page,
                            RenderFailure::Other("Could not decode page".into()),
                        );
                    }
                }
            }
            Ok(PdfReply::Page(Err(RenderFailure::Cancelled))) => {}
            Ok(PdfReply::Page(Err(RenderFailure::Busy))) => {
                // Transient: try again shortly instead of recording a failure. Until the
                // deadline, repaints (including the notify below) skip this key.
                v.busy.defer(key, std::time::Instant::now());
                cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(pdf_viewer::BUSY_RETRY).await;
                    let _ = view.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            Ok(PdfReply::Page(Err(failure))) => {
                // Only document-level problems replace the page column; size or page errors
                // stay on their page and are retried after zoom/rotation changes.
                let whole_document = matches!(
                    failure,
                    RenderFailure::Encrypted
                        | RenderFailure::Damaged
                        | RenderFailure::MissingOrCorrupt
                        | RenderFailure::RuntimeUnavailable(_)
                );
                if whole_document {
                    v.document_failure = Some(failure);
                } else {
                    v.failures.insert(key.page, failure);
                }
            }
            Ok(_) => {}
            Err(e) => {
                v.failures.insert(
                    key.page,
                    RenderFailure::Other(format!("Render failed: {e}")),
                );
            }
        }
        cx.notify();
    }
    fn viewer_zoom(&mut self, mode: ZoomMode, cx: &mut Context<Self>) {
        if let Some(v) = &mut self.viewer {
            let page = v.current_page();
            v.mode = mode;
            v.reset_attempts();
            v.scale = layout_scale(mode, v.page_points(0), v.pane(), v.scale);
            v.scroll_to_page(page);
        }
        cx.notify();
    }
    fn viewer_rotate(&mut self, cx: &mut Context<Self>) {
        if let Some(v) = &mut self.viewer {
            let page = v.current_page();
            v.turns = (v.turns + 1) % 4;
            v.reset_attempts();
            // Bitmaps are rotated client-side; drop them so pages re-render rotated.
            self.retired_images
                .extend(v.cache.drain().into_iter().map(PageImage::into_image));
            v.scroll_to_page(page);
        }
        cx.notify();
    }
    pub(super) fn viewer_jump(&mut self, page: u32, cx: &mut Context<Self>) {
        if let Some(v) = &mut self.viewer {
            v.jump(page);
        }
        cx.notify();
    }
    pub(super) fn viewer_copy(&mut self, cx: &mut Context<Self>) {
        let text = self.viewer.as_ref().and_then(Viewer::selection_text);
        match text {
            Some(text) => {
                let chars = text.chars().count();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.status(format!("Copied {chars} characters from the PDF"), false, cx);
            }
            None => self.status("Select text on a page first (drag across it)", false, cx),
        }
    }
    fn page_input_submitted(&mut self, cx: &mut Context<Self>) {
        let text = self.page_input.read(cx).content.trim().to_owned();
        match text.parse::<u32>() {
            Ok(n) if n >= 1 => {
                self.viewer_jump(n - 1, cx);
                self.page_input
                    .update(cx, |f, cx| f.set_text(String::new(), cx));
            }
            _ => self.status("Enter a page number", true, cx),
        }
    }
    pub(super) fn subscribe_viewer_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe_in(
            &self.page_input,
            window,
            |this, _, _: &text_input::Submitted, _, cx| this.page_input_submitted(cx),
        )
        .detach();
    }
    pub(super) fn viewer_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_viewer();
        if let Some(v) = &mut self.viewer {
            v.device_scale = window.scale_factor();
            let pane = v.pane();
            if pane != v.measured {
                v.measured = pane;
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
        }
        self.schedule_renders();
        let Some((has_selection, current_page, attachment)) = self.viewer.as_ref().map(|v| {
            (
                v.anchor.zip(v.head).is_some(),
                v.current_page(),
                v.attachment,
            )
        }) else {
            return div()
                .p_3()
                .child(hint("Select an attachment above to view it."))
                .into_any_element();
        };
        let annotation_bars = self.annotation_bars(has_selection, current_page, attachment, cx);
        let marks_src: Vec<cm_protocol::annotations::Annotation> =
            self.ann.for_attachment(attachment).cloned().collect();
        let selected_ann = self.ann.selected;
        let find_query = (self.find.attachment == Some(attachment) && !self.find.query.is_empty())
            .then(|| self.find.query.clone());
        let find_current_page = self
            .find
            .pages
            .get(self.find.current)
            .map(|(page, _)| *page);
        let find_bar = self.find.open.then(|| self.find_bar(cx));
        let Some(v) = &mut self.viewer else {
            return div()
                .p_3()
                .child(hint("Select an attachment above to view it."))
                .into_any_element();
        };
        let count = v.page_count;
        let scale = v.scale;
        let current = v.current_page();
        let zoom = render_zoom(scale, v.device_scale);
        let layout = v.layout();
        let (pane_w, _) = v.pane();
        let selection = v.anchor.zip(v.head);
        if let Some((attachment, page)) = self.pending_jump
            && attachment == v.attachment
            && v.page_count.is_some()
        {
            v.scroll_to_page(page);
            self.pending_jump = None;
            // The page counter and toolbar were computed from the old offset; repaint once.
            cx.on_next_frame(window, |_, _, cx| cx.notify());
        }
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(0xf8f9fb))
            .child(button("pdf-prev", "‹", current > 0).on_click(
                cx.listener(move |this, _, _, cx| this.viewer_jump(current.saturating_sub(1), cx)),
            ))
            .child(div().w(px(56.)).child(self.page_input.clone()))
            .child(hint(format!(
                "{} / {}",
                current + 1,
                count.map_or("…".into(), |c| c.to_string())
            )))
            .child(
                button("pdf-next", "›", count.is_some_and(|c| current + 1 < c))
                    .on_click(cx.listener(move |this, _, _, cx| this.viewer_jump(current + 1, cx))),
            )
            .child(
                button("pdf-zoom-out", "−", scale > pdf_viewer::MIN_SCALE).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.viewer_zoom(ZoomMode::Scale(step_zoom(scale, false)), cx)
                    },
                )),
            )
            .child(
                hint(format!("{:.0}%", scale * 100.0))
                    .w(px(40.))
                    .text_center(),
            )
            .child(
                button("pdf-zoom-in", "+", scale < pdf_viewer::MAX_SCALE).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.viewer_zoom(ZoomMode::Scale(step_zoom(scale, true)), cx)
                    },
                )),
            )
            .child(
                nav_item("pdf-fit-width", "Width", v.mode == ZoomMode::FitWidth).on_click(
                    cx.listener(|this, _, _, cx| this.viewer_zoom(ZoomMode::FitWidth, cx)),
                ),
            )
            .child(
                nav_item("pdf-fit-page", "Page", v.mode == ZoomMode::FitPage).on_click(
                    cx.listener(|this, _, _, cx| this.viewer_zoom(ZoomMode::FitPage, cx)),
                ),
            )
            .child(
                button("pdf-rotate", "⟳", true)
                    .on_click(cx.listener(|this, _, _, cx| this.viewer_rotate(cx))),
            )
            .child(
                button("pdf-copy", "Copy", selection.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.viewer_copy(cx))),
            )
            .child(super::pdf_export_ui::save_pdf_button(
                &self.pdf_export,
                self.host.selected,
                cx,
            ));

        if let Some(failure) = &v.document_failure {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(toolbar)
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_color(rgb(0x9b1c1c)).child(failure.to_string()))
                        .child(hint(
                            "The paper's bibliographic data and other attachments are unaffected.",
                        )),
                )
                .into_any_element();
        }
        // #86: pages with a bitmap this frame (filled only while recording frame timings).
        let timing = crate::frame_timing::enabled();
        let mut painted = Vec::new();
        let pages: Vec<Div> = (0..count.unwrap_or(1))
            .map(|page| {
                let (top, height) = layout
                    .get(page as usize)
                    .copied()
                    .unwrap_or((0.0, 792.0 * scale));
                let size = v
                    .page_points(page)
                    .unwrap_or((612.0, height / scale.max(0.01)));
                let width = size.0 * scale;
                let image = v
                    .cache
                    .peek(&RenderKey::new(v.attachment, page, capped_zoom(zoom, size)))
                    .map(|cached| page_canvas(cached, width, height));
                if timing && image.is_some() {
                    painted.push(page);
                }
                let failure = v.failures.get(&page).map(ToString::to_string);
                let image_only = v.has_text.get(&page) == Some(&false);
                let found: Vec<Div> = match (&find_query, v.geometry.get(&page), v.chars.get(&page))
                {
                    (Some(query), Some((crop, r)), Some(chars)) => {
                        match pdf_viewer::viewport(*crop, rotate(*r, v.turns), scale) {
                            Some(vp) => crate::search::find_in_page(chars, query)
                                .into_iter()
                                .flat_map(|(a, b)| {
                                    chars[a..=b].iter().map(|c| c.quad).collect::<Vec<_>>()
                                })
                                .map(|q| {
                                    let b = vp.quad_to_display(q).bounds();
                                    div()
                                        .absolute()
                                        .left(px(b.left))
                                        .top(px(b.bottom.min(b.top)))
                                        .w(px(b.width().abs()))
                                        .h(px(b.height().abs()))
                                        .bg(rgba(if find_current_page == Some(page) {
                                            0xff8c0088
                                        } else {
                                            0xffb34755
                                        }))
                                })
                                .collect(),
                            None => vec![],
                        }
                    }
                    _ => vec![],
                };
                let marks: Vec<Div> =
                    match (
                        v.geometry.get(&page),
                        pdf_viewer::viewport(
                            v.geometry.get(&page).map(|g| g.0).unwrap_or(Rect {
                                left: 0.0,
                                bottom: 0.0,
                                right: 1.0,
                                top: 1.0,
                            }),
                            v.geometry
                                .get(&page)
                                .map(|g| rotate(g.1, v.turns))
                                .unwrap_or(Rotation::R0),
                            scale,
                        ),
                    ) {
                        (Some(_), Some(vp)) => {
                            let on_page: Vec<&cm_protocol::annotations::Annotation> = marks_src
                                .iter()
                                .filter(|a| page_of(a) == Some(page))
                                .collect();
                            // Highlights first, then note markers on top (hit-testing mirrors it).
                            let highlights =
                                on_page.iter().filter(|a| !is_marker(a)).flat_map(|a| {
                                    let color = crate::annotations::rgb(a.color.as_deref());
                                    let chosen = selected_ann == Some(a.id);
                                    quads_of(a)
                                        .iter()
                                        .map(|q| {
                                            let b = vp.quad_to_display(*q).bounds();
                                            div()
                                                .absolute()
                                                .left(px(b.left))
                                                .top(px(b.bottom.min(b.top)))
                                                .w(px(b.width().abs()))
                                                .h(px(b.height().abs()))
                                                .bg(rgba((color << 8) | 0x66))
                                                .when(chosen, |el| {
                                                    el.border_b_2().border_color(rgb(ACCENT))
                                                })
                                        })
                                        .collect::<Vec<_>>()
                                });
                            let markers = on_page.iter().filter(|a| is_marker(a)).enumerate().map(
                                |(i, a)| {
                                    let chosen = selected_ann == Some(a.id);
                                    let (x, y) = marker_origin(vp, a, i);
                                    div()
                                        .absolute()
                                        .left(px(x))
                                        .top(px(y))
                                        .w(px(MARKER_W))
                                        .h(px(MARKER_H))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_md()
                                        .bg(rgb(0xfff4d6))
                                        .border_1()
                                        .border_color(rgb(if chosen { ACCENT } else { 0xf0d58a }))
                                        .text_size(px(11.))
                                        .cursor_pointer()
                                        .child("📝")
                                },
                            );
                            highlights.chain(markers).collect()
                        }
                        _ => vec![],
                    };
                let overlays: Vec<Div> =
                    match (selection, v.geometry.get(&page), v.chars.get(&page)) {
                        (Some((a, b)), Some((crop, r)), Some(chars)) => {
                            match (
                                page_range(page, chars.len(), a, b),
                                pdf_viewer::viewport(*crop, rotate(*r, v.turns), scale),
                            ) {
                                (Some(range), Some(vp)) => chars[range]
                                    .iter()
                                    .map(|c| {
                                        let b = vp.quad_to_display(c.quad).bounds();
                                        // Display quads are top-left origin: "bottom" is the smaller y.
                                        div()
                                            .absolute()
                                            .left(px(b.left))
                                            .top(px(b.bottom.min(b.top)))
                                            .w(px(b.width().abs()))
                                            .h(px(b.height().abs()))
                                            .bg(rgba(0x3b73d955))
                                    })
                                    .collect(),
                                _ => vec![],
                            }
                        }
                        _ => vec![],
                    };
                div()
                    .absolute()
                    .top(px(top))
                    .left(px(((pane_w - width) / 2.0).max(0.0)))
                    .w(px(width))
                    .h(px(height))
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .shadow_sm()
                    .child(match image {
                        Some(canvas) => canvas,
                        None => div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(hint(
                                failure.unwrap_or_else(|| format!("Rendering page {}…", page + 1)),
                            ))
                            .into_any_element(),
                    })
                    .children(marks)
                    .children(found)
                    .children(overlays)
                    .when(image_only, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_1()
                                .right_1()
                                .px_2()
                                .rounded_md()
                                .bg(rgba(0x1c253399))
                                .text_color(rgb(SURFACE))
                                .text_size(px(11.))
                                .child("Image-only page: no selectable text"),
                        )
                    })
            })
            .collect();
        if timing {
            let top = -f32::from(v.scroll.offset().y);
            crate::frame_timing::motion(crate::frame_timing::Span::PdfScroll, top);
            crate::frame_timing::pdf_painted(visible_pages(&layout, top, v.pane().1), &painted);
        }
        let total_height = layout
            .last()
            .map_or(0.0, |(t, h)| t + h + pdf_viewer::PAGE_GAP);
        let total_width = (0..count.unwrap_or(1))
            .filter_map(|p| v.page_points(p))
            .map(|(w, _)| w * scale)
            .fold(0.0f32, f32::max);
        let canvas_width = total_width.max(pane_w);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(toolbar)
            .children(annotation_bars)
            .children(find_bar)
            .child(
                div()
                    .id("pdf-pages")
                    .on_action(cx.listener(|this, _: &AnnotationUndo, _, cx| this.ann_undo(cx)))
                    .on_action(cx.listener(|this, _: &AnnotationRedo, _, cx| this.ann_redo(cx)))
                    .key_context("PdfViewer")
                    .on_key_down(cx.listener(Self::viewer_key))
                    .track_focus(&self.viewer_focus)
                    .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.viewer_copy(cx)))
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .track_scroll(&v.scroll)
                    .bg(rgb(0xe7e9ed))
                    .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.viewer_focus.focus(window, cx);
                            // Text selection always starts here; whether this was a click on
                            // a highlight/note is decided on mouse-up.
                            if let Some(v) = &mut this.viewer {
                                let caret = v.caret_at(event.position);
                                v.anchor = caret;
                                v.head = caret;
                                v.dragging = caret.is_some();
                                v.press = Some(event.position);
                                v.dragged = false;
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        let Some(v) = &mut this.viewer else { return };
                        if let Some(press) = v.press
                            && event.dragging()
                        {
                            let (dx, dy) = (
                                f32::from(event.position.x - press.x),
                                f32::from(event.position.y - press.y),
                            );
                            if dx.abs() > CLICK_SLOP || dy.abs() > CLICK_SLOP {
                                v.dragged = true;
                            }
                        }
                        if v.dragging
                            && event.dragging()
                            && let Some(caret) = v.caret_at(event.position)
                            && v.head != Some(caret)
                        {
                            v.head = Some(caret);
                            cx.notify();
                        }
                    }))
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, _| {
                            if let Some(v) = &mut this.viewer {
                                v.dragging = false;
                                v.press = None;
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, _, cx| {
                            let mut clicked = None;
                            if let Some(v) = &mut this.viewer {
                                v.dragging = false;
                                let press = v.press.take();
                                let click = !v.dragged && v.anchor == v.head;
                                if click {
                                    // A click without a drag clears the text selection...
                                    v.anchor = None;
                                    v.head = None;
                                    // ...and selects the topmost highlight/note under it.
                                    if !event.modifiers.shift {
                                        clicked = press.and_then(|p| {
                                            v.annotation_at(
                                                this.ann.for_attachment(v.attachment),
                                                p,
                                            )
                                        });
                                    }
                                }
                            }
                            if clicked.is_some() {
                                this.ann_select(clicked, cx);
                            }
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .w(px(canvas_width))
                            .h(px(total_height))
                            .children(pages),
                    ),
            )
            .into_any_element()
    }
    fn highlight_selection(&mut self, cx: &mut Context<Self>) {
        let color = self.ann.color;
        let Some(v) = &mut self.viewer else { return };
        let annotations = v.selection_annotations(color);
        if annotations.is_empty() {
            self.status("Select text first (drag across it)", true, cx);
            return;
        }
        v.anchor = None;
        v.head = None;
        for annotation in annotations {
            self.ann_create(annotation);
        }
        cx.notify();
    }
    /// Highlight/note/undo row plus the note editor and the selected-annotation editor.
    fn annotation_bars(
        &self,
        has_selection: bool,
        current: u32,
        attachment: AttachmentId,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let mut bars = vec![
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(rgb(BORDER))
                .child(self.color_swatches(
                    "pdf-hl-color",
                    Some(self.ann.color),
                    cx,
                    |this, c, cx| {
                        this.ann.color = c;
                        cx.notify();
                    },
                ))
                .child(
                    primary_button("pdf-highlight", "Highlight", has_selection)
                        .on_click(cx.listener(|this, _, _, cx| this.highlight_selection(cx))),
                )
                .child(button("pdf-note", "＋ Note", true).on_click(cx.listener(
                    |this, _, window, cx| {
                        this.ann.note_open = !this.ann.note_open;
                        if this.ann.note_open {
                            this.ann.note.focus_handle(cx).focus(window, cx);
                        }
                        cx.notify();
                    },
                )))
                .child(div().flex_1())
                .child(
                    button("pdf-undo", "↶", self.ann_can_undo())
                        .on_click(cx.listener(|this, _, _, cx| this.ann_undo(cx))),
                )
                .child(
                    button("pdf-redo", "↷", self.ann_can_redo())
                        .on_click(cx.listener(|this, _, _, cx| this.ann_redo(cx))),
                ),
        ];
        if self.ann.note_open {
            bars.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(hint(format!("Note on p. {}", current + 1)))
                    .child(div().flex_1().child(self.ann.note.clone()))
                    .child(primary_button("pdf-note-save", "Add", true).on_click(
                        cx.listener(move |this, _, _, cx| this.ann_save_note(current, cx)),
                    )),
            );
        }
        if let Some(a) = self
            .ann
            .selected()
            .filter(|a| a.attachment_id == attachment)
        {
            let id = a.id;
            bars.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(0xfffbea))
                    .child(hint(match page_of(a) {
                        Some(p) => format!("Selected · p. {}", p + 1),
                        None => "Selected · unresolved".into(),
                    }))
                    .when(placement(a).is_some_and(|p| p.manual), |el| {
                        el.child(
                            primary_button(
                                "pdf-ann-place",
                                format!("Place on p. {}", current + 1),
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.ann_place_on_page(id, current, cx),
                            )),
                        )
                    })
                    .when(a.kind == AnnotationKind::Highlight, |el| {
                        el.child(self.color_swatches(
                            "pdf-ann-color",
                            a.color.as_deref(),
                            cx,
                            |this, c, _| {
                                // Re-picking the current colour is not an edit.
                                if let Some(id) = this
                                    .ann
                                    .selected()
                                    .filter(|a| a.color.as_deref() != Some(c))
                                    .map(|a| a.id)
                                {
                                    this.ann_update(
                                        id,
                                        AnnotationPatch {
                                            color: Some(Some(c.to_owned())),
                                            comment: None,
                                        },
                                    );
                                }
                            },
                        ))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(120.))
                            .child(self.ann.comment.clone()),
                    )
                    .child(
                        button("pdf-ann-save", "Save", true)
                            .on_click(cx.listener(|this, _, _, cx| this.ann_save_comment(cx))),
                    )
                    .child(
                        button("pdf-ann-delete", "Delete", true)
                            .text_color(rgb(0xb42318))
                            .on_click(cx.listener(move |this, _, _, _| this.ann_delete(id))),
                    )
                    .child(
                        button("pdf-ann-close", "✕", true)
                            .on_click(cx.listener(|this, _, _, cx| this.ann_select(None, cx))),
                    ),
            );
        }
        bars
    }
    fn find_bar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(div().w(px(200.)).child(self.find.input.clone()))
            .child(
                button("find-prev", "↑", !self.find.pages.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(false, cx))),
            )
            .child(
                button("find-next", "↓", !self.find.pages.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(true, cx))),
            )
            .child(hint(self.find.status.clone()).flex_1())
            .child(
                button("find-close", "✕", true).on_click(cx.listener(|this, _, _, cx| {
                    this.find.open = false;
                    this.reset_find();
                    cx.notify();
                })),
            )
    }
    pub(super) fn show_pdf_tab(&mut self, cx: &mut Context<Self>) {
        if self.settings.details_tab != "PDF" {
            self.settings.details_tab = "PDF".into();
            self.persist(cx);
        }
    }
    pub(super) fn highlight_selection_command(&mut self, cx: &mut Context<Self>) {
        self.highlight_selection(cx);
    }
    /// Palette/keymap entry points for viewer controls (same code paths as the toolbar).
    pub(super) fn viewer_command(&mut self, id: &str, cx: &mut Context<Self>) {
        use crate::commands::pdf_ids as pdf;
        let scale = self.viewer.as_ref().map_or(1.0, |v| v.scale);
        match id {
            pdf::ZOOM_IN => self.viewer_zoom(ZoomMode::Scale(step_zoom(scale, true)), cx),
            pdf::ZOOM_OUT => self.viewer_zoom(ZoomMode::Scale(step_zoom(scale, false)), cx),
            pdf::FIT_WIDTH => self.viewer_zoom(ZoomMode::FitWidth, cx),
            pdf::ROTATE => self.viewer_rotate(cx),
            _ => {}
        }
    }
}
