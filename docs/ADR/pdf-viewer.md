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
- `RenderCache`: LRU keyed by (attachment, page, render zoom ×1000) with a 192 MiB / 48-entry
  budget. Render zoom is bucketed to 0.05 so tiny layout changes reuse bitmaps.
- User rotation is applied to the bitmap client-side (`rotate_rgba`); the effective rotation for
  geometry is page /Rotate + user turns. A test proves the rotated bitmap and the effective
  `Viewport` agree pixel-for-pixel for all 16 combinations.
- Fit Width/Page use the first page as reference so zoom does not jump while paging.
- Selection uses PDFium character quads in PDF space: hit-test the character under the pointer
  (or the nearest on the same line), select an inclusive range in text order across pages, and
  copy only characters PDFium mapped to Unicode (never guessed).
- States: encrypted, damaged, missing/corrupt blob and missing runtime replace the page column
  with a message; image-only pages are marked "no selectable text". Bibliographic data and the
  other tabs are unaffected.

PDF commands/keymap (page jump, search, annotations) remain #21; annotations #19; export #20.
