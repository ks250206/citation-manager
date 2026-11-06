# Native acceptance after wake — 2026-11-07

**The native GUI works. Acceptance is still incomplete; keep PR45 Draft and #15 open.**
The historical `cgWindowNotFound` observations are retained as historical evidence,
not the current blocker. This run used the T3-prepared `t3-425df78a` checkout, normal
merges of PR45 head `8cd9185` and main `9e7e66e` (including #46/#47/#50), and a fresh
private synthetic library. No DB preseed or headless import supplied its GUI records.

## Artifact and observation method

- Own private root: `/private/tmp/cm-pr45-native-425-rz9fto1b`.
- Real arm64 Mach-O executable in `Citation Manager Native Acceptance.app/Contents/MacOS/citation-manager`.
- Bundled `Contents/Frameworks/libpdfium.dylib` SHA-256:
  `8d1e46a6dbe32d7cb95279ab3def2e71e557a3d4cc49162453afa3e3b250dc5e`.
- Final corrected executable SHA-256:
  `0384dfce141a0535138344ca89b8b21f4eb942acf28aec414b7646f633563708`.
- The private `manifest.json` records the build's complete tracked source hashes,
  input hashes, explicit library/settings paths and earlier executable hashes.
  `native-awake-artifacts.json` records final source comparison and evidence hashes.
- All UI input and screenshots used documented native `mcp__cua_repl` APIs. GPUI's
  AX tree exposes the window/menu, so internal controls used screenshot coordinates;
  native pickers used fresh AX state. Harmless Notes-tab calibration established that
  the 2640×1784 screenshot coordinates, rather than logical coordinates, target controls.
  CLI was used only for build/start/owned-process cleanup and filesystem/read-only DB work.
- Earlier batches used the import-row fix before main #50 and the later tree, Trash
  status and progress-total fixes. Main #50 changes test hooks/docs only. The final
  corrected binary retested affected controls and exercised PDF/settings/DOI paths.
  Earlier valid import/filter/export observations are not described as a fresh final-binary run.

## Actual UI results

| Action and observed result | Private screenshot numbers |
|---|---|
| Import `thousand-diverse.bib`; ten separately inspected previews, ten explicit Create decisions and ten separate commits, each 100. Final counter 1000/1000. Read-only DB: 1000 unique synthetic DOIs. | 09–49 |
| Initial near-identical-title corpus: first 100 committed; next preview reported bounded duplicate-match error and preserved the first batch. A new, genuinely empty library received the diverse corpus above. | 02–08 |
| Three panes; parent/child collections, two tags, one paper in both collections and both tags; AND→1 row, OR→2; Cmd-click→2 selected. Combined collection/tag/keyword/author/year/type→1; PDF filter→empty; invalid year→visible error with prior rows retained. Title sort and real forward/back paging; final-binary Year asc/desc and ID/created/updated desc matched read-only order. | 19, 40, 50–67, 105–109 |
| Native HOME-starting Save picker: All→RIS 1000, selected→BibTeX 2, direct Child collection→RIS 1. Production parsers read all outputs with zero diagnostics. | 56–59, export-readback.txt |
| Dirty metadata blocked New, another selection, Trash and DOI replacement; explicit Discard restored saved values. Manual preview/Create/commit saved one record. Error RIS required Skip for the invalid row and saved its valid neighbor. Exact DOI duplicate required Merge; only Title was selected. | 63–75 |
| Final binary: Trash/restore no longer replaced success with a false membership error; manual and PDF previews show 0/1 rather than 0/0. | 76–78 |
| Actual PDF picker loaded bundled PDFium's Info/XMP suggestions and conflicting Info DOI. Edited PDF form survived Library selection and Cmd+N; Cmd+S did not create a manual-only paper. PDF review paused bibliography at 500/1000; Cancel retained the paused file; explicit Resume required a fresh row501 preview/decisions. | 28–36 |
| RegisterNew metadata PDF: reviewed title/DOI then explicit Create/commit. A private regular-file obstruction at the attachment directory caused a real storage error after paper commit. Report retained paper ID `01a1b310-22d0-7590-b2a4-dd06ea152ae9`; Reload/Get preserved failure. Removing only that owned obstruction and clicking Retry attached to that same ID. | 78–81 |
| Actual AttachExisting picker attached `info.pdf` to the retained paper, with bibliography unchanged. Later native metadata correction saved the title and kept both attachment slots Present. | 82, 98–99 |
| Native folder picker: nonrecursive 4 versus recursive 5 entries; duplicate SHA skipped, symlink and non-PDF excluded, nested scanned PDF assessed. Malformed and encrypted PDFs displayed actual parser errors and editable filename/manual fallback. | 83–87 |
| 500 unique synthetic PDFs: inspection progress visible; Cancel stopped after 20 assessments; explicit Retry completed 500. Attachment batch cancellation preserved 9 actual saves; a second explicit Attach operation completed the remaining files. Read-only DB:502 total attachments, all source/blob digests equal. | 95–104 |
| Column visibility/width and pane widths survived real Cmd+Q/relaunch. PDF tab and reading mode also persisted. Reset restored defaults. Malformed private JSON on launch displayed a default-layout warning; native Reset wrote valid settings. | 52, 88–91 |
| Actual public DOI lookup `10.1038/nphys1170`: loading state, Cancel retained manual draft; a second lookup reached normalized review with provider metadata and no automatic commit. Read-only DB confirmed no paper for this DOI. | 92–94 |

The retained manual-form-after-preview limitation remains: after successful manual
creation, explicitly discard the retained dirty form before selecting the saved record.
No automatic retry/Create is performed. No viewer or annotation/notes editor is claimed.

## Corrections found through the actual window

1. Long import titles wrapped and hid decisions. Put decision text first and use the
   pinned GPUI single-line truncation style; full metadata remains in the right pane.
2. Leading spaces did not indent collection names. Use actual per-depth padding.
3. Trash queried active-only membership and displayed an expected NotFound as failure.
   Do not issue membership reads for deleted papers; Restore refreshes active membership.
4. Manual/PDF progress used the file counter's zero total. Non-file preview totals now
   count that preview; existing state regressions assert both this and paused-file totals.

## Checks and remaining gates

Final corrected Rust source passed `cargo fmt --all -- --check`, locked all-target
workspace Clippy (default and native feature), locked workspace tests **200** including
the compile-fail doctest, native desktop tests **19**, workspace and native executable
builds, architecture/migration checks, Python validators **23**, and `git diff --check`.
The earlier bundled-PDFium runtime lane passed 8 tests; its PDF source did not change.
Commands/results are in `native-awake-checks.json`; full private logs are retained.
Published text logs omit trailing blank lines; private originals remain intact.
All 636 dependency identities/checksums and both immutable SQL migrations/manifest remain
unchanged against refreshed main; only existing local workspace edges differ in the lock.

Still **unverified in native UI**, so this is not completion of #15:

- Genuine ExternalPaths drag/drop for both AttachExisting and RegisterNew. Picker success
  and worker tests do not prove drag delivery. No fabricated drop/event injection was used.
- Actual offline DOI fallback. Two process-local sandbox network-denial launches failed
  to become available to CUA and were stopped (owned processes only); these are failed
  setup attempts, not offline proof. No global network/system setting was changed.
- Simultaneous DOI/PDF cancellation isolation in both directions; unrelated Attach/Get
  delivery while a non-PDF preview/dirty form is active; repeated slot-release stress.
  Existing deterministic state/worker tests remain separate evidence.
- Native failed forward/back query state and same-direction retry. Invalid filter input
  is not a failed page query; no DB mutation/fault injection is used to manufacture one.
- Collection edit/move/delete and oversize paths. Only specifically observed actions
  above count as UI evidence.

#34 remains **HELD**; this private test bundle does not satisfy its product bundle,
browser/lifetime or signing gates. #23/#25 remain closed. Root owns independent review
and final-head CI. No children/reviewer CLIs/merge/watch were started by this operator.


Selected actual screenshots (original CUA JPEG bytes):

- [1000 processed after ten approvals](native-awake-screenshots/49-thousand-complete.jpg)
- [OR tags and two selected rows](native-awake-screenshots/55-tags-or-two-multiselect.jpg)
- [Final Trash status correction](native-awake-screenshots/76-final-trash-no-false-membership-error.jpg)
- [Retained paper after attachment failure](native-awake-screenshots/79-retained-paper-attachment-failure.jpg)
- [Invalid settings warning](native-awake-screenshots/91-invalid-json-fallback.jpg)
- [Cancellation preserving nine attachments](native-awake-screenshots/101-partial-attachments-cancelled.jpg)

The final normal native process exited0 after Cmd+Q. Both network-restricted setup
attempts were terminated143 and are excluded from normal-exit/offline success claims.
All 14 original input digests and all 502 attachment source/blob pairs were rechecked
unchanged; the complete private screenshot manifest is retained.
