//! #17 PDF viewer in the Details › PDF slot: continuous pages, zoom/fit/rotate, page jump,
//! bounded render cache with cancellation, and character-based text selection.
use super::*;
use crate::pdf_viewer::{
    self, RenderCache, RenderKey, TextPos, ZoomMode, capped_zoom, hit_char, page_offsets,
    page_range, render_zoom, rotate, rotate_rgba, rotated_points, scale_for, selected_text,
    step_zoom, visible_pages, wanted_pages,
};
use std::collections::BTreeMap;

/// Concurrent page renders; the PDF worker itself is serial, so more only queues.
const MAX_IN_FLIGHT: usize = 3;

pub(super) struct Viewer {
    attachment: AttachmentId,
    page_count: Option<u32>,
    /// Crop box and the page's own rotation, from the first render of each page.
    geometry: HashMap<u32, (Rect, Rotation)>,
    chars: HashMap<u32, Vec<PageChar>>,
    has_text: HashMap<u32, bool>,
    failures: HashMap<u32, RenderFailure>,
    document_failure: Option<RenderFailure>,
    mode: ZoomMode,
    turns: u8,
    cache: RenderCache<Arc<RenderImage>>,
    /// Request serial + cancellation per key; a reply applies only if its serial matches,
    /// so a late reply for a cancelled request cannot consume a newer request's slot.
    in_flight: HashMap<RenderKey, (u64, Cancellation)>,
    serial: u64,
    scroll: ScrollHandle,
    anchor: Option<TextPos>,
    head: Option<TextPos>,
    dragging: bool,
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
            has_text: HashMap::new(),
            failures: HashMap::new(),
            document_failure: None,
            mode: ZoomMode::FitWidth,
            turns: 0,
            cache: RenderCache::default(),
            in_flight: HashMap::new(),
            serial: 0,
            scroll: ScrollHandle::new(),
            anchor: None,
            head: None,
            dragging: false,
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
    fn selection_text(&self) -> Option<String> {
        let (a, b) = (self.anchor?, self.head?);
        let pages: BTreeMap<u32, &[PageChar]> =
            self.chars.iter().map(|(p, c)| (*p, c.as_slice())).collect();
        let text = selected_text(&pages, a, b);
        (!text.is_empty()).then_some(text)
    }
    fn scroll_to_page(&mut self, page: u32) {
        let page = page.min(self.page_count.unwrap_or(1).saturating_sub(1));
        if let Some((top, _)) = self.layout().get(page as usize) {
            let x = self.scroll.offset().x;
            self.scroll
                .set_offset(point(x, px(-(top - pdf_viewer::PAGE_GAP).max(0.0))));
        }
    }
    fn current_page(&self) -> u32 {
        let (_, h) = self.pane();
        let top = -f32::from(self.scroll.offset().y);
        let visible = visible_pages(&self.layout(), top + h * 0.3, 1.0);
        visible.start
    }
}

/// GPUI images are BGRA; the worker returns straight RGBA.
fn to_render_image(width: u32, height: u32, mut bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

impl LibraryView {
    /// Keep the viewer in step with the attachment chosen in the PDF host.
    fn sync_viewer(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .host
            .selected
            .filter(|_| self.editor.is_some() && self.settings.details_tab == "PDF");
        match (&mut self.viewer, wanted) {
            (Some(v), Some(id)) if v.attachment == id => {}
            (viewer, wanted) => {
                if let Some(mut old) = viewer.take() {
                    old.cancel_all();
                    for image in old.cache.drain() {
                        cx.drop_image(image, None);
                    }
                }
                *viewer = wanted.map(Viewer::new);
            }
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
        let reference = v.page_points(0);
        if let Some(page) = reference.filter(|_| pane_w > 1.0) {
            v.scale = scale_for(v.mode, page, (pane_w, pane_h));
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
            if v.cache.contains(&key) || v.in_flight.contains_key(&key) {
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
                v.chars.insert(page.page, page.chars);
                v.failures.remove(&page.page);
                let (w, h, rgba) = rotate_rgba(page.width, page.height, &page.rgba, v.turns);
                let bytes = rgba.len();
                match to_render_image(w, h, rgba) {
                    Some(image) => {
                        for old in v.cache.insert(key, image, bytes) {
                            cx.drop_image(old, None);
                        }
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
                // Transient: try again shortly instead of recording a failure.
                cx.spawn(async move |view, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
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
            if let Some(reference) = v.page_points(0) {
                v.scale = scale_for(mode, reference, v.pane());
            }
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
            for image in v.cache.drain() {
                cx.drop_image(image, None);
            }
            v.scroll_to_page(page);
        }
        cx.notify();
    }
    pub(super) fn viewer_jump(&mut self, page: u32, cx: &mut Context<Self>) {
        if let Some(v) = &mut self.viewer {
            v.scroll_to_page(page);
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
        self.sync_viewer(cx);
        if let Some(v) = &mut self.viewer {
            v.device_scale = window.scale_factor();
            let pane = v.pane();
            if pane != v.measured {
                v.measured = pane;
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
        }
        self.schedule_renders();
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
            );
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
                    .get(&RenderKey::new(v.attachment, page, capped_zoom(zoom, size)))
                    .cloned();
                let failure = v.failures.get(&page).map(ToString::to_string);
                let image_only = v.has_text.get(&page) == Some(&false);
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
                        Some(image) => img(ImageSource::Render(image))
                            .w(px(width))
                            .h(px(height))
                            .into_any_element(),
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
            .child(
                div()
                    .id("pdf-pages")
                    .key_context("PdfViewer")
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
                            if let Some(v) = &mut this.viewer {
                                let caret = v.caret_at(event.position);
                                v.anchor = caret;
                                v.head = caret;
                                v.dragging = caret.is_some();
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if let Some(v) = &mut this.viewer
                            && v.dragging
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
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if let Some(v) = &mut this.viewer {
                                v.dragging = false;
                                if v.anchor.is_some() && v.anchor == v.head {
                                    // A click without a drag clears the selection.
                                    v.anchor = None;
                                    v.head = None;
                                }
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
}
