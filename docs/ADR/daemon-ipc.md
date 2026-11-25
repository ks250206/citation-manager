# #25: papersd, authenticated local IPC, single writer, durable jobs

Status: adopted for M4-1 on Linux/Unix. macOS lifecycle/.app placement is not verified here
(#34). Concrete feature contracts extend this compatibly; HTTP (#27), CLI (#28) and MCP tools
(#29) build on it. Persistent tokens, scopes, elevation, audit and API settings (#26):
[api-tokens-grants](api-tokens-grants.md).

## Processes and ownership

`papersd` (`crates/daemon/src/bin/papersd.rs`) is the only process that opens a library's
SQLite database for writing or migration. Start order (`cm_daemon::ipc::start`):

1. Prepare the private library directory and `<library>/daemon/` (0700).
2. Take `<library>/daemon/papersd.lock` with an OS advisory lock (`flock` via
   `File::try_lock`). A second daemon exits with status 75 (`EXIT_ALREADY_RUNNING`).
   The kernel releases the lock when the holder dies (SIGKILL included), so a stale lock
   file never blocks a restart and needs no PID guessing; the PID inside is diagnostic only.
3. Open with `LibraryDb::open_with_safety_backups`: the lock holder is the only migration
   runner and upgrades take #9's pre-migration online backup.
4. Load grants, ensure the owner bootstrap credential (below), start the existing library
   worker (`InProcessLibraryClient`, now daemon-internal) and the job runner.
5. Remove a leftover socket only while holding the lock, bind a 0600 Unix socket inside a
   0700 runtime directory. The optional loopback REST listener (#27, [rest-api](rest-api.md))
   starts only when the persisted API settings enable it and dispatches through the same
   `Session::dispatch`.

The GUI (`citation-manager`) no longer embeds the worker. It injects
`cm_protocol::client::IpcLibraryClient` into the unchanged views (`LibraryClient`), and
starts `papersd` from the same directory as its own executable (`.app/Contents/MacOS` or
`<prefix>/bin`), never via PATH. Offline operation is unchanged: IPC is local only.

## Endpoint and lifecycle

`DaemonPaths` (`cm_protocol::client::local`, std-only so CLI/MCP/native host can reuse it)
canonicalizes the library path first (a library that does not exist yet via its nearest
existing ancestor), so every spelling of one library shares one socket, lock and
`Hello.library`. Socket `<runtime>/<first 64 bits of SHA-256(canonical path), hex>.sock`
(a dependency-free SHA-256 keeps `cm-protocol` free of implementation crates); runtime
directory override, else `$XDG_RUNTIME_DIR/citation-manager`, `$TMPDIR/citation-manager-<uid>`,
`/tmp/citation-manager-<uid>`. Clients refuse a runtime directory that is not a real directory
owned by their UID without group/other bits, and a socket not owned by them. `Hello.library`
must equal the daemon's library (a hash collision cannot attach to the wrong library).

`LocalConnector` connects or starts `papersd` (own process group, stdio closed) and retries
with backoff until a timeout. Concurrent starts: losers exit 75, every client reaches the
single winner. If the lock holder was a daemon in its idle exit (75 but no socket appears),
the connector launches again, at most every 500 ms, until its timeout. A connection closed
during the handshake (the daemon accepted just before its idle exit) is retried exactly once
(`connect_and_handshake`). Lifetime: a connection or any active job (queued/running/retry_wait) keeps the
daemon alive. Idle time counts from when both reached zero; after `--idle-timeout-ms`
(default 30 s) the daemon removes its socket and exits. The GUI keeps one connection open for
its lifetime. A lost daemon fails in-flight requests with `Unavailable`; the next request
reconnects (and restarts it).

Client liveness (`cm_protocol::client::ClientTimeouts`; `IpcLibraryClient::new` uses the
defaults, `with_timeouts` overrides them). Nothing the daemon does can block the GUI forever:

| Limit | Default | Applies to |
|---|---|---|
| handshake | 10 s | each read/write of Hello/Welcome (`handshake`, `Session::open`, the client) |
| write | 30 s | socket writes of request/cancel frames |
| control | 30 s | `Status`, `Jobs`, `Grants` (answered on the daemon connection thread) |
| interactive | 5 min | library CRUD, query, organization, lifecycle, settings, DOI resolve |
| long | 60 min | `Import`, `Export`, `Pdf`; each PDF progress frame restarts it |
| reconnect backoff | 250 ms doubling to 10 s | from the 2nd consecutive failed connect |

Library commands share the daemon's single library worker, so a query may wait behind an
import; hence the generous interactive/long limits. An expired request completes with
`Unavailable` and a best-effort `Cancel` is sent; the connection is dropped (and re-made by the
next request) only when no other request is still waiting on it, so one slow request never
aborts a long operation within its own limit. A failed connect also fails the requests already
queued behind it, and during backoff requests fail fast instead of each re-running a daemon
start.

## Wire protocol

Frames are a 4-byte big-endian length plus UTF-8 JSON (`cm_protocol::ipc::frame`), with
limits per direction: first (pre-auth) frame 64 KiB, requests 8 MiB, responses 64 MiB. A
declared oversized length is rejected before allocation. At most 64 in-flight requests per
connection (`Busy`), 64 connections, 5 s to complete the handshake.

```text
client: Hello{protocol_version, client_kind, requested?, library?, credential}
daemon: Welcome{protocol_version, daemon_version, session, grant, capabilities, limits}
      | Rejected{VersionMismatch{supported} | Unauthenticated | PeerRejected | WrongLibrary | Malformed}
client: Request{id, command: Command} | Cancel{id}
daemon: Response{id, result: Result<Reply, WireError>} | PdfProgress{id, report}
```

`PROTOCOL_VERSION = 1`. `Command`/`Reply`/`JobCommand`/`GrantCommand` are grouped by feature
module and `#[non_exhaustive]`; additive variants keep the version, incompatible changes bump
it. Unknown commands get `InvalidInput`, never execution. `PdfRequest` travels as
`PdfCommand`: the client keeps its `Cancellation` and sends `Cancel{id}`; the daemon binds a
fresh token per request and cancels all of a connection's tokens when it closes.
`WireError::Denied(Denial)` maps to a visible `ServiceError::InvalidInput("not permitted: …")`
through the `LibraryClient` adapter. Core/domain DTOs gained additive serde derives only.

## Authentication and authorization (security ADR)

- Credentials are 256-bit random secrets (`/dev/urandom`), shown once; only SHA-256 is stored
  (`daemon_grants`). `Credential`'s `Debug` is redacted.
- Every request is mapped to one `cm_core::policy::Operation` and authorized by
  `policy::authorize` against the **current** stored grant (default deny). Revocation and expiry
  therefore apply to live sessions (`Denied(Revoked|Expired)`), and new handshakes fail.
- `client_kind`, `requested` scopes, UID and PID are never authority. `requested` only narrows
  (grant ∩ requested). A native-host grant claiming `client_kind = "desktop"` and all scopes
  still holds only `CapturePreview + DestinationRead`.
- Peer check: Linux `SO_PEERCRED` UID must equal the daemon's; on macOS the private 0700
  directory is the OS boundary (rustix exposes no `LOCAL_PEERCRED`; to verify in #34).
- Presets: `desktop` (all capabilities, GrantAdmin), `native_host` (capture preview,
  destination names/IDs; no paper reads, delete, export, commits or local paths; previews hide
  duplicate matches without LibraryRead), `mcp_read_only` (LibraryRead, JobsRead).
- Elevation is a separate path: only a live `GrantAdmin` session can `GrantCommand::Issue`, and
  only a non-empty subset of its own (narrowed) capabilities. MCP cannot mint itself write.
- Preview binding: an import/PDF preview handle is bound to the grant that created it; commit or
  cancel from another grant is `Forbidden` even with LibraryWrite. Retained parse/PDF selection
  state is per session. Import and PDF previews both hide duplicate matches without LibraryRead.
- Jobs: non-admin grants see/cancel/retry only their own jobs; each job kind adds capability
  requirements (`attachment.import` needs LibraryWrite + LocalFileRead) that the *session*
  must hold for submit **and** explicit retry (`Forbidden(JobRetry)` otherwise). Cancel only
  stops effects and needs JobsControl alone.
- Jobs never outlive their owner's authority: the runner checks the owner grant (live, and
  still holding the kind's capabilities) when it claims a job and at every heartbeat. Revoking
  a grant fails its queued/retry_wait jobs at once and stops its running job, all with error
  "owner grant revoked"; an expired owner's queued job fails "owner grant expired" when
  claimed. Ownerless jobs are daemon-internal.
- Revocation fails closed: the in-memory registry consulted by every request is updated before
  the database, so a failed write still revokes the grant for the daemon's lifetime (the
  command reports the error). The `desktop-bootstrap` label is reserved for the daemon's own
  bootstrap grant; `GrantCommand::Issue` rejects it.

Minimal bootstrap: on start the daemon checks `<library>/daemon/desktop.credential` (0600). If
missing or not matching a live `desktop-bootstrap` grant it revokes previous bootstrap grants,
issues a new desktop grant and atomically writes the secret. Any same-UID process can read this
file; the security ADR's same-UID limitation applies. Keychain storage, consent UI, listing and
rotation UX are #26.

## Durable jobs

Table `daemon_jobs` (migration `20261124T000000Z_issue-25_001_daemon-grants-jobs.sql`):
state, attempts/max_attempts, progress, error, result, `detected_committed`, cancel flag,
lease owner/generation/expiry, next attempt, deadline, owner grant, and a unique index on
`(coalesce(owner_grant, ''), kind, idempotency_key)`.

- Submit is idempotent per owner grant on `(kind, idempotency_key)`: the grant's existing job
  is returned in any state; a different payload under the same key is `Conflict`. Another
  grant's key never matches, so keys neither leak another owner's `JobStatus` nor reveal that
  a job exists.
- Claim (one IMMEDIATE transaction) fails expired deadlines, then leases the next runnable job:
  `attempts+1`, `lease_generation+1`, expiry `now + lease`. Every running-job update is fenced on
  `(state='running', lease_generation)`; a stale runner can neither renew nor commit.
- A heartbeat renews the lease every `lease/3` (default 30 s lease) and persists progress.
  Live progress/cancel are in memory between beats because **any committed write to
  library.sqlite invalidates open #33 import previews** (conservative `data_version` revision).
  Grant changes and job transitions are therefore also preview-invalidating writes; #18 must
  address this before frequent job writes (e.g. a revision over paper tables only).
- Retryable failure: `retry_wait` with backoff `1 s * 2^(attempt-1)` capped at 60 s; at the
  attempt limit (default 3, 1..=20) → `failed` "attempt limit N reached: reason"; a retry past
  the deadline → `failed` "deadline exceeded before retry". Fatal errors fail immediately.
- Cancel: queued/retry_wait → cancelled; running → `cancel_requested`, the handler stops.
- Running time: an attempt that passes the job's `deadline_at_ms` fails "deadline exceeded
  while running"; one that exceeds its kind's `JobHandler::max_run` (default 60 min) fails
  "run time limit of N ms exceeded" (no retry). Stopping is cooperative: the handler sees it
  at its next `progress` call.
  Explicit retry of failed/cancelled resets attempts (and the deadline) to queued.
- Restart recovery: the new lock holder reclaims every `running` row of another owner
  (or with an expired lease): cancel requested → cancelled; attempt limit → failed
  "lease expired; attempt limit reached"; otherwise retry_wait due now.
- This is **at-least-once execution, not exactly-once**. Handlers keep effects idempotent and
  detect committed results: `attachment.import` keys its effect by `(paper, sha256)` and returns
  `AlreadyCommitted` (`detected_committed = true`) instead of registering twice.
- Built-in kinds: `attachment.import`, `diagnostic.wait` (no library effect; used to test
  lifecycle/retry/lease recovery). #18 adds extraction kinds via `JobHandler`.

## Verification and limits

Linux automated evidence: [ISSUE_25](../validation/ISSUE_25.md). Not verified here: macOS
peer credentials, `.app`-relative start, launchd/Finder launch, browser host probes (#34/#23),
signed bundles (#31). The GUI path was compiled and clippy-checked, not driven interactively.
