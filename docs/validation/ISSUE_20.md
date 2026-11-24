# #20 validation: annotated PDF export and re-read verification

Run 2026-11-24 by the implementing agent in an isolated worktree (base `claude/integration-m2`
@ `b6d89ee`). Environment: Linux 6.18 x86_64 container, rustc/cargo 1.99.1, Python 3.13.18,
pinned PDFium chromium/8125 Linux x64 (`scripts/pdfium/fetch.py`, checksum-verified), poppler
`pdftoppm` 24.02.0, pypdf 6.1.1 / Pillow 13.0.0 from `scripts/pdfium/requirements.txt` in a venv.
Design: [pdf-export ADR](../ADR/pdf-export.md).

## Automated (default lanes)

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| `cargo clippy -p cm-daemon -p cm-desktop --all-targets --features desktop-bootstrap --locked -- -D warnings` | passed |
| `cargo clippy -p cm-pdf --all-targets --features runtime --locked -- -D warnings` | passed |
| `cargo clippy -p cm-daemon --all-targets --features pdfium-runtime-tests --locked -- -D warnings` | passed |
| `cargo test --workspace --locked --no-fail-fast` | passed: 318 tests, 0 failed (65 binaries) |
| `cargo test -p cm-daemon -p cm-desktop --features cm-daemon/desktop-bootstrap --locked --no-fail-fast` | passed: 136 tests, 0 failed (30 binaries), incl. `native::pdf_export_ui::tests::the_native_host_connects_pdf_save` |
| `python3 scripts/check_architecture.py` | passed (no new crate edge) |
| `python3 scripts/check_migrations.py --base origin/main` | passed (no migration added) |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | passed: 32 |

One earlier plain `cargo test --workspace` run hit `daemon_lifecycle::concurrent_starts_leave_exactly_one_writer`
(`library I/O: File exists`), a start-race in code this change does not touch; it passed 3/3 when
rerun alone and in the full `--no-fail-fast` run above.

New default-lane tests:

- `cm-core` `pdf_export` (5): plan mapping (multi-line quads, Unicode comment, palette colour,
  opacity, page-level and anchored notes) and every omission reason; shared palette; `note_slot`
  at the visible top-left for 0/90/180/270 with one icon kept free towards far edges; `verify`
  accepts a faithful export within 0.01 pt and reports each mismatch (quads, comment, missing
  `/AP`, colour/opacity, note position, subtype, recoloured/AP-stripped existing annotation,
  missing/duplicate/unexpected annotations).
- `cm-pdf` unit: appearance streams are ASCII page-space polygons; `/NM`, opacity and NUL validation.
- `cm-daemon` `pdf_export` (1, no PDFium): relative/inside-library/missing-parent destinations,
  existing file without confirmation (content kept), symlink/hard link/directory never replaced,
  unresolved annotation → `Unexportable` with nothing written, pre-cancel, unknown attachment,
  tampered stored blob → `MissingOrCorrupt`, runtime absence; no stray files in the folder.
- `daemon_auth`: native-host grant and MCP read-only grant are denied `Operation::PdfExport`.
- `cm-desktop` `pdf_export` (6): `pdf.save` is no longer a deferred placeholder; the
  `commands::pdf_export` provider registers through `CommandRegistry` (duplicate refused) and
  resolves Enabled from a keymap sequence and the palette when connected; request → IPC
  `PdfCommand::Export` with client-side cancellation and `LibraryRead + LocalFileWrite`; status
  text never reports incomplete/failed exports as success; save-dialog name sanitising; viewer
  and export palettes agree. Native unit test `the_native_host_connects_pdf_save` (desktop-bootstrap lane).

## Real PDFium (pinned chromium/8125, Linux x64)

```sh
python3 scripts/pdfium/fetch.py --platform linux-x64 target/pdfium-linux
export CM_PDFIUM_LIBRARY="$PWD/target/pdfium-linux/lib/libpdfium.so"
cargo test -p cm-pdf --features runtime --locked --test metadata_runtime --test runtime --test extract_runtime --test export_runtime
cargo test -p cm-daemon --features pdfium-runtime-tests --locked --test fulltext_runtime --test pdf_export_runtime
```

All 6 passed. `export_runtime` (cm-pdf): corpus pages 0–3 (/Rotate 0/90/180/270, CropBox
[20,30,380,470]) with two-line highlights from PDFium glyph boxes, Unicode comments
(`検証コメント αβ café 頁n`), four palette colours, page-level notes (two stacked on page 0) and an
anchored note shifted inside the crop → `Export` → file → `Annotations` re-read → `verify`: 4
highlights + 6 notes, max position error 1.4e-5 pt; a deliberately wrong expectation fails.
PDFium renders the persisted appearances at the expected display positions on all rotations
(highlight blend and note icon pixel coverage). A hand-written `/Rotate 90` source with an
**AP-less blue Highlight** and a **Square with its own AP** keeps both unchanged (no `/AP`
added, no recolouring) with the new annotations appended; exporting an export keeps its 10
annotations. Encrypted → `PasswordRequiredOrIncorrect`, malformed → `Malformed`, invalid page,
off-page note and pre-cancelled requests fail. Original SHA-256 unchanged.

`pdf_export_runtime` (daemon, real worker/SQLite/storage): 4 highlights + 4 notes + 1 unresolved
on the stored corpus → `Unexportable` (nothing written) → with `allow_omissions` a verified copy
(report lists the omission, `complete() == false`, original SHA-256 reported and unchanged); the
copy re-read by `/NM` has every DB annotation with its Unicode text; existing destination refused,
then replaced only with `replace_existing`; the copy attached as a new source keeps its 8
annotations and gains one; encrypted source → `Encrypted`, nothing written; cancelling a running
export yields either `Cancelled` with no file or a complete file; no temporary files remain;
stored blobs byte-identical.

Observed export structure (pypdf dump of `export.pdf`): `/Highlight` with `/C [1 1 0]`,
`/CA 0.5019608` (128/255), Unicode `/Contents`, `/NM`, `/F 4`, 8-value-per-line `/QuadPoints`,
`/AP /N` → FlateDecode Form XObject, `/BBox` = `/Rect`, `/Resources /ExtGState /GS`
(`/CA /ca 0.5019608 /BM /Normal`), stream `q /GS gs 1 1 0 rg x y m … l h f … Q`; `/Text` notes with
their own icon form.

## Independent reader (no PDFium)

```sh
target/pdfium-tools/bin/python scripts/pdfium/validate_export.py target/pdf-export-test-<pid>
```

Passed with pypdf 6.1.1 and poppler 24.02.0: export 10 annotations added (max error 1.4e-5 pt),
legacy 2 added + 2 preserved (dictionaries identical; AP-less highlight still AP-less), source hash
unchanged. Poppler raster at 72 dpi `-cropbox`: highlight line coverage 0.58–0.68 on 0/90/180/270,
note icons 0.86–0.90. Negative controls (quad +1 pt, note rect +30 pt in `expected.json`) fail.
Observed difference: poppler draws Text icons upright (NoRotate) pinned at the rect's unrotated
upper-left corner; PDFium rotates them with the page (see ADR). The same script also passed with
the system pypdf 6.17.0.

## Not verified here

- macOS PDFKit gate (`scripts/pdfium/run.py` / `verify.py`, updated to inspect the Rust export
  directly): needs macOS; not run. CI `pdfium-runtime` job is its evidence.
- The new CI step (apt `poppler-utils` + venv + `validate_export.py`) was reproduced locally with
  the same requirements, not on GitHub Actions.
- GUI (Xvfb, [issue-20](issue-20/README.md)): the "Save PDF…" toolbar button and the enabled
  palette command were exercised; the Linux save dialog is the XDG portal and this container has
  none, so choosing a file and a completed save *through the GUI* were not performed.
- Signed PDFs, password-protected export, non-Highlight/Square/Text existing annotation subtypes.
