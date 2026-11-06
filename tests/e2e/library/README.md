# Native acceptance checklist (not an automated mock/browser test)

Required operator: native CUA against a freshly linked actual GPUI executable, using
only a fresh private temp library/settings. No preseeded UI data. #34 remains held.
The current run could not obtain the native window from CUA; every path below is
**unverified in GUI**. Headless service tests do not substitute for these actions.

1. Build the actual `citation-manager` with `desktop-bootstrap --locked`. Use
   `scripts/prepare_native_smoke.py` without the optional shell launcher; the test app
   must contain the actual executable. Pass canonical absolute `--library` and
   `--settings` paths. Never open a real user library. See ISSUE_15 evidence paths.
2. Import a synthetic UTF-8 1000-row BibTeX file in ten separate <=100 previews.
   Inspect rows/warnings/diffs; choose current decisions and commit each batch. Keep
   the window usable while work runs. Verify the real saved rows after completion.
3. Build parent/child collections and multiple tag memberships, exercise tag AND/OR,
   metadata/author/year/type/PDF combined filters and Title/Year/ID/date sorting.
   Page forward/back, multi-select, trash/restore, empty/error/loading/retry states.
4. Adjust column visibility/width/sort, pane widths/tab/reading mode, quit fully and
   relaunch with the same private paths. Test malformed private settings and reset.
5. Manually preview/create, DOI lookup/cancel/offline fallback, duplicate merge with
   selected fields, explicit error-row Skip, rejected-file diagnostics, interrupted
   later batch and export All/selected/direct collection in BibTeX/RIS. Reparse output.
6. Use actual pickers and drag/drop into each explicit Attach/New zone. Test generated
   fixtures/library PDFs plus licensed fixtures/pdf encrypted/malformed and copies for
   duplicate hashes. Exercise explicit recursive/nonrecursive folder selection,
   non-PDF/symlink/oversize exclusions, progress, cancellation, partial paper success
   with attachment failure and retry against that retained paper. Quit/relaunch,
   select the actual attachment host slot, edit metadata later and verify original bytes.
7. Capture native screenshots and logs after each actual result. Keep DB-only and
   runtime-only evidence labelled separately. Do not claim viewer/notes/command palette,
   product bundle/lifetime/browser/signing gates passed. Quit only the test app and
   preserve evidence paths at handoff.
