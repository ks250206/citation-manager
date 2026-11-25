# #19 annotations checklist

Automated: `crates/core/tests/annotations.rs` (validation, exact-state undo/redo with conflict
refusal, AI provenance/confirmation, carry-over + proposal search), `crates/daemon/tests/annotations.rs`
(SQLite persistence across reopen, page order, undo/redo/delete-restore, stale undo refused, AI
confirm, unresolved carry-over and explicit placement), `crates/desktop/tests/annotations.rs`
(selection → per-line quads, quote, bounded context, search/manual placement of unresolved notes).

Native (installed bundle):
1. Drag across text (multi-line, Japanese, rotated page) → choose a colour → Highlight. Restart:
   same position and text. Page boundaries create one highlight per page.
2. Click a highlight: change colour, edit/save comment (↩), delete. ⌘Z/Ctrl+Z and ⇧⌘Z/Ctrl+Shift+Z
   in the viewer (and Notes › Undo/Redo) step through every change; the DB and list agree.
3. ＋ Note adds a page note (📝); Notes lists everything in page order; clicking a row opens the
   PDF at that page with the annotation selected.
4. AI annotations (source=ai via API/MCP) show "AI · review" with Confirm / Remove.
5. Attach a replacement PDF: "Copy annotations from …" creates unresolved copies; "Find position"
   proposes a page (context match shown); nothing moves until "Place here".
6. A copied page note (no quoted text) shows "Place in PDF…" instead of a search. It opens the PDF
   with the note selected; go to a page (‹ ›, page field ↩, or J/K with Vim keys) and choose
   "Place on p. N", or run "Place Note on Current Page" from ⌘K/Ctrl+K. The 📝 marker appears on
   that page; undo unplaces it.
