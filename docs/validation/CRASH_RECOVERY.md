# #50: owned-process SIGKILL recovery (headless subset of #30)

This adds private Rust unit-test subprocess regressions to the existing #8/#9 tests.
It exercises real `PaperLibrary::update` and `ManagedAttachments::import` (the normal
`Cut::None` route), kills the owned child after a verified checkpoint, then reopens
real SQLite and inventories immutable files. The existing error-return `Cut` tests
remain unchanged and are not counted as process-death evidence.

## Reproduce

Use the pinned repository toolchain and an ordinary Unix/macOS checkout. No native
features, UI, accounts, PDFium download, user library or opt-in/ignored tests are needed.

```sh
cargo test -p cm-storage --lib --locked crash_recovery -- --nocapture --test-threads=1
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/check_architecture.py
python3 scripts/check_migrations.py --base c54fa3984c729f5e731cf17a223033c2aa1f6d25
python3 -m unittest discover -s scripts -p 'test_*.py' -v
git diff --check
```

The full workspace command deliberately includes both existing 1,000-record BibTeX
and RIS real-SQLite roundtrips, without filtering or ignoring them. It also runs the
SIGKILL regressions with libtest's ordinary parallel scheduling. The first command
runs them serially only to make the per-case evidence easy to read.

`crates/storage/src/crash_recovery.rs` is compiled only by
`#[cfg(all(test, unix))] mod crash_recovery`. Every added call in production source
has that same condition; release/non-test builds contain neither the module nor
its environment handling or checkpoint calls. There is no new executable, public
test API, feature, dependency, migration or runtime switch. SQL, manifest,
`Cargo.lock` (636 package entries) and every Cargo manifest are preserved from base
`c54fa3984c729f5e731cf17a223033c2aa1f6d25`.

## Ownership, handshake and bounded cleanup

Each parent creates a fresh `TempDir` directly under canonical `/tmp` (macOS:
`/private/tmp`) with a random UUID nonce in its name and explicit mode 0700 at
creation. Short paths fit macOS's Unix socket path limit. All data is synthetic:
two papers, ordered contributors/raw external IDs, and two initial references to
one shared immutable blob. The original is 196,625 bytes; the distinct new source
is 131,103 bytes. Sources and control files are newly created 0600 files.

The parent launches `current_exe()` with `--exact crash_recovery::owned_child_entry`
and child-specific `Command::env` values. There is no process-global environment
mutation. Before opening its fixture, the entry checks the exact invocation, UUID,
canonical root/name, root's UID/private permissions/ancestors, and the private nonce
file. The entry initializes the private checkpoint context only in that child.
Other unit tests have no initialized context and the hooks do nothing.

The product-operation child disables WAL auto-checkpointing on its own connection
with `wal_autocheckpoint=0` to retain committed WAL frames for reopen recovery.
Production leaves auto-checkpointing enabled; its `synchronous=FULL` setting is
unchanged in these tests. This is a deliberate test condition, not evidence of
recovery under every production checkpoint schedule.

At the selected hook the child validates the actual database path, connects to the
parent's private Unix socket, sends `nonce:stage\n`, and blocks. The parent accepts
only an exact nonce/stage match before invoking `Child::kill` on the `Child` handle
it obtained from `Command::spawn`; no PID is parsed or globally matched. It reaps
using `try_wait` and asserts Unix `signal() == Some(9)` and `code() == None`. SIGKILL
therefore prevents Rust DB/transaction destructors and rollback/cleanup from running
in that child. Panic, normal exit and `Cut` errors cannot satisfy these assertions.

Readiness has one absolute 10-second deadline, a 128-byte message bound and socket
read timeouts; reap has a separate 10-second deadline. The blocked child has a
30-second read/park deadline if its parent disappears. The 5-ms sleeps only poll
socket/exit readiness; elapsed time never selects a successful product kill point.
No stdout pipe or reader thread is created. `OwnedChild::Drop` kills/reaps an
uncollected owned child on assertion/handshake failures. Accepted sockets, including
rejected/partial handshakes, remain owned by the guard until after reaping, avoiding
an EOF-induced child-unwind race. Cleanup failure fails the run (or is explicitly
reported during an existing unwind); it is never recorded as passed evidence.
The two synthetic parked children recheck an absolute deadline after every wake,
so a spurious `park_timeout` return cannot turn them into an early panic/exit.

The harness regression separately checks early successful exit, no connection,
connected-but-silent read timeout, wrong nonce and wrong stage. Receipts confirm
that all five children are reaped; four have signal 9 and early exit has code 0.
These synthetic harness cases are not product recovery cases. The helper entry
returns immediately in a normal parent test run and is also not crash evidence.
Thus the dedicated command's **10 passed tests = 8 real product SIGKILL cases +
1 harness-failure test (5 child invocations) + 1 helper-only no-op**. Nothing is ignored.

## Product checkpoints and observed state

All cases start with the same two existing papers and shared references. Counts
below describe the reopened library before any retry. "Target refs" counts the
paper being changed; the unrelated paper always retains its one exact reference.

| Case / selected hook | Actual operation at kill | Reopened metadata | Target refs / total refs | Remaining partials / orphans | Operator action |
|---|---|---|---|---|---|
| metadata-transaction | Complete metadata/authors/external-ID replacement inside actual IMMEDIATE transaction, before commit | Entire old generation, including raw person UUIDs and ordered raw IDs | 1 / 2 | 0 / 0 | Keep old state |
| metadata-committed | Immediately after actual update commit, before return | Entire new generation, old created_at preserved | 1 / 2 | 0 / 0 | Retain committed state |
| attachment-staged, new source | Real copy synced, closed and independently hash/size verified, before publication | Old, unchanged | 1 / 2 | 1 / 0 | Inspect, then explicit import |
| attachment-published, new source | New blob published and file/shard/staging syncs completed, before reference transaction | Old, unchanged | 1 / 2 | 0 / 1 | Inspect, then reuse matching published blob |
| attachment-transaction, new source | Actual reference inserted in IMMEDIATE transaction, before commit | Old, unchanged | 1 / 2 | 0 / 1 | Inspect, then reuse matching published blob |
| attachment-committed, new source | Actual reference committed, before return | Old, unchanged | 2 / 3 | 0 / 0 | List and retain committed result; no import retry |
| attachment-transaction, shared source | Matching existing shared blob reused; actual reference inserted, before commit | Old, unchanged | 1 / 2 | 0 / 0 | Inspect, then explicit import using unchanged shared blob |
| attachment-committed, shared source | Reference to existing shared blob committed, before return | Old, unchanged | 2 / 3 | 0 / 0 | List and retain committed result; no import retry |

The staged `.part` is a **complete 131,103-byte staged file**, not a partially
written stream. Its random filename is under `attachments/.staging/<uuid>.part`.
The orphan in the two new-blob precommit cases is the new source's
content-addressed blob. Deterministic fixture hashes/paths are:

- Original/shared SHA-256: `34d4df63f6e27079061c021e1e61c7f6cabb93cd736ca2b16bc6ad3c11427955`, at `attachments/34/d4/34d4df63f6e27079061c021e1e61c7f6cabb93cd736ca2b16bc6ad3c11427955.pdf`.
- New/staged/orphan SHA-256: `6c0bd559314b5b5299fbd48e58e36938bd4b2ba9854147c5b3111e7243cb2d6a`, at `attachments/6c/0b/6c0bd559314b5b5299fbd48e58e36938bd4b2ba9854147c5b3111e7243cb2d6a.pdf` after publication.

The tests check hashes and the
exact reported file sets; repeated inventories leave names, bytes and modes
unchanged. The staged partial remains after explicit retry; the orphan becomes
referenced. No scanner deletion/chmod/repair is added. Fixture `TempDir` cleanup
only removes the parent's synthetic tree at the end of the test.

Every case verifies real SQLite `integrity_check` and `foreign_key_check`, complete
paper values, ordered contributors and raw external IDs, the unrelated paper and
its raw contributor identities, initial attachment identities/metadata, both
original-source hashes and the shared published blob hash. Metadata rollback also
checks the complete raw relation snapshot; person counts reject leaked/half-written
contributor records. Attachment cases check exact target/other reference vectors.

For new/shared precommit publication retries, device/inode identity must remain
unchanged: the matching blob is reused rather than removed/replaced, and both old
references survive. After retry the target has exactly two references; the new
reference matches the original source. A final close/reopen retains that count.

### Unknown result: inspect before retry

The [attachment ADR](../ADR/attachments-backup.md#ingestion-and-recovery) requires
listing actual references when a committed result may have lost its response.
The #9 low-level import is append-only and does **not** deduplicate blind retries.
The test first lists the target's real references. In both postcommit cases it
validates the new reference's ID, paper, key, SHA-256, size, name and MIME, retains
that exact result and does **not** call import again. The reference vector stays
identical with zero duplicates introduced by the harness. Only the four cases
with a proven uncommitted outcome explicitly import again. This is a conservative
operator procedure, not transport idempotency, a lease, or an exactly-once guarantee.

### Portable recovery of the actual preserved state

Before deciding whether to retry, **all eight cases** call the existing
`backup_library` -> `validate_bundle` -> `restore_library` sequence into fresh
private sibling paths. The restored library is reopened using the normal factory.
Complete papers, raw internal contributor identities/ordered external-ID rows,
exact reference vectors and each referenced blob's hash/size must match the
recovered source. Integrity/FKs pass and the restored inventory is clean.

The six precommit/metadata bundles contain two references to one shared blob;
the two postcommit attachment bundles contain three references (two distinct blobs
for the new source, one shared blob for the shared-source case). Partials/orphans
are deliberately excluded by the existing portable-data contract. They remain in
the recovered source; a clean portable restore is not a claim that they were repaired.

## Execution evidence and boundaries

See [run results](CRASH_RECOVERY_RESULTS.json) for exact commands, environment,
exit codes, timings, pinned/source checks and full-suite counts, and
[serial subprocess output](CRASH_RECOVERY_SUBPROCESS.log) for the actual stage,
SIGKILL, reap, inventory, retry and backup/restore assertions from each case.

The unfiltered workspace run passed **186 tests**, with 0 failed, 0 ignored, and
0 filtered out across its test executables/doc-tests. The Python run passed 22 tests.
The dedicated serial SIGKILL command passed 10 tests; its 16 filtered-out tests
are existing unit tests that also run in the unfiltered workspace command.

After independent review of `ca78726`, root changed only the two synthetic parked
children to tolerate spurious wakes and added the WAL/coverage disclosures above.
On that follow-up source, root reran the serial subprocess command (10 passed),
ordinary parallel `cargo test -p cm-storage --lib --locked` (26 passed),
`cargo check -p cm-storage --locked`, storage all-targets locked Clippy with
`-D warnings`, workspace fmt-check and `git diff --check`; all passed. The 186-test
workspace and 22-test Python figures remain the original implementation results,
not a claim that those full suites were rerun for this small follow-up.

The observed platform is macOS 27.2 (26B123), Darwin 27.2.0, Apple Silicon
Mac16,6 / arm64 with 38,654,705,664 bytes memory and local APFS. Toolchain:
Rust/Cargo 1.99.1, Python 3.14.0. Builds/tests use ordinary debug/default features.
The host is **not** the 16-GB full-load benchmark target. Linux/other Unix execution
was not performed; hooks/tests are Unix-gated and signal verification here is macOS.
No UI/native-feature/runtime checks were required or claimed.

API uncertainty was checked with find-docs/Context7 and the pinned local tempfile
source (`tempfile 3.27.0`, `Builder::permissions`). Context7's Rust search was too
broad for process cleanup, so the official Rust 1.99.1 primary docs were checked:
[Child](https://doc.rust-lang.org/std/process/struct.Child.html),
[ExitStatusExt](https://doc.rust-lang.org/std/os/unix/process/trait.ExitStatusExt.html),
[UnixStream](https://doc.rust-lang.org/std/os/unix/net/struct.UnixStream.html).

**Not proved:** power loss, OS/filesystem crash, physical device barriers/fsync
persistence, interrupted partial streaming copy, jobs/IPC/daemon writer lifetime,
leases/request deduplication, migration kills, FTS recovery, security regression,
PDF/GUI/native runtime, signing, 16-GB/full-load performance, or exactly-once writes.
Shared-source kills at the staged and published checkpoints are not covered;
shared-source transaction/committed cases are covered. No mutation test established
that these assertions catch a write-order change accompanied by moved checkpoints.
This PR changes no production write/fsync/publication order or public API. #30's
other requirements/dependencies remain open; #15/PR #45 Draft GUI block, #34 HELD,
and the #23/#25 closed gates remain unchanged. Independent Opus review and CI
integration/merge belong to the root agent; this implementation performs one CI
inspection and no watch/poll/merge.
