# #19 Linux native evidence

Debug build installed with `scripts/install_linux.py` (pinned PDFium), Xvfb + xdotool, scratch
library created before this change (so opening it ran the new migration through the
pre-migration safety backup — `backups/pre-migration-*.sqlite` was written).

| File | Shows |
|---|---|
| 01-highlight-created-selected.jpg | Drag-select → green swatch → Highlight: highlight drawn on the text, auto-selected with colour/comment/delete bar |
| 02-persisted-after-restart.jpg | After quitting and relaunching: the highlight is at the same position |
| 03-notes-list-with-comment.jpg | Comment "key result" saved with ↩; Notes tab lists p. 1, quote and comment |
| 04-page-note-marker.jpg | ＋ Note → typed text → ↩: 📝 marker on the page and a note row in Notes |

Also observed: Notes › Undo removed the comment and Redo restored it; clicking a highlight on
the page selects it. Found and fixed during the runs: (1) libraries created before #19 could not
be opened because the in-process worker used `LibraryDb::open` (no pre-migration backup hook);
it now opens with `open_with_safety_backups`; (2) "＋ Note" did not focus its text field.
Not verified: macOS, AI-created annotations from a real MCP client (covered by service and
worker tests), re-anchoring against a real replacement PDF in the GUI (the proposal search is
tested in core; the worker path needs the bundled PDFium and was not driven here).
