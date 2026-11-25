# #19: highlights, comments, notes, undo/redo and anchors

Status: accepted for #19. Builds on #8 (annotations table), #17 (viewer, shared geometry).

- **Model** (`cm_core::annotations`): kind highlight|note; source human|ai|import; anchor
  resolved{page_index 0-based, page_label, quads in unrotated PDF points} or unresolved{reason};
  quote, ≤48-char context before/after, colour (named palette or `#RRGGBB`), comment, and
  `confirmed_at` (new column, migration `20261124T120000Z_issue-19_001`, only for source=ai).
  Validation bounds quote/comment (64 KiB), context, quads (finite, 1–1024 for highlights),
  page index (≤ 4294967294), page label (256 B), unresolved reason (1 KiB) and colours. A note
  needs non-blank text however it is anchored; whitespace-only comments are stored as none.
- **Service** is the only writer. Every change returns an `Edit` (Created/Updated/Deleted with full
  rows). Undo applies the inverse, redo the edit, both restoring *exact* rows (ids, timestamps) and
  only from the state the edit expects — otherwise `Conflict`, so an AI agent's or another window's
  newer change is never clobbered. `History` (bounded 200) lives in the UI.
- **Replayed edits are untrusted input.** Every row an undo/redo writes passes the same validation
  as a new write; `confirmed_at` is only valid on `source=ai`; an `Updated` edit may not change
  id, attachment, kind, source, quote, context or `created_at`. A written `confirmed_at` must be
  the row's current value or one the service itself recorded (Confirm, carry-over, or seen on a
  row it deleted/overwrote) in the writer's bounded `Confirmations` ledger, kept for the library
  worker's lifetime (the GUI history lives no longer). So undoing the removal of a confirmed AI
  annotation restores it exactly, but a client cannot invent a confirmation through `Redo`.
- **GUI history** is per paper: every request carries the paper it was sent for and replies for
  another paper are ignored; one undo/redo is in flight at a time (buttons disabled); a step that
  fails for a reason other than `Conflict` (or could not be sent) is kept for a retry.
- **AI**: stays `source=ai` for provenance; Confirm records `confirmed_at`; Remove is an ordinary
  undoable delete. Full-text indexing of annotation text belongs to #18's triggers; this issue only
  writes the source table.
- **PDF replacement**: `CarryOver` copies annotations to the new attachment as *unresolved*, in
  one transaction, keeping source and AI confirmation. Rows already on the target (same kind,
  quote and context; for notes also the text) are skipped, so repeating it copies nothing; the
  Notes tab only offers a source that still has something to copy (`pending_carry_over`).
  `Propose` searches the new PDF's characters for the quote (preferring matching context) and
  returns a proposal; nothing is placed until `Resolve` after the person confirms, and `Resolve`
  only places unresolved annotations. A note can also be placed by hand: page notes have no quote,
  so their copies cannot be searched for. The Notes tab offers "Place in PDF…", which opens the PDF
  with the note selected; "Place on p. N" in the viewer (or the `pdf.place_note` palette command)
  sends `Resolve` for the page being viewed with no quads (`desktop::annotations::place_on_page`).
  Highlights still need a search proposal, since they need text quads. Quote and context use one
  text rule everywhere: characters without Unicode are skipped. Matches are counted up to 1000.
- **Propose off the library worker**: the worker only looks up rows; checksum verification and
  text-only page snapshots (`zoom: None`, no bitmap) run on a helper thread through a cloneable
  PDF-worker submitter, under a 120 s overall deadline and the request's `Cancellation` (the GUI
  cancels on a new search or a paper switch). PDFs over 2000 pages are refused explicitly.
  Under papersd (#25) the PDF worker is the process-wide shared PDFium worker (also used by #18
  extraction and #17 rendering). The token is not serialized (`#[serde(skip)]`): the IPC client
  turns its cancellation into a `Cancel` frame for the request id, and papersd gives the request
  its own server-side token, cancelled by that frame or when the connection closes.
- **Viewer**: selection → one highlight per page with per-line quads (`desktop::annotations`).
  Overlays draw quads with the shared viewport, so rotation/zoom/CropBox match the text layer.
  A press always starts a text selection; only a click without a drag (≤ 3 px) selects the
  topmost annotation under it (note markers, then highlights in reverse drawing order). Page
  notes get fixed-size markers stacked down the left margin, each with its own hit rectangle.
- **Export** (#20, [pdf-export](pdf-export.md)): highlights/comments/notes become standard PDF
  annotations in a verified copy; unresolved anchors are listed, never silently dropped.
- Opening a library created before this migration goes through `open_with_safety_backups`
  (pre-migration backup) in the in-process worker, matching the #25 daemon.
