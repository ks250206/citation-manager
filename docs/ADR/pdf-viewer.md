# #17: bounded PDF viewer and character-based selection

Status: accepted for #17. Builds on #11 (PDFium worker), #15 (Details › PDF slot) and #16.

## Boundaries

- `cm_core::geometry` now owns the page geometry (`Rect`, `Quad`, `Rotation`, `Viewport`) that
  #11 introduced in `cm-pdf`; `cm_pdf::geometry` re-exports it, so the PDF worker and the
  desktop use one implementation of CropBox/rotation/zoom mapping. `cm-desktop` still depends
  only on `cm-protocol` (which re-exports it); no PDF runtime enters the GUI crate.
- `PdfRequest::Render(RenderRequest)` → `PdfReply::Page(Result<RenderedPage, RenderFailure>)`.
  The library worker verifies the attachment's stored bytes against its recorded SHA-256
  (`ManagedStore::verified_path`) and submits one page to the serial PDF worker; a helper thread
  waits for the result, so rendering never blocks library reads/writes. The UI's
  `Cancellation` is bridged to the PDF worker's cooperative cancellation.
- Zoom is bounded (`0.1..=8` px/pt, device scale included) before any work; the PDF worker's
  existing 4 MP/8192 px bitmap limit still applies (`RenderFailure::TooLarge`).

## Viewer state (`desktop::pdf_viewer`, framework-free)

- Continuous column layout with per-page sizes (estimates until a page is rendered), visible
  range + 1 page prefetch, at most 3 renders in flight; renders that scroll out are cancelled.
  A page the worker rejects as `Busy` is held back for 300 ms (`BusyBackoff`), not resubmitted
  on the next repaint.
- `RenderCache`: LRU keyed by (attachment, page, render zoom ×1000) with a 192 MiB / 48-entry
  budget. Render zoom is bucketed to 0.05 so tiny layout changes reuse bitmaps. Only the
  visible/prefetch working set refreshes recency; building the page column reads cached
  bitmaps without reordering (`peek`), so offscreen pages are evicted before visible ones.
  (#30 lowered the budget to 64 MiB.)
- #85 page memory: the bitmap arrives as a raw binary IPC payload (no base64) and becomes the
  GPU image in place (no copy without a user rotation). A page is drawn by a canvas that calls
  `paint_image` and then flags the page as uploaded; from the next message-pump tick
  (`release_page_memory`, every 16 ms) the cache keeps only a 1x1 stand-in `RenderImage`
  with the same image id (`PageImage`, `texture_only`), so the CPU copy is freed while the
  atlas texture keeps showing the page. Re-display (scroll back, repaint) draws from that
  texture; a page whose texture is released (eviction, rotation, another document) leaves
  the cache with it and is rendered again, never drawn from the stand-in
  (`drawn_pages_release_their_cpu_pixels_and_redisplay_from_the_texture`). Textures are
  released (`drop_image`) from the message pump, outside window updates: inside one,
  `drop_image(_, None)` cannot reach the window being updated and the texture stayed in its
  atlas (rotation and document switches leaked textures before #85). The byte budget still
  counts each cached page, so it now bounds texture memory.
  Limitation: if the GPU renderer clears its atlas (device loss or repeated GPU errors), a
  released page would be redrawn from its 1x1 stand-in until it leaves the cache; changing
  zoom or rotation re-renders it.
- papersd and the GUI re-execute themselves once at start on glibc Linux with
  `GLIBC_TUNABLES=glibc.malloc.mmap_threshold=262144` (`cm_daemon::malloc_tuning`), so freed
  bitmap-sized buffers go back to the OS instead of staying in the heap (glibc's dynamic
  threshold otherwise serves them from the heap after the first one is freed). A threshold the
  user already set (in `GLIBC_TUNABLES` or `MALLOC_MMAP_THRESHOLD_`) is left alone. Measurements:
  [ISSUE_30 #85](../validation/ISSUE_30.md#85-pdf-閲覧時のメモリ削減).
- User rotation is applied to the bitmap client-side (`rotate_rgba`); the effective rotation for
  geometry is page /Rotate + user turns. A test proves the rotated bitmap and the effective
  `Viewport` agree pixel-for-pixel for all 16 combinations.
- Fit Width/Page use the first page as reference so zoom does not jump while paging. Explicit
  −/+ zoom applies even before any page has rendered (`layout_scale`), so a first page that is
  too large at 100% (`TooLarge`, no geometry yet) is recovered by zooming out.
- Selection uses PDFium character quads in PDF space: hit-test the character under the pointer
  (or the nearest on the same line), select an inclusive range in text order across pages, and
  copy only characters PDFium mapped to Unicode (never guessed).
- States: encrypted, damaged, missing/corrupt blob and missing runtime replace the page column
  with a message; image-only pages are marked "no selectable text". Bibliographic data and the
  other tabs are unaffected.

PDF commands/keymap (page jump, search, annotations) remain #21; annotations #19; export #20.
