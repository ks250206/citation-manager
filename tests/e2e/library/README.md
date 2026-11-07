# Native acceptance checklist (not an automated mock/browser test)

Required operator: native CUA, with an explicitly identified human operator for
cross-window gestures the automation cannot reliably deliver, against the actual GPUI executable, using
only a fresh private temp library/settings. No preseeded UI data. #34 remains held.
The awake run obtained the actual native window and exercised many paths below.
See [current per-path results and incomplete gates](../../../docs/validation/issue-15/native-awake.md).
The [isolated continuation](../../../docs/validation/issue-15/native-continuation.md)
adds collection/oversize/dirty-guard/Attach-Get/repeated-Resume proof, with all new action
and screenshot ranges mapped to their exact initial/integrated executables. The assisted
D&D follow-up below covers both intents on the PDF files / folder screen; the duplicate
Details/PDF-pane zones remain untested. Offline DOI and failed-page robustness remain
unverified; this checklist is not fully passed.
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
6. Use actual pickers and drag/drop for both explicit Attach/New intents. Record the
   surface tested; duplicate zones in the Details/PDF pane require separate coverage. Test generated
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

Ownership follow-up: [actual corrected native run and queued-ticket regressions](../../../docs/validation/issue-15/doi-pdf-ownership.md).
The original Import-review-tab overlap was reachable and is now fixed: beginning an
Editor/Import/PDF ownership request or navigating to Import review invalidates pending
DOI. Native pending-lookup navigation preserved and committed the PDF review; PDF-working
Library view disabled DOI controls. This does not claim actual queued late-arrival
observation or freely overlapping operations. Separate-token tests remain automated.
Offline PDF filename/manual registration is local-only; offline DOI and real failed-page
retry are additional robustness checks and remain unverified. Both D&D intents are literal #15 requirements; the later assisted run below now
covers them on the dedicated PDF files / folder screen. Other pending checks remain.

Assisted native D&D follow-up: [both explicit intents and subsequent persistence/edit
checks](../../../docs/validation/issue-15/native-dnd.md) now passed using the user's
actual Finder gestures and root CUA observation/review/save/relaunch. Earlier D&D-open
notes are historical. This is not a CUA drag API success claim or a complete pass of
the additional offline DOI/failed-page/native-late-arrival checklist.


Latest-runtime bulk follow-up: [fresh native 1000-record workflow](../../../docs/validation/issue-15/native-bulk.md)
repeats the complete import → classify → metadata search → re-export sequence on the
final unchanged runtime, with interaction observed while previews run. It supersedes
only the earlier-binary gap for that sequence. The final report explicitly accepts the
local PDF offline path by code plus observed fallback/editing, without claiming a
network-disabled GUI experiment. Additional robustness and duplicate-zone coverage
above remain unverified. Final acceptance review and exact-head CI remain due.
