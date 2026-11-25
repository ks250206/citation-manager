//! Pure viewer state; native rendering/scrolling/selection is checked in tests/e2e/pdf_viewer.
use cm_desktop::pdf_viewer::*;
use cm_protocol::{
    attachments::{PageChar, Rect, Rotation},
    domain::AttachmentId,
};
use std::collections::{BTreeMap, HashMap};

fn attachment(n: u8) -> AttachmentId {
    AttachmentId(
        format!("00000000-0000-0000-0000-0000000000{n:02}")
            .parse()
            .unwrap(),
    )
}
fn ch(c: char, left: f32, bottom: f32) -> PageChar {
    let r = Rect {
        left,
        bottom,
        right: left + 6.0,
        top: bottom + 10.0,
    };
    PageChar {
        unicode: Some(c),
        quad: r.quad(),
    }
}
fn line(text: &str, bottom: f32) -> Vec<PageChar> {
    text.chars()
        .enumerate()
        .map(|(i, c)| ch(c, 10.0 + i as f32 * 6.0, bottom))
        .collect()
}

#[test]
fn rotated_bitmap_matches_effective_viewport_for_every_rotation() {
    // A 3x2 bitmap rendered with the page's own rotation; user turns are applied client-side.
    let crop = Rect {
        left: 5.0,
        bottom: 7.0,
        right: 8.0,
        top: 9.0,
    };
    for page_rotation in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
        let base = viewport(crop, page_rotation, 1.0).unwrap();
        let (w, h) = base.size();
        let (w, h) = (w as u32, h as u32);
        // Pixel value encodes the PDF point it shows (centre of each pixel).
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let p = base.to_pdf(Point {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                });
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = (p.x * 2.0) as u8;
                rgba[i + 1] = (p.y * 2.0) as u8;
                rgba[i + 3] = 255;
            }
        }
        for turns in 0..4u8 {
            let (rw, rh, rotated) = rotate_rgba(w, h, rgba.clone(), turns);
            let effective = viewport(crop, rotate(page_rotation, turns), 1.0).unwrap();
            assert_eq!(effective.size(), (rw as f32, rh as f32));
            for y in 0..rh {
                for x in 0..rw {
                    let p = effective.to_pdf(Point {
                        x: x as f32 + 0.5,
                        y: y as f32 + 0.5,
                    });
                    let i = ((y * rw + x) * 4) as usize;
                    assert_eq!(
                        (rotated[i], rotated[i + 1]),
                        ((p.x * 2.0) as u8, (p.y * 2.0) as u8),
                        "page {page_rotation:?} + {turns} turns at ({x},{y})"
                    );
                }
            }
        }
    }
    // Malformed buffers are returned unchanged rather than panicking.
    assert_eq!(rotate_rgba(2, 2, vec![1, 2, 3], 1), (2, 2, vec![1, 2, 3]));
}

#[test]
fn zoom_modes_and_steps_are_bounded() {
    let page = (600.0, 800.0);
    assert_eq!(scale_for(ZoomMode::Scale(1.5), page, (100.0, 100.0)), 1.5);
    let fit_width = scale_for(ZoomMode::FitWidth, page, (624.0, 300.0));
    assert!((fit_width - 1.0).abs() < 1e-4);
    let fit_page = scale_for(ZoomMode::FitPage, page, (624.0, 424.0));
    assert!((fit_page - 0.5).abs() < 1e-4);
    assert_eq!(
        scale_for(ZoomMode::FitWidth, (0.0, 0.0), (500.0, 500.0)),
        1.0
    );
    assert_eq!(scale_for(ZoomMode::Scale(100.0), page, page), MAX_SCALE);
    assert_eq!(step_zoom(1.0, true), 1.25);
    assert_eq!(step_zoom(1.0, false), 0.8);
    assert_eq!(step_zoom(4.0, true), MAX_SCALE);
    assert_eq!(step_zoom(0.5, false), MIN_SCALE);
    assert_eq!(render_zoom(1.0, 2.0), 2.0);
    assert_eq!(render_zoom(1.013, 1.0), 1.0);
    assert_eq!(render_zoom(100.0, 2.0), 8.0);
    assert_eq!(
        rotated_points(
            Rect {
                left: 0.0,
                bottom: 0.0,
                right: 10.0,
                top: 20.0
            },
            Rotation::R90
        ),
        (20.0, 10.0)
    );
}

#[test]
fn layout_visibility_and_prefetch() {
    let mut sizes = HashMap::new();
    sizes.insert(0, (100.0, 200.0));
    sizes.insert(2, (100.0, 100.0));
    let layout = page_offsets(4, &sizes, 1.0);
    assert_eq!(layout[0], (PAGE_GAP, 200.0));
    // Unknown page 1 uses the previous size as an estimate.
    assert_eq!(layout[1].1, 200.0);
    assert_eq!(layout[2].1, 100.0);
    assert_eq!(layout[3].1, 100.0);
    assert_eq!(visible_pages(&layout, 0.0, 150.0), 0..1);
    assert_eq!(visible_pages(&layout, 250.0, 150.0), 1..2);
    assert_eq!(visible_pages(&layout, 250.0, 280.0), 1..3);
    assert_eq!(visible_pages(&layout, 10_000.0, 10.0), 4..4);
    assert_eq!(wanted_pages(1..2, 4), vec![1, 0, 2]);
    assert_eq!(wanted_pages(0..1, 1), vec![0]);
    assert_eq!(wanted_pages(3..4, 4), vec![3, 2]);
    assert!(wanted_pages(0..0, 0).is_empty());
}

#[test]
fn cache_is_lru_bounded_by_bytes_and_entries() {
    let a = attachment(1);
    let b = attachment(2);
    let mut cache: RenderCache<&str> = RenderCache::new(100, 3);
    assert!(cache.insert(RenderKey::new(a, 0, 1.0), "p0", 40).is_empty());
    assert!(cache.insert(RenderKey::new(a, 1, 1.0), "p1", 40).is_empty());
    assert_eq!(cache.get(&RenderKey::new(a, 0, 1.0)), Some(&"p0"));
    // Over budget: the least recently used (page 1) goes and is handed back.
    assert_eq!(
        cache.insert(RenderKey::new(a, 2, 1.0), "p2", 40),
        vec!["p1"]
    );
    assert!(!cache.contains(&RenderKey::new(a, 1, 1.0)));
    assert!(cache.contains(&RenderKey::new(a, 0, 1.0)));
    assert_eq!(cache.bytes(), 80);
    // Re-inserting a key returns the replaced value.
    assert_eq!(
        cache.insert(RenderKey::new(a, 2, 1.0), "p2b", 40),
        vec!["p2"]
    );
    // Different zoom is a different entry.
    assert!(!cache.contains(&RenderKey::new(a, 0, 2.0)));
    assert_eq!(
        cache.insert(RenderKey::new(a, 9, 1.0), "huge", 101),
        vec!["huge"]
    );
    assert!(cache.insert(RenderKey::new(b, 0, 1.0), "b0", 10).is_empty());
    assert_eq!(cache.len(), 3);
    let mut dropped = cache.clear_except(Some(b));
    dropped.sort();
    assert_eq!(dropped, vec!["p0", "p2b"]);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.bytes(), 10);
    assert_eq!(cache.drain(), vec!["b0"]);
    assert!(cache.is_empty() && cache.bytes() == 0);
}

#[test]
fn explicit_zoom_applies_before_the_first_page_has_rendered() {
    // First page too large at 100%: no geometry yet, and the pane may not be measured.
    let mut scale = 1.0;
    for expected in [0.8, 0.67, 0.5, MIN_SCALE] {
        let mode = ZoomMode::Scale(step_zoom(scale, false));
        scale = layout_scale(mode, None, (0.0, 0.0), scale);
        assert_eq!(scale, expected);
    }
    assert_eq!(
        layout_scale(ZoomMode::Scale(9.0), None, (500.0, 500.0), 1.0),
        MAX_SCALE
    );
    // Fit modes need the reference page and a measured pane; until then the scale is kept.
    for mode in [ZoomMode::FitWidth, ZoomMode::FitPage] {
        assert_eq!(layout_scale(mode, None, (624.0, 900.0), 0.67), 0.67);
        assert_eq!(
            layout_scale(mode, Some((600.0, 800.0)), (0.0, 0.0), 0.67),
            0.67
        );
    }
    assert_eq!(
        layout_scale(
            ZoomMode::FitWidth,
            Some((600.0, 800.0)),
            (624.0, 900.0),
            0.67
        ),
        1.0
    );
}

#[test]
fn busy_renders_wait_for_the_retry_delay_before_resubmission() {
    use std::time::{Duration, Instant};
    let a = attachment(1);
    let key = RenderKey::new(a, 3, 1.5);
    let other = RenderKey::new(a, 4, 1.5);
    let t0 = Instant::now();
    let mut busy = BusyBackoff::default();
    assert!(!busy.is_deferred(&key, t0));
    busy.defer(key, t0);
    // The repaint right after the reply, and every frame within the delay, must skip it.
    for ms in [0, 16, 150, 299] {
        let now = t0 + Duration::from_millis(ms);
        busy.prune(now);
        assert!(busy.is_deferred(&key, now), "resubmitted after {ms} ms");
        assert!(!busy.is_deferred(&other, now));
    }
    let due = t0 + BUSY_RETRY;
    assert!(!busy.is_deferred(&key, due));
    busy.prune(due);
    assert!(busy.is_empty());
    // Zoom/rotation changes start over.
    busy.defer(key, due);
    busy.defer(other, due);
    assert_eq!(busy.len(), 2);
    busy.clear();
    assert!(!busy.is_deferred(&key, due));
}

#[test]
fn building_the_page_column_does_not_evict_the_working_set() {
    let a = attachment(1);
    let key = |p: u32| RenderKey::new(a, p, 1.0);
    let mut cache: RenderCache<u32> = RenderCache::new(1_000, 4);
    // Browsed to the end of the document: later pages fill the cache.
    for p in 6..10 {
        cache.insert(key(p), p, 1);
    }
    // Scrolled back to the top: pages 0 and 1 are visible/prefetched.
    let working = [0u32, 1];
    let mut renders = 0;
    for _frame in 0..10 {
        // `schedule_renders`: refresh the working set and request one missing page.
        for p in working.iter().rev() {
            cache.get(&key(*p));
        }
        let requested = working.iter().copied().find(|p| !cache.contains(&key(*p)));
        // The page column then reads every page in document order, including offscreen ones.
        for p in 0..10 {
            let _ = cache.peek(&key(p));
        }
        // The render reply arrives after the frame.
        if let Some(p) = requested {
            renders += 1;
            cache.insert(key(p), p, 1);
        }
    }
    assert_eq!(
        renders, 2,
        "each working page renders once, without thrashing"
    );
    assert!(working.iter().all(|p| cache.contains(&key(*p))));
    assert_eq!(cache.peek(&key(0)), Some(&0));
}

#[test]
fn render_zoom_is_capped_to_the_worker_bitmap_limits() {
    use cm_protocol::geometry::{MAX_RENDER_PIXELS, MAX_RENDER_SIDE};
    let letter = (612.0, 792.0);
    // 2x display, Fit Width on a wide pane: would be ~5 MP uncapped.
    let zoom = capped_zoom(3.3, letter);
    assert!(zoom < 3.3);
    let (w, h) = ((letter.0 * zoom).ceil(), (letter.1 * zoom).ceil());
    assert!(w * h <= MAX_RENDER_PIXELS as f32);
    // Very tall/narrow pages are bounded by side length.
    let strip = (100.0, 20_000.0);
    let zoom = capped_zoom(2.0, strip);
    assert!((strip.1 * zoom).ceil() <= MAX_RENDER_SIDE);
    // Small zooms are untouched; degenerate sizes pass through.
    assert_eq!(capped_zoom(1.0, letter), 1.0);
    assert_eq!(capped_zoom(1.5, (0.0, 0.0)), 1.5);
}

#[test]
fn selection_hits_characters_and_spans_lines_and_pages() {
    let mut first = line("Hello", 100.0);
    first.extend(line("world", 80.0));
    let second = line("日本語", 100.0);
    // Inside a glyph.
    assert_eq!(hit_char(&first, Point { x: 11.0, y: 105.0 }), Some(0));
    // Past the end of the second line: nearest on that line.
    assert_eq!(hit_char(&first, Point { x: 500.0, y: 85.0 }), Some(9));
    // Between lines: nothing.
    assert_eq!(hit_char(&first, Point { x: 11.0, y: 95.0 }), None);
    let start = TextPos { page: 0, index: 3 };
    let end = TextPos { page: 1, index: 1 };
    let mut pages = BTreeMap::new();
    pages.insert(0, first.as_slice());
    pages.insert(1, second.as_slice());
    assert_eq!(selected_text(&pages, end, start), "loworld\n日本");
    assert_eq!(page_range(0, first.len(), start, end), Some(3..=9));
    assert_eq!(page_range(1, second.len(), start, end), Some(0..=1));
    assert_eq!(page_range(2, 5, start, end), None);
    // Unmapped glyphs are skipped, not guessed.
    let mut odd = line("ab", 0.0);
    odd[1].unicode = None;
    let mut only = BTreeMap::new();
    only.insert(0, odd.as_slice());
    assert_eq!(
        selected_text(
            &only,
            TextPos { page: 0, index: 0 },
            TextPos { page: 0, index: 1 }
        ),
        "a"
    );
}

/// #30: the default budget stays inside DESIGN §16.2's 64–128 MB, still holds two visible
/// pages plus `PREFETCH` on each side at the 4 Mpx render cap, and a whole 20-page document
/// at that size never exceeds it (the GUI also holds a GPU copy of each cached bitmap).
#[test]
fn default_render_cache_budget_is_within_the_design_range() {
    const MIB: usize = 1024 * 1024;
    assert!((64 * MIB..=128 * MIB).contains(&CACHE_BUDGET_BYTES));
    let page = cm_protocol::geometry::MAX_RENDER_PIXELS * 4;
    assert!(CACHE_BUDGET_BYTES >= (2 + 2 * PREFETCH as usize) * page);
    let a = attachment(1);
    let mut cache: RenderCache<u32> = RenderCache::default();
    let mut evicted = 0;
    for p in 0..20 {
        evicted += cache.insert(RenderKey::new(a, p, 2.18), p, page).len();
        assert!(cache.bytes() <= CACHE_BUDGET_BYTES);
    }
    assert_eq!(cache.len(), CACHE_BUDGET_BYTES / page);
    assert_eq!(evicted, 20 - cache.len());
}

/// A page image as the GUI sees it: an atlas identity and, until released, its pixels.
#[derive(Debug, Clone, PartialEq)]
struct FakeImage {
    id: u32,
    pixels: Option<Vec<u8>>,
}
/// Stand-in for the GPU texture atlas: uploads on the first draw of an id, like
/// `Window::paint_image`, and panics if it ever has to upload a released (pixel-less) image.
#[derive(Default)]
struct FakeAtlas(HashMap<u32, Vec<u8>>);
impl FakeAtlas {
    fn draw(&mut self, page: &PageImage<FakeImage>) -> Vec<u8> {
        let image = page.image();
        let texture = self.0.entry(image.id).or_insert_with(|| {
            image
                .pixels
                .clone()
                .expect("a released page image must never be uploaded")
        });
        page.upload_flag().set(true);
        texture.clone()
    }
    fn drop_image(&mut self, image: FakeImage) {
        self.0.remove(&image.id);
    }
}
fn texture_only(image: &FakeImage) -> FakeImage {
    FakeImage {
        id: image.id,
        pixels: None,
    }
}

#[test]
fn drawn_pages_release_their_cpu_pixels_and_redisplay_from_the_texture() {
    let doc = attachment(9);
    let key = |page| RenderKey::new(doc, page, 1.0);
    let image = |id: u32| FakeImage {
        id,
        pixels: Some(vec![id as u8; 40]),
    };
    let mut atlas = FakeAtlas::default();
    let mut cache: RenderCache<PageImage<FakeImage>> = RenderCache::new(120, 10);
    for page in 0..2 {
        assert!(
            cache
                .insert(key(page), PageImage::new(image(page)), 40)
                .is_empty()
        );
    }
    // Prefetched pages that were never drawn keep their pixels: nothing uploaded them yet.
    assert_eq!(cache.release_uploaded(texture_only), 0);
    assert_eq!(cache.pages_holding_pixels(), 2);

    // First draw uploads; the next release frees the CPU copy but keeps the identity.
    assert_eq!(atlas.draw(cache.peek(&key(0)).unwrap()), vec![0; 40]);
    assert_eq!(cache.release_uploaded(texture_only), 1);
    let released = cache.peek(&key(0)).unwrap();
    assert!(!released.holds_pixels());
    assert_eq!(released.image().pixels, None);
    assert_eq!(released.image().id, 0);
    assert_eq!(cache.release_uploaded(texture_only), 0, "released once");
    assert_eq!(cache.bytes(), 80, "the budget still counts the texture");

    // Re-display (scroll back, repaint, zoom back to this bucket): still cached, so it is
    // not re-requested, and it draws the original pixels from the texture.
    assert!(cache.contains(&key(0)));
    for _ in 0..3 {
        assert_eq!(atlas.draw(cache.get(&key(0)).unwrap()), vec![0; 40]);
    }

    // Eviction hands back the stand-in, whose identity releases the texture; the page is
    // then rendered again (not cached) and uploads its new pixels on the next draw.
    atlas.draw(cache.peek(&key(1)).unwrap());
    cache.get(&key(1));
    assert!(
        cache
            .insert(key(2), PageImage::new(image(2)), 40)
            .is_empty()
    );
    let evicted = cache.insert(key(3), PageImage::new(image(3)), 40);
    assert_eq!(evicted.len(), 1);
    let evicted = evicted.into_iter().next().unwrap().into_image();
    assert_eq!(evicted, texture_only(&image(0)));
    atlas.drop_image(evicted);
    assert!(!atlas.0.contains_key(&0));
    assert!(
        !cache.contains(&key(0)),
        "an evicted page must be rendered again"
    );
    let fresh = FakeImage {
        id: 10,
        pixels: Some(vec![7; 40]),
    };
    for old in cache.insert(key(0), PageImage::new(fresh), 40) {
        atlas.drop_image(old.into_image());
    }
    assert_eq!(atlas.draw(cache.peek(&key(0)).unwrap()), vec![7; 40]);

    // Rotation / another document drain everything, released or not: no texture outlives
    // its cache entry.
    cache.release_uploaded(texture_only);
    for old in cache.drain() {
        atlas.drop_image(old.into_image());
    }
    assert!(atlas.0.is_empty());
    assert_eq!(cache.pages_holding_pixels(), 0);
}

#[test]
fn unrotated_bitmaps_are_not_copied() {
    let rgba = vec![9u8; 2 * 3 * 4];
    let before = rgba.as_ptr();
    let (w, h, out) = rotate_rgba(2, 3, rgba, 0);
    assert_eq!((w, h), (2, 3));
    assert_eq!(out.as_ptr(), before);
}
