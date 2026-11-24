# #25 validation: papersd, authorized IPC, single writer, durable jobs

Run 2026-11-24 by the implementing agent in an isolated worktree (base
`claude/beautiful-knuth-vl7zc1` @ `97c8fb3`). Environment: Linux 6.18 x86_64 container (runs as
root), rustc/cargo 1.99.1, Python 3.13.18. Design: [daemon-ipc ADR](../ADR/daemon-ipc.md).

## Automated (Linux)

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| `cargo clippy -p cm-daemon --all-targets --features desktop-bootstrap --locked -- -D warnings` | passed (Linux GPUI deps) |
| `cargo test --workspace --locked` | passed: 252 tests, 0 failed, 0 ignored (55 test binaries/doctest groups) |
| `python3 scripts/check_architecture.py` | passed (no new crate edge) |
| `python3 scripts/check_migrations.py --base origin/main` | passed: 3 migrations, baseline preserved |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | passed: 32 |
| `cargo check -p cm-protocol -p cm-core --target aarch64-apple-darwin --locked` | passed |

New tests (all in the workspace run above):

- `cm-core` `policy::authorizer` (4): default deny, liveness, presets, narrowing-only scopes, issuance subset.
- `cm-protocol` `ipc::frame` (1): limits, truncated/oversized frames, EOF.
- `cm-storage` `daemon` (3): grant round trip/revoke, idempotent submit + payload conflict, lease
  fencing (stale generation cannot heartbeat/commit), recovery to retry_wait/failed-at-limit,
  explicit retry, backoff not runnable early, deadline failure, running cancel.
- `daemon_ipc` (4): GUI-less `LibraryClient` CRUD/query/organization/settings/trash over real IPC,
  daemon stop → `Unavailable` then reconnect to a new daemon; 0700 dirs, 0600 socket/lock/credential,
  secret not stored in the DB, unsafe runtime dir refused by the client; handshake rejects unknown
  credential, version mismatch, wrong library, request-before-Hello, oversized pre-auth frame,
  silent client (timeout); oversized authenticated frame closes the session.
- `daemon_auth` (4): native-host grant spoofing `client_kind=desktop` + all scopes is still denied
  trash/get/query/export/parse/delete/grant list, allowed destination list and capture preview with
  hidden matches, denied commit; MCP read-only query OK, create/self-elevation/job submit denied,
  separately issued write grant works, narrowed admin cannot exceed itself; revocation and expiry
  deny live sessions and new handshakes; preview bound to its grant (commit/cancel from another
  writer denied, owner commits); jobs invisible to other grants; kind-specific capabilities.
- `daemon_jobs` (3): `attachment.import` idempotent key, payload conflict, committed-result detection
  (`detected_committed`) and a lost-result-after-commit retry — exactly one attachment each time;
  attempt limit → failed "attempt limit 2 reached: transient", retry_wait observed, explicit retry
  succeeds, succeeded job cannot be retried; running cancel; past deadline → failed; unknown kind.
- `daemon_lifecycle` (4, real `papersd` binary): 4 concurrent starts → 3 exit 75, one PID serves all
  clients; lazy start from a client, held connection keeps it alive past the idle timeout, idle exit
  removes the socket, next request starts a new daemon; retry_wait/running job keeps a daemon with a
  200 ms idle timeout alive until the job succeeds (attempts 2), then it exits; SIGKILL during a
  running job leaves socket+lock files, a new daemon starts anyway, reclaims the lease (attempts+1,
  newer generation) and the job can be cancelled.

## Headless GUI smoke (Linux, Xvfb)

`cargo build -p cm-daemon --bins --features desktop-bootstrap --locked`, then the real
`citation-manager --library <temp>` under `xvfb-run` with a private `XDG_RUNTIME_DIR`: it started
the sibling `papersd` in its own process group, the runtime directory was 0700 with a 0600 socket,
`<library>/daemon/{desktop.credential,papersd.lock}` were 0600. An independent Python client using
the desktop credential got `Welcome` and `Status.connections = 2` (GUI + probe), proving the GUI held
an authenticated session. After killing the GUI the daemon exited on its own ~31 s later (30 s idle
default). No window interaction was performed.

## Not verified

- macOS: peer credentials (only the 0700 directory boundary applies there), `.app` sibling start,
  Finder/launchd launch, bundle relocation, browser host probes (#34/#23). `cargo check` of
  `cm-daemon` for `aarch64-apple-darwin` was blocked by C build dependencies (`aws-lc-sys`) needing
  an Apple SDK; only `cm-core`/`cm-protocol` were cross-checked.
- A different-UID peer (tests run as one user; the SO_PEERCRED rejection path is not exercised).
- Interactive GUI operation over IPC (import/PDF/DOI flows were not driven in a window).
- Credential storage beyond the 0600 file (Keychain, consent UI, rotation UX) — #26.
