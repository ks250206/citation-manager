# #15 library workbench — validation and blockers

**Current: the native GUI works; acceptance remains incomplete. Keep PR45 Draft and #15 open.**
The 2026-11-07 awake run used a fresh private library, actual Mach-O app, bundled pinned
PDFium, real CUA interactions and ten separately approved 100-row batches. Main through
`9e7e66e` is integrated. Four observed UI defects were corrected. See the
[actual native actions, remaining gates and source/binary evidence](issue-15/native-awake.md),
[artifact manifest](issue-15/native-awake-artifacts.json) and
[final local checks](issue-15/native-awake-checks.json). #34 remains HELD.

## Historical implementation and headless evidence

The observations below predate the successful awake run. Statements that a native path
was unrun describe that historical run; the current matrix is near the end of this file.

Base `fd50af1fe385c30b8581f06c90247630f27caf55`, T3-prepared isolated checkout
`t3-6fa2df7e`, branch `t3/issue-15-library-import`. Native macOS arm64, Rust 1.99.1,
GPUI revision and all 636 locked dependency identities/checksums unchanged.

**Historical window-observation failure, now superseded.** Real worker/DB/PDF operations
are implemented; native CUA could not locate the application window. It returned
`Computer Use server error -10005: cgWindowNotFound` for two fresh isolated test bundle
identities. Native startup diagnostics reached GPUI launch/window creation/activation,
but no actual window screenshot or GUI operation was observed. The cause is unresolved.
[Exact observations and private paths](issue-15/native-observations.txt) distinguish these
logs from visible UI evidence. Earlier #10 screenshots do not verify the new implementation.

The controlled follow-up built the actual baseline `fd50af1` in T3-prepared checkout
`t3-6bedd541` before fast-forwarding to published `0afdee4`. Both independently copied
real-executable test apps, each with fresh private data, returned `cgWindowNotFound`
through both app-path and bundle-ID CUA lookup; CUA inventory reported each running.
This is a native window-observation blocker shared with the control, not evidence of a
#15-specific regression or proof that CUA alone is faulty. Temporary application logs
reached render, a scheduled next-frame callback and worker replies, but do not establish
visibility, native input or screenshots. Instrumentation was removed. See
[comparison and follow-up checks](issue-15/follow-up-checks.json).

## Independent-review corrections

The [review of original `0afdee4`](https://github.com/ks250206/citation-manager/pull/45#issuecomment-6693421153)
reported no P0/P1 and three P2 findings. This follow-up addresses their code paths:

- PDF registration owns separate form entities and a draft bound to row index, source
  path and SHA-256. Library selection and Cmd+N cannot supply PDF metadata. A mismatched
  origin is rejected; matching origins retain edits. Cmd+S saves only on the Library
  screen; PDF registration uses its explicit preview/commit controls.
- Replacing a preview first submits coordinator Cancel. Failed cancellation submission
  retains the original handle/decisions. PDF/manual review pauses the bibliography
  counters, and explicit Resume requires a fresh preview/decisions. An unrelated PDF
  report preserves a non-PDF preview. Choosing a new file explicitly reports the prior
  stopped progress; prior commits remain. Parsed file diagnostics are restored on resume.
- DOI and PDF own independent real cancellation tokens. Idle PDF Cancel is guarded.
  Leaving for the PDF screen invalidates the earlier DOI response generation. Attach,
  retry and commit cannot replace the DOI cancellation token.
- Adjacent P3-1: Get replies use a separate load event and no longer display “Saved
  locally” or erase the PDF partial-failure summary.

Root integrated the follow-up `17a0e8e` with the reserved paging change `5d41f6d`,
then addressed the remaining P3 code paths:

- Next/previous page requests retain the current rows, cursor and history until success.
  The native query error handler releases pending navigation, so the same direction can
  be retried. Two model regressions cover failed forward/back moves, successful retries,
  reload and stale success/error replies. These do not exercise native interactions.
  Reload clears old rows instead of displaying them under reset filters/cursors.
- A persistent unsaved banner and explicit Discard metadata edits action preserve the
  library form across screens. New/selection/DOI replacement/trash/restore require saved
  or explicitly discarded edits. Background Get retains dirty fields instead of replacing
  them, even after an independently confirmed import/PDF commit. This also means that a
  dirty manual form can remain after its record is saved through preview/commit; select
  the saved record after discarding the retained form. No automatic retry or second Create
  is performed. Native confirmation of these paths remains unrun.
- Export Save picker starts in absolute HOME, not `/private/tmp`; absent/relative HOME
  shows an error instead of falling back to a shared directory. The exact pinned GPUI
  `App::prompt_for_new_path` and macOS NSSavePanel adapter were read to verify that this
  argument selects only the starting directory. Actual picker operation remains unrun.

New tests are **state/worker evidence**, not native acceptance: four pure-state tests
exercise PDF origin changes, retained handles/batch counters, failed cancellation
submission and both cancellation directions with real tokens. A real worker regression
commits the first batch, alternates twelve PDF/manual previews without exhausting the
four coordinator slots, resumes the exact next file row with fresh decisions, and
verifies exactly three real saved records.

The [independent review of integrated `b6c9ba9`](https://github.com/ks250206/citation-manager/pull/45#issuecomment-6694366929)
confirmed the original P2×3/P3×4 fixes in the native handlers, with no P0–P2. The reviewer
ran import-flow state 4, workbench state 5 and feature-enabled mixed worker 1, and verified
all 268 tracked hashes unchanged. Two optional P3 follow-ups were then addressed:
PDF reports now refresh form/list/retained-paper and report counts even if releasing a
preview cannot be submitted; that release error is appended and the retained handle
still blocks replacement until release succeeds. Cancelling a preview explicitly says
when bibliography remains paused and resumable. PDF forms also state that selecting
another row resets unsubmitted edits. These final small changes require a narrow
independent recheck and final-head CI; no additional native GUI acceptance is implied.
The [small follow-up checks](issue-15/minor-follow-up-checks.json) record final-source
fmt, workspace native Clippy, desktop focused 4+5, mixed worker 1 and native executable
build success. Full/thousand-record/PDF/Python suites were not repeated for these small
handler/status-only changes; final CI and the separate GUI gate remain required.

## Executed evidence

- `crates/daemon/tests/library_workbench.rs` uses actual temporary SQLite libraries,
  retained #33 ImportCoordinator, #12 collection/tag/query services, #13 parsers/export,
  #9 atomic shared-blob ingestion and real filesystem failures. It is headless evidence.
- 1000 synthetic BibTeX records: ten explicit previews/commits of 100, each preview checked
  for no writes. Classification into real collection/tag, combined metadata query/sort,
  1000-record direct-collection RIS export/reparse, soft-delete/restore, settings save,
  DB/client reopen and true cursor paging. The focused worker suite took about126s;
  this is not a native responsiveness measurement or an automatic GUI approval path.
- File diagnostics (including rejected syntax) AND row errors; Create fails for error
  rows, explicit Skip permits the valid neighbor. Cancel consumes preview without writes,
  intervening real DB changes cause Conflict, reviewed selected-title merge succeeds.
- Offline filename proposal, folder non-PDF/symlink/hash-duplicate reports, pre-cancel,
  actual paper commit followed by attachment filesystem failure, retained paper ID on
  reinspection, attachment-only retry and no second paper. A real obstruction is removed
  only inside the test's private temporary directory; product code performs no such repair.
- Explicit attach does not create/modify bibliography. Cancellation after one actual
  attachment preserves partial success; remaining attachments can be completed. Reopen
  verifies real references/hash statuses and later metadata correction.
- Invalid settings return validated defaults with a warning. A symlink settings target
  is not overwritten; its unrelated sentinel survives.
- Framework-free state tests reject stale query results, require every import decision,
  disallow Create for error rows, and preserve unedited absent abstract/contributor identity.
  Existing #10 dropped-callback/editor-generation tests remain; actual new native callback/
  picker/render behavior still needs GUI acceptance.
- Real PDFium 158.0.8125.0, original SHA-256
  `8d1e46a6dbe32d7cb95279ab3def2e71e557a3d4cc49162453afa3e3b250dc5e`, loaded by the
  existing serial worker from the explicitly pinned development test path. Metadata DTO
  digest/page count, real Info dictionary, bounded XMP suggestions/competing Info DOI,
  image-only missing metadata, encrypted/malformed errors, pre-cancel and recovery pass.
  The existing full runtime corpus also passes. This is ABI/runtime evidence, not product
  .app loading, viewer, CUA or signing evidence. Test-only CM_PDFIUM_LIBRARY is not read by
  the product worker; missing bundle runtime falls back honestly to manual/filename review.
- Original CC0 PDFs are reproducible with stdlib Python; fixture hashes/regeneration checked.
  Existing licensed/generated fixtures are retained unchanged.

## Required checks

Original implementation commands, results and source/binary hashes are in
[checks.json](issue-15/checks.json). Follow-up results, final binary/source hashes and
the baseline comparison are in [follow-up-checks.json](issue-15/follow-up-checks.json);
supporting logs are retained separately and identified there.
Root integration ran the unfiltered workspace suite (171 including the compile-fail
doctest and all thousand-record tests), native-feature desktop suite (19, overlapping
workspace tests), workspace/native executable builds, final all-target workspace native
Clippy, formatting and validators (19 Python tests). The last reload-clear refinement
was applied after the full suite had compiled desktop; final focused paging tests (5)
and the native desktop suite cover that refinement. Final-source full CI remains due.
See [root integration checks and binary/source hashes](issue-15/root-integration-checks.json).
This final root executable was built, not launched in CUA again; the preceding controlled
baseline/follow-up observation failures remain the blocker, not a new observation claim.
Workspace tests, native-feature tests and PDF runtime tests are separate lanes. CI and
independent different-model review belong to root and are not implied by local success.

## CI watchdog follow-up

At `3ad121e`, contracts and PDFium runtime passed, but native-dependencies failed in
the unoptimized 1000-record worker test: the test's 30-second callback watchdog expired
([failed job](https://github.com/ks250206/citation-manager/actions/runs/42286082857/job/128127300646)).
The old helper did not identify the operation or batch; it does not prove whether that
callback would eventually arrive. The worker test had run for 236.65 seconds, while the
separate BibTeX/RIS 1000-record round-trip tests had already passed in 246.01 seconds.

The test-only follow-up keeps ordinary requests on 30 seconds and gives just this large
test's 100-row preview/commit requests a finite 120-second watchdog. It logs batch/stage
and elapsed time, and distinguishes Timeout from Disconnected on failure. All 1000
records, ten explicit previews/commits, no-write assertions and classification/query/
export/reopen checks are retained. No product timeout, preview TTL, batch/resource cap,
workflow lane, dependency or fixture is changed. This is functional debug-build coverage;
it makes no release-build latency or native responsiveness claim. #30 performance work
and the independent native GUI gate remain outstanding. The final head must pass CI.
The feature-enabled `library_workbench` binary then passed all five tests locally in
121.74 seconds, including all ten reviewed 100-row batches; maximum observed preview
and commit waits were 15.37 and 7.93 seconds in this unoptimized local run. These local
measurements do not establish CI runner timing or release performance. Formatting and
all-target native daemon Clippy passed. [Exact stage timings and failure context](issue-15/ci-watchdog-checks.json).

## Current native acceptance matrix

| Required path | Observed result after wake |
|---|---|
| Actual 1000-record import, ten separate approvals | Passed in actual GUI; exactly 1000 synthetic DOIs corroborated read-only |
| Classification, combined filters, multiselect, paging, loading/empty/error, trash/restore | Observed; Title/Year/ID/created/updated sorting observed; failed-query paging/retry and collection edit/move/delete remain unverified |
| Manual preview/Create, exact duplicate selected-title Merge, error-row Skip | Observed; invalid candidate could not Create |
| DOI lookup/cancel | Actual loading/cancel and public provider response into uncommitted review observed; no DOI paper saved; offline fallback remains unverified |
| Native Open/Save pickers, exports/readback | PDF Attach/New pickers, All RIS 1000 / selected BibTeX 2 / direct collection RIS 1 passed; real D&D remains unverified |
| PDF folder/metadata/fallback/progress/cancel/retry/partial success | Recursive5/nonrecursive 4, digest duplicate, symlink/non-PDF, malformed/encrypted fallback forms; cancel20/500 then retry500; attach cancel retained 9, explicit resume completed 500; retained-new-paper failure/retry observed |
| Layout/settings and attached-paper correction | Actual quit/relaunch, tabs/reading/columns/panes, invalidJSON warning/reset and later metadata correction observed;502 managed blobs match original source hashes |
| Review regressions | PDF form/Library/New/CmdS, bibliography pause/resume/fresh decisions, dirty guards, failure-survives-Get, HOME Save picker observed; simultaneous cancellation and dirty/non-PDF preview plus unrelated Attach/Get remain unverified |

The [native run log](issue-15/native-awake.md) specifies exactly which binary each phase
used and retains the incomplete gates. Earlier headless results above remain headless
results. No viewer, annotations/note editor, saved advanced search or command/keymap
registry is claimed; #52/#53 parallel work is outside PR45. The checklist remains at
[tests/e2e/library](../../tests/e2e/library/README.md).

## Boundaries and limits

[ADR](../ADR/library-workbench.md) specifies ownership and private settings/backup policy,
cooperative cancellation, source/digest checks and bounded in-process partial-file reports.
XMP is an untrusted bounded uncompressed packet hint; compressed XMP/catalog association
is not proven by the scan. Native parser allocation/preemption, quit-during-write unknown
outcomes, post-process-loss attachment retry/replay and cross-process writer ownership
retain the existing limits. #34 remains **HELD**, with its gates closed and no app-layout/browser runner,
VM, signing, native-host or system-profile work. #23/#25 were not started. No agents or
CLI reviewers were created; root owns Opus review, CI monitoring and any future merge.
