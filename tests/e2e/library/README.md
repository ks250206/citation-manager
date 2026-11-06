# Native acceptance checklist (not an automated mock/browser test)

Required operator: native CUA against a freshly linked actual GPUI executable, using
only a fresh private temp library/settings. No preseeded UI data. #34 remains held.
The awake run obtained the actual native window and exercised many paths below.
See [current per-path results and incomplete gates](../../../docs/validation/issue-15/native-awake.md).
The [isolated continuation](../../../docs/validation/issue-15/native-continuation.md)
adds collection/oversize/dirty-guard/Attach-Get/repeated-Resume proof, with all new action
and screenshot ranges mapped to one exact executable. D&D/offline/simultaneous-cancel/
failed-page gates remain open; this checklist is not fully passed.
This is still an acceptance checklist, not a claim that all paths passed. Headless
service tests do not substitute for native actions.

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

Review regressions (consult the current run log before repeating already verified paths): inspect a PDF,
edit its title, visit Library/select a different paper and Cmd+N, then return to the same
PDF row. Its own edits/DOI must survive; Cmd+S in PDF must not create a manual-only paper.
Open a file batch preview after earlier commits, switch repeatedly between PDF/manual
reviews, and explicitly Resume bibliography: counters and the next row must agree, while
fresh decisions are required and no preview-slot exhaustion occurs. An unrelated Attach
report must retain the non-PDF preview. Cancel DOI while PDF work runs and cancel PDF
while a DOI lookup runs; each token must affect only its own operation, and idle PDF
Cancel must do nothing. Verify that a partial attachment failure summary survives the
subsequent paper Get reply.

Also verify that failed forward/back queries keep the visible page number, rows and
cursor and allow the same move to be retried. Edit library metadata, then try New,
paper selection, DOI replacement and trash/restore: the form must remain until saved
or explicitly discarded. Repeat while a PDF report/Get arrives. Discard must restore
original form values without deleting saved records. Confirm that the unsaved banner
survives unrelated operation messages, and the export Save picker starts at HOME.
The current run log distinguishes observed guards/HOME picker from outstanding native
failed-page and simultaneous-operation cases.
