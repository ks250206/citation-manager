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

Workspace:87 unit/integration tests +1 compile-fail doctest, including6 new pure/core and10
new genuine SQLite integration tests. Python:18 tests. Architecture:12 crates, existing
allowed dependency direction/platform isolation. Migrations: existing2 SQL/checksums unchanged;
no new migration or dependency/lockfile change was needed. Local default-feature build is
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
