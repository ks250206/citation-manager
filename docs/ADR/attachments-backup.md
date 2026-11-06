# #9: Immutable attachments and recoverable backups

Status: adopted. Adds worker-side APIs to #8 without changing `PaperService`, existing
attachment read ports, or applied SQL/checksums. UI/PDF processing and daemon authorization
remain owned by their issues.

## Ingestion and recovery

`cm_core::attachments::{ImportAttachment, AttachmentLibrary, AttachmentService,
AttachmentStatus, AttachmentError}` is the additive application boundary. A trusted worker
composes `AttachmentService::new(db.managed_attachments(&directory)?)`.
`import(request)` returns a new `Attachment`; `status(id)` returns `Present`, `Missing`, or
`Corrupt`. Missing record/deleted paper is `Service(NotFound)`; filesystem safety failures
are `UnsafePath`. No existing association is replaced. The source is an explicit absolute
regular, single-link file with no symlink ancestors. Name/MIME are metadata, never file paths.

1. Copy through a 64 KiB buffer to a unique private `attachments/.staging/<uuid>.part`.
2. Flush the file, re-read its SHA-256/size, and compare the optional expected digest.
3. Publish `attachments/ab/cd/<lowercase-sha256>.pdf` with atomic **no-replace** rename
   (`renameatx_np(RENAME_EXCL)` on macOS, `renameat2(RENAME_NOREPLACE)` on Linux).
   The extension is a storage convention; no PDF parsing or MIME detection is performed.
4. An existing blob must match both hash and size. Never replace a corrupt existing blob.
   Remove only this operation's redundant private temporary when reusing a verified blob.
5. Flush the published/reused file and shard/staging directories **before** inserting the
   reference in an IMMEDIATE SQLite transaction. New directory entries are also flushed.

Different papers can refer to one blob. A failed DB association never removes a blob,
including one already used by another paper. Errors/cancellation before publication leave
`.part` files; interruption after publication and before commit can leave an orphan.
Interruption after commit can leave a successful association with no delivered response:
list references before retrying; transport-level request deduplication is #25's responsibility.

`LibraryDb::check_integrity(&directory)` checks SQLite integrity/FKs and returns
`IntegrityReport { dangling, corrupt, orphans, partials, unsafe_entries, backup_issues }`, including references
owned by soft-deleted papers. Quiesce writes during an inventory for a stable report.
**The scanner never deletes, fixes permissions, follows a symlink, or repairs DB references.**
A safe interrupted ingest can be retried from the original: matching published bytes are reused.
For a corrupt blob, restore verified bytes into a fresh library rather than replacing a shared
file in place. Partials/orphans require explicit operator investigation; there is no automatic GC.

`cm_storage::attachments::ManagedStore::new(&directory)` implements the existing
`cm_core::ports::AttachmentStore::open(&StorageKey)` with hash checking and a reader rewound
to byte zero. `ManagedStore::verify(&Attachment)` additionally checks the recorded byte size.
These are reusable by #15 export/PDF consumers. Readers must never write the managed original;
annotation/export output belongs in a separate file.

## SQLite baseline and migration hook

All disk `LibraryDb::open` / `open_with_backup` calls ensure a real standalone
`backups/initial.sqlite` after successful schema initialization and before returning to a writer.
The first successful open of an existing library snapshots its then-current state. Existing
baselines receive private-file safety checks at open and are never overwritten. A corrupt
baseline does not block a healthy live library: explicit `check_integrity` performs its full
content validation and lists damaged/missing/unsafe baselines in `backup_issues`. Neither
header corruption nor deeper corruption is treated as authority over the live DB. Concurrent
baseline publication accepts the winner and removes only the losing call's private stage. In-memory factories do not create files.

`LibraryDb::open_with_safety_backups(&directory)` also wires the real
`backup::SqliteMigrationBackup` into #8's `MigrationBackup` hook. Each upgrade publishes a
unique `pre-migration-<uuid>.sqlite` in `backups/`; `completed` exposes successful snapshot paths.
For explicit composition, pass `SqliteMigrationBackup::new(&private_backup_directory)?` to
`LibraryDb::open_with_backup`. Ordinary `open` retains #8's `BackupRequired` for pending upgrades.
Hook failure prevents schema changes. The immutable #8 SQL and history contracts remain intact.

These snapshots use `rusqlite::backup::Backup`, stepping 128 pages, with a 30-second deadline.
They contain committed WAL data. Destination files are private/exclusively created, checked
with `integrity_check` and `foreign_key_check`, converted to standalone DELETE journal mode,
closed/flushed, and published atomically. Failed snapshots retain `*.partial.sqlite` names.
**Never copy the live `library.sqlite` file alone.** Standalone DB snapshots do not contain PDF
bytes; use a full library bundle for portable recovery. A pre-migration snapshot can intentionally
have an older schema and is retained for diagnosis/version-specific recovery, not silently migrated
by the portable-bundle validator.

## Portable backup and fresh restore

Reusable APIs for #25 (trusted worker composition):

```rust,ignore
use cm_storage::{LibraryDb, LibraryDirectory};
use cm_storage::backup::{restore_library, validate_bundle};

let directory = LibraryDirectory::prepare(explicit_library_path)?;
let mut db = LibraryDb::open_with_safety_backups(&directory)?;
// destination_parent must already exist, be owned by this user, and have exact mode 0700.
// Choose a new absolute destination outside the running library.
let manifest = db.backup_library(&directory, fresh_bundle_path)?;
let bundle = LibraryDirectory::prepare(fresh_bundle_path)?;
validate_bundle(&bundle)?;
let restored = restore_library(fresh_bundle_path, fresh_restored_library_path)?;
// Activation/restarting clients under the daemon lock is a separate #25 action.
let restored_db = LibraryDb::open_with_safety_backups(&restored)?;
```

A bundle has `manifest.json`, standalone `library.sqlite`, and one copy of each referenced
blob. All SQLite tables are preserved: full bibliography/authors/external IDs, relations,
soft-deleted records, annotations (including source/unresolved anchors/page labels), full text,
jobs/token hashes, and version history. Caches, settings, logs, existing backups, unreferenced
blobs and partial ingests are excluded. This is a library-data backup, not an application-directory clone.

Manifest format 1 (`BackupManifest`, `BackupFile`, `SchemaVersion`) records schema IDs/checksums,
DB/blob SHA-256 values and sizes, and canonical relative names. It is bounded to 16 MiB and
100,000 distinct blobs; attachment bytes are streamed. Hashes detect damage, not authenticate
a bundle's author. Treat access to bundles as access to private library metadata/PDFs.

Backup snapshots the DB **first** and enumerates references from that closed, standalone
snapshot, then copies/checks only those immutable blobs. SQLite online backup can coexist with
metadata writes and append-only ingestion: newly committed references either belong to the
snapshot or are absent from it. The implementation never deletes/replaces published blobs.
**#25 must prevent migrations, physical blob deletion/GC/replacement and competing activation
for the entire operation**, and hold its write/lifecycle gate across the pre-migration hook and
migration. #9 does not implement a daemon lock or claim a distributed/externally-mutated filesystem
snapshot. The tests prove real committed WAL inclusion and snapshot stability across later writes;
they do not prove the future daemon's quiescence protocol.

Restore validates the manifest/file set/hashes/schema/SQLite integrity/FKs before copying into a
private sibling `.restore-<uuid>.partial`. It rechecks copied bytes, attachment-to-DB correspondence,
SQLite integrity and schema, and only then publishes a new destination directory by no-replace
rename. It does not overwrite/open/activate the running library or an existing destination.
A failed stage is retained with a `.partial` name; existing libraries and unrelated files survive.
Bundles reject unknown/unsupported versions/fields, duplicate or noncanonical names, absolute
paths/traversal, missing or extra files/directories, symlinks/hardlinks, unsafe permissions,
checksum mismatch, WAL-format/nonstandalone DBs, schema history mismatch and DB/file-set mismatch. The entire `sqlite_schema` (object type,
name, owner table and SQL, including indexes and triggers) must match the current immutable
migration registry applied to a fresh in-memory database; a rehashed manifest cannot authorize
extra triggers or altered DDL. Destination containment compares ancestor device/inode identities
as well as lexical names, so case aliases cannot place output inside the source on macOS.
A restored library uses the ordinary #8 factory to reopen in WAL mode after validation.

## Filesystem boundary and evidence

Managed directories require exact 0700; files require exact 0600, effective UID ownership,
regular type and a single hard link, using #8's ancestor checks. No chmod of existing inputs,
symlink following or unsafe automatic deletion. Bundle/destination parents must already exist;
restore never creates unrelated ancestor paths. macOS/Linux atomic no-replace publication is
implemented; other platforms fail rather than weaken publication. Use a local filesystem as #8
requires. Repack/unpack portable bundles preserving these private modes and no links.

As in #8, root, hostile concurrent same-UID processes, ACL/volume policy and external modification
of the private tree are outside the POSIX ownership boundary. Durability uses `sync_all` on files
and relevant directories plus SQLite FULL synchronous transactions; no physical power-cut or
storage-device barrier guarantee has been tested. Injected cut points and real DB commit failures
are covered with temporary libraries only. See [validation](../validation/ISSUE_9.md).

API details checked against official sources through find-docs and pinned Rust source:
[rusqlite backup](https://docs.rs/rusqlite/0.41.1/rusqlite/backup/index.html),
[rustix no-replace rename](https://docs.rs/rustix/1.1.6/rustix/fs/fn.renameat_with.html).

Import error mapping: `HashMismatch` means copied/expected digest disagreement; an already
published corrupt shared blob returns `CorruptBlob` without replacement. A missing selected
source is `Service(InvalidInput)`, unsafe paths remain `UnsafePath`, and unrelated integrity/IO
failures are `Unavailable` rather than falsely reporting a caller digest mismatch.
