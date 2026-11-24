# #19: highlights, comments, notes, undo/redo and anchors

Status: accepted for #19. Builds on #8 (annotations table), #17 (viewer, shared geometry).

- **Model** (`cm_core::annotations`): kind highlight|note; source human|ai|import; anchor
  resolved{page_index 0-based, page_label, quads in unrotated PDF points} or unresolved{reason};
  quote, ≤48-char context before/after, colour (named palette or `#RRGGBB`), comment, and
  `confirmed_at` (new column, migration `20261124T120000Z_issue-19_001`, only for source=ai).
  Validation bounds quote/comment (64 KiB), context, quads (1–1024 for highlights) and colours.
- **Service** is the only writer. Every change returns an `Edit` (Created/Updated/Deleted with full
  rows). Undo applies the inverse, redo the edit, both restoring *exact* rows (ids, timestamps) and
  only from the state the edit expects — otherwise `Conflict`, so an AI agent's or another window's
  newer change is never clobbered. `History` (bounded 200) lives in the UI.
- **AI**: stays `source=ai` for provenance; Confirm records `confirmed_at`; Remove is an ordinary
  undoable delete. Full-text indexing of annotation text belongs to #18's triggers; this issue only
  writes the source table.
- **PDF replacement**: `CarryOver` copies annotations to the new attachment as *unresolved*.
  `Propose` searches the new PDF's characters for the quote (preferring matching context) and
  returns a proposal; nothing is placed until `Resolve` after the person confirms.
- **Viewer**: selection → one highlight per page with per-line quads (`desktop::annotations`).
  Overlays draw quads with the shared viewport, so rotation/zoom/CropBox match the text layer.
- Opening a library created before this migration goes through `open_with_safety_backups`
  (pre-migration backup) in the in-process worker, matching the #25 daemon.
