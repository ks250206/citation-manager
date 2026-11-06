# #33 execution evidence

2026-11-06, T3-prepared isolated worktree `t3-4c3653a2`, branch
`t3/atomic-import-service`, base `42345ccf7f80c11d3b8a89e8ad1db5e0cafa881b`.
Only synthetic data/private temporary libraries were opened. The root checkout, other
worktrees, actual user library/settings/profile and agent configurations were not changed.

## Implemented and exercised

- Real `ImportCoordinator<B>` with framework-free neutral full-bibliography/source/row
  diagnostic contracts and replaceable backend port; same implementations reexported by
  metadata. Existing spy and raw-request compile-fail tests remain.
- Shared DOI normalization (resolver UTF-8 suffix percent decode once vs raw DOI literals),
  Unicode contributor whitespace/order, calendar precision/leap years, page ranges,
  exact/fuzzy dedup, retained/generated/suffixed citation keys and explicit input limits.
- Retained destination and immutable service-side preview; returned-copy tampering cannot
  alter saved values. Library/service handle isolation, expiry/capacity, cancellation,
  consume-on-success and repeated commit/cancel behavior.
- Real SQLite in-memory AND disk transactions. Separate real connections change metadata,
  external IDs, DOI, key and destination between preview/commit, including A→B→A and a
  selected merge target. Stale commits fail before paper writes.
- Real SQL triggers inject failure in the second paper write, contributor relationship,
  and destination relationship. Earlier papers, people, external IDs and memberships all
  roll back. A mixed merge+create transaction also rolls back the earlier merge. A new
  preview after removing the injected failure retries successfully.
- Exact duplicates within a batch, including choosing one create and one skip, trash
  protection, DOI/identifier disagreement, citation-key collisions and similar Unicode
  candidates. Explicit merge fields reject unknown/duplicate selections and target indices;
  missing/repeated/out-of-range decisions and repeated merge targets are rejected.
- A real #9 attachment ingestion with synthetic bytes precedes a metadata merge. The
  regression confirms original source/blob SHA-256, attachment IDs, contributor/person ID,
  collection membership and all unselected full bibliography/external IDs survive, including
  reopen. The bytes are a synthetic attachment fixture, NOT proof of PDF parsing.
- Dry-run/preview/cancel leave source, database file and WAL byte hashes unchanged.
- PDF candidate precedence/provisional/unresolved and attachment-only policy plus folder
  exclusion/duplicates/planning progress/cancel/retry/partial-result transitions use mocks
  only. No directory traversal, XMP parser or filesystem job runner is claimed.

## Completed checks

All exited0 on the final code:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/check_architecture.py
python3 scripts/check_migrations.py --base 42345ccf7f80c11d3b8a89e8ad1db5e0cafa881b
python3 -m unittest discover -s scripts -p 'test_*.py' -v
git diff --check
```

Workspace:98 unit/integration tests +1 compile-fail doctest, including10 new pure/core and17
new genuine SQLite integration tests relative to base `42345cc`. Python:18 tests. Architecture:12 crates, existing
allowed dependency direction/platform isolation. Migrations: existing2 SQL/checksums unchanged;
no new migration or dependency version/checksum change was needed. Core now uses the
already-pinned uuid with its explicit v7 feature; Cargo.lock adds only that dependency edge. Local default-feature build is
compilation evidence; SQLite integration tests above are actual runtime evidence.

`find-docs` resolved rusqlite via Context7, then queried `/rusqlite/rusqlite/v0.41.1`.
The version-specific result was primarily historical changelog; the pinned local0.41.1
`src/transaction.rs` confirmed IMMEDIATE/commit/rollback-on-drop and `src/lib.rs` confirmed
`total_changes`. Official [SQLite data_version docs](https://www.sqlite.org/pragma.html#pragma_data_version)
confirm same-connection comparison and other-connection commit invalidation. Installed gh2.103.3
`pr create --help` confirmed explicit head/base/body-file behavior before PR creation.

## Limits and handoff

See [ADR](../ADR/import-service.md) for precise input/inventory/candidate/state budgets,
connection-wide conservative revisions, unsupported durable replay, and the PDF/folder policy.
A failed write can advance local `total_changes` despite rollback: cancel/repreview/review
before retry. An unrelated concurrent database write may also invalidate a preview.

No BibTeX/RIS/HTTP DOI adapter, real PDF parsing/folder ingestion, GUI, IPC/grant enforcement,
packaging, native UI/PDFium runtime, code signing or notarization was implemented/executed here.
The #34 packaging work was not edited. The worker's existing upgrade/backup gate remains intact.
Different-model independent read-only review and merge belong to root for this exact PR head;
this implementation agent did not run reviewers, merge, watch or poll CI. CI is inspected once
at PR handoff, with its observed non-final/failure state reported honestly.

## Review corrections and legacy P1 repair

The first independent review found one P1 (legacy resolver DOI compatibility), one P2
(preview ID reuse after real process restart), and three P3s. Published `adb2207` fixed P2
with UUIDv7 handles and a real two-process regression, plus P3 arXiv wrappers, e-locators
and supplied citation keys belonging to the single exact match. That complete published
commit was fast-forward integrated into the T3-prepared `t3-f6f9b87d` worktree's
`t3/legacy-doi-compat` branch before the P1 repair. Other worktrees/root stayed read-only.
No delegate or CLI reviewer was run by this implementation agent.

P1 now has a persisted-record-only compatibility validator, all-DOI matching/disagreement
and final-state checks, and a transaction-held manual/import DOI equivalence check. An
unchanged stored DOI plus a retained raw URL exactly reproducing old normalization is
required; strict new input and explicit incompatible edits cannot claim that exception.
No schema/dependency/pin/lock changes were needed for this repair. The compatibility policy
and ambiguity/collision limitations are explicit in the ADR.

The actual base `42345cc` workspace was extracted with `git archive` into private scratch,
compiled with `cargo run --locked --offline`, and its real old `PaperLibrary::create` API
created six disk records. API output confirms the reported `10.1002/abc%3c493%3e` plus raw
resolver URL, query/fragment, double encoding, malformed hex, invalid UTF-8 and decoded
whitespace cases. The initial generator invocation rejected a noncanonical macOS `/var`
path via the existing path guard; rerunning with the canonical `/private/var` path succeeded.
No path protection was weakened. The committed SQL fixture is the resulting INSERT data;
[fixture provenance/reproduction](../../crates/storage/tests/fixtures/legacy_doi/README.md)
includes the archived-API source. Seven new SQLite regressions also passed using copies
of that **whole actual old-API-created disk database**, via `CM_TEST_OLD_API_DATABASE`.

New coverage: reopened legacy title-only edits, unchanged DB/WAL hashes for preview and
rejected Create, exact duplicate detection, approved title/external-ID merges preserving
unselected metadata/raw IDs/DOI and real synthetic #9 attachments through reopen; retained
self-owned citation key; manual create/save/update and trash duplicate protection; true DOI
disagreement and forged new-input legacy exception rejection; raw percent versus decoded
identity separation; all existing colliding matches with Create/Merge/manual-update Conflict;
invalid legacy URL preservation with strict new-input rejection. A new core regression
checks every bibliography/external DOI in both directions, rather than only the first one.

The full validation commands above passed for this repair:98 workspace unit/integration
tests +1 compile-fail doctest,18 Python tests, fmt, warnings-denied locked workspace
clippy/build,12-crate architecture, immutable migrations against `42345cc`, and whitespace.
Independent re-review of the repaired exact head and CI/merge
follow-up remain with root; this implementation result is not a review approval.

UUID API evidence: Context7 resolved `/uuid-rs/uuid`, then official source for now_v7
confirmed the v7/std feature and timestamp+random generation. The exact pinned1.29.0 API
also compiled in an isolated core check and in the separate-process regression.
For P1, find-docs resolved rusqlite and queried `/rusqlite/rusqlite/v0.41.1`; its historical
changelog response was supplemented with the pinned primary-source `transaction.rs`
(IMMEDIATE mapping and rollback-on-drop) and `statement.rs` (`query`/row iteration).
