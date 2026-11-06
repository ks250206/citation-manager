# #15 library workbench — validation and blockers

Base `fd50af1fe385c30b8581f06c90247630f27caf55`, T3-prepared isolated checkout
`t3-6fa2df7e`, branch `t3/issue-15-library-import`. Native macOS arm64, Rust 1.99.1,
GPUI revision and all 636 locked dependency identities/checksums unchanged.

**Acceptance is not complete. This is a draft handoff.** Real worker/DB/PDF operations
are implemented; native CUA could not locate the application window. It returned
`Computer Use server error -10005: cgWindowNotFound` for two fresh isolated test bundle
identities. Native startup diagnostics reached GPUI launch/window creation/activation,
but no actual window screenshot or GUI operation was observed. The cause is unresolved.
[Exact observations and private paths](issue-15/native-observations.txt) distinguish these
logs from visible UI evidence. Earlier #10 screenshots do not verify the new implementation.

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

Final executed commands, results and source/binary hashes are in
[checks.json](issue-15/checks.json); supporting command logs are retained in the same directory.
Workspace tests, native-feature tests and PDF runtime tests are separate lanes. CI and
independent different-model review belong to root and are not implied by local success.

## Native acceptance still unrun

| Required path | Current result |
|---|---|
| Actual 1000-record UI import in ten approvals → classification → metadata search → export | **blocked/unrun**; headless service path passed |
| Real native picker and ExternalPaths D&D for both attach/new intents | **blocked/unrun** |
| Folder progress/cancel/retry, duplicate hash, scanned/corrupt/encrypted/offline fallback and true partial attachment UI | **blocked/unrun**; worker/ABI paths tested separately |
| Manual/DOI preview, read-only lookup/offline fallback/cancel, field merge/Skip diagnostics in the actual window | **blocked/unrun**; deterministic provider/service tests passed, no public-network success claimed |
| Settings persistence across actual Cmd+Q/relaunch, invalid-setting native warning/reset | **blocked/unrun**; worker reopen/default tests passed |
| Virtualized table/paging, combined filters, hierarchy/multiple memberships, trash/restore, loading/empty/error UI | **blocked/unrun**; real service/state paths tested |
| Selected Details/PDF attachment slot, quit/relaunch and later metadata edits via GUI | **blocked/unrun**; DB/reference reopen passed |

Follow [the native checklist](../../tests/e2e/library/README.md) after a native window can
be obtained. No GUI acceptance checkbox is marked passed. No fulltext, saved advanced
search, viewer, annotation/note editing or command palette/keymap registry is claimed.

## Boundaries and limits

[ADR](../ADR/library-workbench.md) specifies ownership and private settings/backup policy,
cooperative cancellation, source/digest checks and bounded in-process partial-file reports.
XMP is an untrusted bounded uncompressed packet hint; compressed XMP/catalog association
is not proven by the scan. Native parser allocation/preemption, quit-during-write unknown
outcomes, post-process-loss attachment retry/replay and cross-process writer ownership
retain the existing limits. #34 remains **HELD**, with its gates closed and no app-layout/browser runner,
VM, signing, native-host or system-profile work. #23/#25 were not started. No agents or
CLI reviewers were created; root owns Opus review, CI monitoring and any future merge.
