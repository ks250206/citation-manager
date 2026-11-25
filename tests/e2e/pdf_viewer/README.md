# #17 PDF viewer checklist

Automated (default lane): `crates/desktop/tests/pdf_viewer.rs` (bitmap rotation ≡ shared viewport
for all page/user rotations, zoom/fit bounds, layout/visibility/prefetch, LRU byte/entry budget,
character hit-testing and multi-line/multi-page text), `crates/daemon/tests/pdf_render.rs`
(zoom validation, unknown/cancelled requests, tampered blob detected before PDFium, library
work continues), `crates/core` geometry round-trip tests, and the cm-pdf runtime corpus tests
with `CM_PDFIUM_LIBRARY`.

Native checks (installed bundle so PDFium resolves: macOS `.app`, Linux `install_linux.py`):

1. Select a paper with an imported PDF → PDF tab: the attachment opens; page 1 renders, then
   neighbours. `‹ ›`, the page field (↩) and scrolling move through all pages; the counter follows.
2. −/+ step zoom; Width/Page fit to the pane; Expand (reading mode) refits to the full width.
3. ⟳ rotates all pages clockwise; pages with their own /Rotate stay correct.
4. Drag across text: highlight follows characters across lines/pages; Copy or ⌘C/Ctrl+C puts the
   exact Unicode text (including Japanese) on the clipboard. Click without drag clears.
5. Encrypted, damaged and image-only PDFs show a clear state; metadata and other tabs keep working.
6. Switching papers/attachments quickly never shows the previous document's pages; memory stays
   bounded on long documents at high zoom.
