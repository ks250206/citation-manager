# #9 validation and downstream handoff

Environment: macOS, Rust 1.99.1, pinned workspace lockfile. Base:
`08cb717dd26b174bcada3f291b0cd466f39b2ea7` (#8). All filesystem/DB tests use canonical
temporary directories; the user's actual library is never opened. No UI/PDF parsing/network
functionality or applied SQL/checksum changes are included.

## Local checks

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`: 45 unit/integration tests plus 1 compile-fail doctest passed.
  Storage accounts for 35 tests; all existing #8 CRUD/path/migration tests pass.
- `cargo build --workspace --locked`
- `python3 scripts/check_architecture.py`: 12-crate boundaries passed.
- `python3 scripts/check_migrations.py --base origin/main`: both immutable SQL IDs/checksums preserved.
- `python3 -m unittest discover -s scripts -p 'test_*.py' -v`: 13 tests passed.
- `git diff --check`

## Recovery evidence

`crates/storage/src/attachments/tests.rs` exercises actual temporary files/SQLite and:

- streams files larger than the copy buffer; SHA-256 of original and existing managed bytes stays unchanged;
- shares one blob across two papers; failed association and deferred COMMIT constraint failure never delete it;
- injects interruptions during copy, after durable publication, before COMMIT, at real deferred-FK COMMIT,
  and after successful COMMIT/lost response; reopen proves committed and old references survive;
- reports partials/orphans, retries safely, and retains partials for explicit investigation;
- returns typed missing/corrupt status; refuses corrupt existing blob replacement;
- rejects source/managed symlinks, hardlinks, unsafe permissions, directory traps, traversal/relative inputs,
  symlink shards and pairing a DB with another library's root.

`crates/storage/src/backup/mod.rs` unit tests prove atomic no-replace file/directory publication and
failure after staging the restored DB: changing bundle bytes after validation is detected again during copy,
the destination is not published, and the live library/original remain intact.

`crates/storage/tests/recovery.rs` proves:

- real WAL with an open writer/autocheckpoint disabled is included by online backup;
- full bibliography/DOI/extra JSON, collections/tags, shared attachments, full text, resolved and unresolved
  annotations/source/page label/comments survive backup -> fresh restore -> ordinary WAL reopen;
- later metadata/attachment writes and soft delete do not alter an existing snapshot; trash remains restorable;
- malformed/unknown-version/unknown-field/incomplete/duplicate/traversing/absolute manifests are rejected;
- missing/corrupt bytes, link traps, extra files/directories/WAL, unsafe mode, forged schema history,
  mismatched DB references and invalid foreign keys are rejected (even when the DB manifest hash is recomputed);
- existing destinations, running libraries, symlinks and unrelated paths survive failed restore;
- baseline is real, validated, private, idempotent and never overwritten;
- a real version-1 WAL library upgrades only after `SqliteMigrationBackup` publishes its pre-migration
  old-schema/old-data snapshot; failed/refused hooks preserve current history/data.

## Reusable APIs and limits

The exact API and consistency/permission requirements are in
[attachments-backup ADR](../ADR/attachments-backup.md).

- #15: `ManagedStore::new`, existing `AttachmentStore::open`, `ManagedStore::verify`,
  and `AttachmentStatus`; readers receive verified immutable original bytes.
- #25: `AttachmentService<ManagedAttachments>` / `ImportAttachment`,
  `LibraryDb::{managed_attachments,check_integrity,open_with_safety_backups,backup_library}`,
  `SqliteMigrationBackup` implementing existing `MigrationBackup`,
  `backup::{validate_bundle,restore_library,BackupManifest}`.

#25 authorization, transport request deduplication, daemon single-writer/lifecycle gate, physical deletion/GC,
and activation of a restored library are not implemented here. Migration/GC/replacement must be quiesced as
documented; immutable append-only ingestion can coexist with a snapshot. Files/DBs are flushed with fsync /
SQLite FULL and publication is atomic/no-replace. No hardware power-cut, malicious same-UID race/ACL isolation,
Windows publication, GPUI/native window or PDFium runtime validation was performed. The unchanged optional
native dependency lane is left to CI/root integration. No CLI cross-model review was run: root owns the
required independent different-model review and merge. This document reports local checks, not remote CI.

## Root integration on current main

Integrated merged #11 (`239c5a0`) before independent review. Both DESIGN appendices are
preserved (#11 §28, #9 §29), and immutable #8 SQL/manifest remain unchanged.
Root passed workspace fmt, clippy, 46 unit/integration tests plus 1 compile-fail doctest,
build, architecture, migration preservation against current main, and 17 Python tests.
An offline full lockfile regeneration was inspected and rejected because the local cache
selected unrelated transitive upgrades/downgrades. The automatically combined lock retains
both already-reviewed feature entries without that churn; all checks used `--locked`.
Optional GPUI and PDFium runtime lanes on this integrated head are remote CI gates.
