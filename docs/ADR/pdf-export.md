# #20: annotated PDF export and re-read verification

Status: accepted for #20. Builds on #11 (PDFium runtime, [pdfium-runtime](pdfium-runtime.md)),
#17 (viewer geometry), #19 ([annotations](annotations.md)), #16 (command registry,
[desktop-commands-keymap](desktop-commands-keymap.md)) and #25 ([daemon-ipc](daemon-ipc.md)).
DESIGN §9.3 / §21.2. Evidence: [ISSUE_20](../validation/ISSUE_20.md).

## Flow (DESIGN §9.3 steps 1–5)

1. **Plan** (`cm_core::pdf_export::plan`, pure): every #19 annotation of the attachment becomes a
   standard PDF annotation or an explicit `Omitted { id, kind, reason }`.
   - Highlight → `/Subtype /Highlight`, one `/QuadPoints` quad per stored line quad (multi-line),
     `/C` = the viewer palette colour (`annotations::rgb`, shared with the overlay), `/CA` = 0.5
     (`HIGHLIGHT_OPACITY`; the DB stores no opacity), `/Contents` = the comment (Unicode).
   - Note → `/Subtype /Text`, `/Contents` = the note text, `/C` = its colour, 24 pt icon. A
     page-level note (no quads) is placed by `note_slot` in the *visible* top-left corner after
     `/Rotate`, stacked per page; a note with quads is anchored at their top-left and shifted
     inside the crop if it would stick out. A position entirely off the page is an error.
   - Omitted: unresolved anchors (e.g. carried over from a replaced PDF), highlights without
     quads or with non axis-aligned quads, notes without text.
2. **Export** (`cm_pdf` `Operation::Export`, serial PDFium thread): loads the stored original
   *from bytes* (the file is only read), appends the annotations after any existing ones, sets
   `/NM` = the DB annotation UUID, print flag, colour/opacity, then a persistent **normal
   appearance** `/AP /N` (below), and returns the saved bytes plus the final note rectangles.
   Existing annotations are never touched: no recolouring, no `/AP` added or rewritten (the
   pdfium-render colour setters are only called on the new annotations, before their appearance).
3. **Write** (daemon helper thread): `tempfile` in the destination directory
   (`.citation-manager-export-*.pdf.partial`, mode 0600), `fsync`.
4. **Re-read and verify**: `Operation::Annotations` re-reads the source and the temp file
   *without rendering* (rendering lets PDFium synthesise appearances in memory and would hide a
   missing `/AP`); `pdf_export::verify` then requires: every source annotation unchanged and first
   on its page; each planned annotation exactly once by `/NM` with its subtype, page, contents,
   QuadPoints/Rect within 0.01 pt, and an appearance whose first filled path has the planned colour
   (±1) and opacity (±2/255); nothing else added. The temp file's bytes/page count must equal the
   export; the original's SHA-256 is recomputed and must equal the stored attachment digest.
5. **Persist**: `persist_noclobber` (never replaces), or — only when the person confirmed replacing
   that file in the native save dialog (`replace_existing`) — an atomic rename over an existing
   *ordinary* file (no symlink/hard link/directory). Then the directory is fsynced.

Every failure returns `PdfExportFailure` and leaves nothing at the destination (the temp file is
deleted on drop): `MissingOrCorrupt` (stored hash mismatch), `Encrypted`, `Damaged`,
`Unexportable(omitted)`, `InvalidAnnotation`, `Destination(reason)`,
`VerificationFailed(problems)`, `OriginalChanged`, `Cancelled`, `Busy`, `RuntimeUnavailable`.
`Unexportable` is returned *before any PDF work* unless the request sets `allow_omissions`; with it
the copy is written and the report lists the omissions (`PdfExportReport::complete() == false`),
which the GUI shows as **INCOMPLETE** in error styling — never as a plain success.

Destinations: absolute, parent an existing directory reached without symlinks (`files::safe`),
never inside the library directory (so a stored original can never be a destination), and an
existing file is refused unless `replace_existing`.

## Appearance streams in Rust (replaces the M0 pypdf completion)

PDFium 8125 saves new annotations without `/AP` (M0 finding). The product must not depend on
Python, so `crates/pdf/src/appearance.rs` calls `FPDFAnnot_SetAP` (normal mode) with an ASCII
content stream in page space: PDFium sets the form `/BBox` to the annotation `/Rect` (identity
matrix) and, because `/CA` < 1, adds `/Resources /ExtGState /GS` (CA/ca = 128/255, BM Normal),
which the highlight stream selects with `/GS gs` before filling each quad polygon
(TL→TR→BR→BL). Notes draw a filled square, border and three lines. `FPDFAnnot_SetStringValue`
sets `/NM`. Observed output is in ISSUE_20 (FlateDecode form XObjects).

**Unsafe exception.** pdfium-render 0.9.5 exposes these functions only as `unsafe` bindings and
keeps the annotation handle crate-private, so `appearance.rs` obtains a second annotation handle
with `FPDFPage_GetAnnot(page, last_index)`, sets the values and closes it, after the high-level
wrapper for that annotation is dropped. It is the only `unsafe` in the workspace: cm-pdf sets
`unsafe_code = "deny"` instead of inheriting the workspace `forbid`, and only that module opts in
(`#![allow(unsafe_code)]`, SAFETY comment). Everything still runs on the `cm-pdf-serial` thread
and no handle escapes. A wrapper upgrade that exposes safe setters should remove the exception.

The colour getters of the wrapper are never used on read: on an annotation with an appearance
their fallback treats the annotation handle as a page object. Verification reads the parsed
appearance's page objects (`objects()`, first path fill colour incl. graphics-state alpha).

## Viewer differences (independent check)

`scripts/pdfium/validate_export.py` re-checks the files from the cm-pdf runtime test without
PDFium: pypdf for structure (`/Subtype`, `/NM`, Unicode `/Contents`, `/QuadPoints`, `/Rect`, `/C`,
`/CA`, print flag, `/AP /N` form with `/BBox` = `/Rect`, `/GS` state, source annotations unchanged
— the AP-less legacy highlight stays AP-less) and poppler `pdftoppm -cropbox` for pixels on all four
rotations. Poppler draws *Text* icons upright (NoRotate semantics) pinned at the `/Rect`'s
unrotated upper-left corner, while PDFium and PDFKit (M0) rotate them with the page; on rotated
pages the icon therefore appears up to one icon size apart. `/Rect`, contents and colour are the
same; `note_slot` keeps one icon size free towards the far page edges so both stay visible. The
script checks each renderer with its own rule. Highlights render identically (form XObject).

The macOS gate (`scripts/pdfium/run.py` + `verify.py`, PDFKit) now inspects the Rust export
directly (no completion step) and checks `/AP`, `/C`, `/CA ≈ 0.5`, quads, contents and rasters.
It was not run for this change (no macOS runner here); see ISSUE_20.

## IPC, authorization, GUI

- `PdfRequest::Export(PdfExportRequest)` → wire `PdfCommand::Export { attachment, destination,
  replace_existing, allow_omissions }`; the cancellation token stays client-side (`ClientFrame::Cancel`)
  like `Render`. Policy `Operation::PdfExport` = `LibraryRead + LocalFileWrite` (MCP read-only and
  the native host are denied). Reply `PdfReply::Exported(Result<Box<PdfExportReport>, PdfExportFailure>)`.
- The library thread validates, resolves the hash-verified stored file and plans; PDFium, temp
  file, verification and persist run on a `pdf-export` helper thread with a cancellation bridge
  (one fresh worker token per PDF operation, because a finished ticket cancels its own token).
- GUI: `pdf.save` ("Save Current PDF…", `commands::pdf_export`) replaces the #16 deferred
  placeholder and is connected by the native host (palette, keymap, viewer toolbar "Save PDF…").
  `cx.prompt_for_new_path` chooses the file (suggested `<title> (annotated).pdf`); a second press
  while saving cancels; status bar shows progress and the outcome. On `Unexportable` the button
  becomes "Save without N unplaced…" for the same PDF and destination — an explicit second choice.

## Limits / not covered

Signed PDFs (signature validity after an incremental change is not preserved — PDFium rewrites
the file), password-protected sources (refused as `Encrypted`; the export never asks for a
password), arbitrary existing annotation subtypes beyond the tested Highlight/Square/Text
preservation, highlight blend mode Multiply (PDFium's `SetAP` resource uses Normal), popups for
comments, and FreeText. Export is bounded by the worker limits (32 MiB input/output, 1024
annotations, 4096 quads, 1 MiB text per request; at most 4096 annotations re-read per file).
