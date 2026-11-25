# #17 Linux native evidence

Debug build of `citation-manager` installed with `scripts/install_linux.py` (pinned
`libpdfium.so` in `<prefix>/lib/citation-manager/`), Xvfb + lavapipe, driven with xdotool,
against a scratch library whose papers have `fixtures/pdf/{corpus,encrypted,malformed}.pdf`
attached through the normal Inspect/Attach path.

| File | Shows |
|---|---|
| 01-pdf-tab-corpus.jpg | PDF tab opens the attachment; page 1/6 rendered by PDFium (Japanese text) |
| 02-reading-mode-rotated-page.jpg | Reading mode, Fit Page, page 2 with its own /Rotate |
| 03-text-selection.jpg | Drag selection highlight; status "Copied 19 characters" — `xclip` read `Unicode 日本語 αβ café` |
| 04-encrypted-state.jpg | Encrypted PDF message; bibliographic data unaffected |
| 05-damaged-state.jpg | Damaged PDF message |

Also observed: a two-line drag copied `Unicode 日本語 αβ café\nsecond line: coordinate proof`;
`›` advanced pages 1→2→3; ⟳ rotated pages client-side. Found and fixed during this run: a new
viewer requested nothing until the next input event because the pane had no size before its
first layout (now assumes a screenful and schedules one frame after measuring).
Not verified: macOS (Metal, ⌘C, trackpad scrolling), real GPU drivers, very large documents.
