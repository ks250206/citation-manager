# #10 native macOS vertical slice — validation

2026-11-06. Base: merged main `08cb717dd26b174bcada3f291b0cd466f39b2ea7` (#8).
Worktree: T3-prepared `t3-3d803c19`, branch `t3/native-desktop-library`.
Environment: macOS 27.2 arm64, Rust 1.99.1, Xcode 26.6 (17F113).
GPUI/gpui_platform retain workspace revision `4a6121bb0f10c6e8037b741b7dc0a4cd639ae289`.

## Actual native result

The **real GPUI executable was built, linked, launched and operated through native
CUA**. This is separate from the headless tests and CI compile lane. No browser,
seeded paper/demo list, real user library, attachment store or PDFium was used.
All paper rows came from synthetic manual input in a dedicated temp library.
Final executable SHA-256:
`58c7a4eedb6fd3260633bdeab9f83e5084d29a57e854ca6c208706d7f4d3a081`.

- Create via New/⌘N, Unicode paste, Tab, ⌘S. Edit title/detail with mouse focus,
  ⌘A, paste, normal typing/backspace, and Save locally. Confirm `Saved locally` and
  the selected row/ID. [Saved native window](issue-10/01-native-saved.png).
- ⌘Q terminated the process; `pgrep` found no matching bundle executable. Relaunch
  the same binary/library and select the saved paper. The title, detail and ID survived.
  [Relaunched native detail](issue-10/02-native-relaunched.png).
- Relaunch and edit/save under macOS `sandbox-exec` denying inbound/outbound IP traffic
  **for this app process only**. The negative probe using the same profile rejected
  a Python TCP connection to `127.0.0.1:9` with `PermissionError: Operation not permitted`.
  The user's network settings were not changed. [Offline write](issue-10/08-native-offline-save.png).
- Clear title and save: visible validation error, draft retained, confirmed row not replaced
  by blank data. A read-only DB query verified the previous stored value remained.
  [Validation error](issue-10/03-native-validation-error.png).
- Hold `BEGIN IMMEDIATE` on the **temp DB only**, quit/relaunch: open fails visibly
  after #8's busy timeout; Reload retries. [Open error](issue-10/04-native-open-error.png).
  During retry `Loading…` is visible and Save is disabled.
  [Loading](issue-10/05-native-loading.png).
- During the blocked read, New and typing remained responsive. Its newer draft survived
  the superseded response/error. [Stale response ignored](issue-10/06-native-stale-response-ignored.png).
  Release the lock, Reload and select: saved title/detail return.
  [Recovered](issue-10/09-native-recovered.png).
- A long Unicode title scrolls horizontally to keep its caret visible.
  [Long input](issue-10/07-native-long-unicode-input.png).
- Final quit was observed by CUA and a process check; final read-only `PRAGMA integrity_check`
  returned `ok`. [Runtime observations](issue-10/native-observations.txt).

Native AX exposes the window controls, but GPUI content is custom drawn; CUA used
screenshots/coordinates plus keys. On initial app activation an immediate screenshot
could be older than the latest key events, so results were checked in a subsequent
observation before being recorded. A repeated create during this check produced two
papers with the same title, which #8 correctly allows without a DOI/citation-key conflict.

## Reproduce safely

```sh
cargo build -p cm-daemon --bin citation-manager --features desktop-bootstrap --locked
python3 scripts/prepare_native_smoke.py --deny-network
# Run the printed open command. Keep smoke.json and reuse the SAME command/library.
```

The helper creates a fresh private temp directory and unsigned local .app, copying the
actual built binary. `--deny-network` installs a small local launcher using this profile:

```text
(version 1)(allow default)
(deny network-outbound (remote ip "*:*"))
(deny network-inbound (local ip "*:*"))
```

Click New paper once the window is active, type a title/detail, Save, edit, Save, ⌘Q.
Run the same printed `open` command; select the saved row using mouse or ⌘↑/⌘↓.
For contention, close the app, open that temp `library.sqlite` with Python sqlite3,
execute `BEGIN IMMEDIATE`, and hold the connection while relaunching/retrying. Use
New while it is loading, type a draft, then release the lock; the draft must survive.
Reload and select the saved record. Do not point these steps at a real library.

This executable also runs directly with an explicit absolute `--library` path. The
throwaway .app only provides a LaunchServices/CUA identity; a bare CLI binary cannot
be addressed reliably by CUA. It is not a product bundle/signing/distribution claim.

## Automated results

All passed on the final source; compact command output is in
[automated-checks.txt](issue-10/automated-checks.txt).

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| `cargo test --workspace --locked` | 36 passed, including #8 storage/DTO/DOI regressions and 1 compile-fail doctest |
| `cargo build --workspace --locked` | passed |
| `cargo check -p cm-daemon --bin citation-manager --features desktop-bootstrap --locked` | passed |
| `cargo clippy -p cm-daemon -p cm-desktop --all-targets --features cm-daemon/desktop-bootstrap --locked -- -D warnings` | passed |
| `cargo test -p cm-daemon -p cm-desktop --features cm-daemon/desktop-bootstrap --locked` | 6 passed; these are headless state/worker tests, not GUI tests |
| `cargo build -p cm-daemon --bin citation-manager --features desktop-bootstrap --locked` | linked actual executable |
| `python3 scripts/check_architecture.py` | passed; 12 crates and narrow optional/macOS bootstrap edge |
| `python3 scripts/check_migrations.py --base origin/main` | passed; 2 immutable SQL/checksums unchanged |
| `python3 -m unittest discover -s scripts -p 'test_*.py' -v` | 14 passed, including invalid bootstrap dependency/default wiring |
| `git diff --check` / staged whitespace check | passed |

New headless tests cover out-of-order data/errors, double-save/navigation guards,
failed save retaining confirmed data, metadata preservation, pagination cursor after
insert, an injected deferred client, worker callback thread identity, Unicode create/edit,
reopen, DOI normalization/conflict and invalid library/title errors. Existing #8 tests
remain unchanged. Native CI builds the actual executable but never launches the GUI.
CI status and independent different-model review are recorded on the PR by the owner;
local checks do not imply either has passed.

## Limits and ownership

The [ADR](../ADR/desktop-bootstrap.md) describes the temporary composition edge and
transition to #25. No HTTP/IPC daemon lifecycle/auth/single-writer lock, backup/file store,
DOI resolver/import, collections, PDF viewer, or signing is claimed. The editor currently
exposes title and a single-line abstract/detail; other stored fields survive updates.
Undo/redo, multiline editing and confirmation before discarding an unsaved draft on
New/selection/quit are not implemented. Native IME protocol/UTF-16 conversion and marked
text are wired from the pinned official example; Japanese input-source candidate UI was
not exercised. Unicode/grapheme/clipboard/editing were exercised as described above.
Developer ID signing/notarization remain unavailable/outside #10; local unsigned run passed.
