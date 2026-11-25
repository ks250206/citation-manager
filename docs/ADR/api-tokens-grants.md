# #26: persistent tokens, scopes, MCP profiles, elevation and API settings

Status: adopted on Linux. Builds on [daemon-ipc](daemon-ipc.md) (#25) and the
[security ADR](security.md). REST (#27), CLI (#28) and MCP (#29) import the scope table from
core and send every request through the daemon's single authorization decision. macOS
Keychain storage and the `.app` keychain ACL are not verified here (#34).

## One principal model, one decision

API, MCP and CLI tokens are **#25 daemon grants**. There is no second token table and no
transport-specific check: each request is mapped to an `Operation` and
`policy::authorize` runs against the grant's *current* capabilities in the daemon's live
registry (`Authenticator`). Issuing, re-issuing, scope changes, elevation and revocation
update that registry, so they apply to the next request of every live session (IPC today,
in-process REST in #27). Revocation and narrowing update memory first (fail closed);
widening is persisted first.

Migration `20261124T180000Z_issue-26_001_tokens-settings.sql` adds `client_kind`,
`scopes_json`, `last_used_at_ms`, `elevated_until_ms` and `replaces` to `daemon_grants`,
creates the singleton `api_settings` row, and **drops the unused #8 `api_tokens` table**.
That table was never written by any component; any row found is carried over fail-closed as
a revoked grant with no capabilities.

## Scopes (data in `cm_core::policy::scopes`)

Scopes are names for fixed capability sets. #26 adds fine-grained capabilities behind them;
`LibraryRead` stays the #25 read bundle (desktop, `presets::mcp_read_only`) and still
satisfies every read. `Requirement::AnyOf` expresses "the bundle, or these fine ones".

| Scope | Capabilities | Covers |
|---|---|---|
| `papers:read` | MetadataRead | papers, collections/tags, queries, attachment lists, extraction status |
| `papers:write` | LibraryWrite, NetworkLookup | create/update papers, organization, import preview/commit, DOI lookup |
| `papers:delete` | LibraryDelete | trash/restore, delete collections/tags |
| `files:read` | ContentRead | extracted page text, rendered pages |
| `annotations:read` | AnnotationsRead | highlights/comments/notes |
| `annotations:write` | AnnotationsWrite | create/edit/delete annotations (now required for everyone, owner included) |
| `exports:write` | ExportAllowedDir | export only into allowed export folders |
| `jobs:read` / `jobs:write` | JobsRead / JobsControl | the token's own jobs |
| `bulk:elevated` | BulkElevated | **not issuable**; only during a time-limited elevation |

Never reachable by scopes: `GrantAdmin`, `UiSettings`, `LocalFileRead`, `LocalFileWrite`,
`LocalFileReplace` (#20), `CapturePreview`, `DestinationRead`, `LibraryRead`. The native-host preset is unchanged.

`ENDPOINTS` maps every REST endpoint (DESIGN §13.2), MCP tool (§14.2) and CLI command (§15)
to its operations and required scopes (`Access::Scoped`), `Public` (`GET /health`) or
`OwnerOnly` (local-file reads, e.g. `papers add file`, `papers import`). Summary:

- `papers:read`: `GET /papers`, `GET /papers/{id}`, `GET /collections`, `GET /tags`,
  `GET /papers/{id}/fulltext/status`; MCP `search_papers`, `get_paper`; CLI list/show.
- `papers:read + files:read + annotations:read`: `GET /search`, MCP `search_fulltext`,
  `papers search` (snippets carry page and annotation text).
- `files:read`: `GET /papers/{id}/fulltext`, `GET /attachments/{id}/pages/{page}`,
  `get_paper_fulltext`, `papers fulltext`.
- `annotations:read` / `annotations:write`: annotation list / create, patch, delete;
  `get_paper_annotations` / `add_annotation`.
- `papers:write`: `POST/PATCH /papers`, `POST /papers/resolve-doi`, `POST /imports`,
  collection/tag create/update/membership; `add_paper_by_doi`, `add_paper_from_url`,
  `update_paper`, `create_collection`, `move_paper_to_collection`, `add_tag_to_paper`.
- `papers:delete`: `DELETE /papers/{id}`, restore, `DELETE /collections|tags/{id}`.
- `papers:delete + bulk:elevated`: `POST /papers/bulk-delete`;
  `papers:write + bulk:elevated`: `POST /collections/{id}/papers/bulk-move`.
- `papers:read + exports:write`: `POST /exports`, `export_citations`, `papers export`.
- `jobs:read` / `jobs:write`: `GET /jobs/{id}` / `POST /jobs`, `POST /jobs/{id}/cancel`,
  `POST /jobs/{id}/retry` (each job kind also needs its own capabilities).

Core tests check, for **every one of the 1024 scope combinations** and every table row, that
the shared authorizer's decision equals "required scopes ⊆ held scopes", that each listed
scope is necessary, that `OwnerOnly` rows are unreachable, and that the MCP read-only
profile allows exactly what the #25 `mcp_read_only` preset allows.

## Token lifecycle

- Issue (`ApiCommand::IssueToken`, GrantAdmin only): kind `api`/`mcp`/`cli` (never
  `native-host`/`desktop`), at least one issuable scope, always an expiry: default 90 days
  (max 365) for read-only scopes, default 30 (max 90) when any write scope is present.
  The capabilities must be a subset of the issuing session (`may_issue`).
- Secret: 256-bit random hex, returned once. The database stores SHA-256 only. A slow KDF
  is unnecessary for uniformly random 256-bit secrets and would only add latency; the
  lookup is keyed by the digest and confirmed with a constant-time comparison.
- Re-issue: new secret, same label/kind/scopes and lifetime length (clamped to the current
  maximum), `replaces` links the old grant, which is revoked (its jobs stop) in the same step.
- Scope change (`SetScopes`): in place, token unchanged; widening is bounded by the owner's
  session; the expiry is pulled in to the new maximum, never extended.
- Revoke: live sessions are refused at their next request, new handshakes fail, owned jobs
  fail ("owner grant revoked") and the stored secret is deleted. The owner's
  `desktop-bootstrap` grant cannot be revoked through `ApiCommand` (it rotates when its
  credential file is removed, #25).
- Last use: minute granularity in memory, written to the database when the daemon exits
  (every committed write invalidates open import previews, so connecting must not write).
- Bootstrap upgrade: an existing owner grant missing capabilities added later (e.g. #26's)
  receives them in place at daemon start; credential and owned jobs are kept.

## MCP profiles and elevation

MCP profiles are `mcp` grants created **read-only** (`profiles::mcp_read_only`:
papers/files/annotations read, jobs read). "Allow writes" (`profiles::mcp_write`: adds
`papers:write` and `annotations:write`) is a separate, confirmed owner action with the
warning `profiles::MCP_WRITE_WARNING`; delete and export stay separate scopes.

Bulk operations: the daemon only has per-item commands, so a client could split a bulk
delete/move. The daemon's `BulkGuard` (`policy::elevation`) therefore counts per grant, in a
60 s sliding window on the **monotonic** clock, `BulkKind::Delete` (paper trash/restore,
collection/tag delete, annotation delete; limit 10) and `BulkKind::Move` (collection/tag
membership changes; limit 25), for every transport. Beyond the limit the request is decided
(and audited) as `BulkTrash` / `BulkMove`. The owner's bootstrap grant is exempt (it holds
`BulkElevated` permanently; `GrantCommand::Issue` refuses it). Any other grant is exempt
only through `ApiCommand::Elevate { kind, .. }`: owner-only (`GrantElevate` needs
`GrantAdmin + ElevationAdmin + BulkElevated`), **one kind per elevation**, at most 15
minutes, never past the token expiry, only for tokens holding that kind's base scope
(`papers:delete`/`annotations:write` for delete, `papers:write` for move), and session
narrowing applies. The deadline is a monotonic `Instant` in the daemon; the stored
wall-clock end is display only, is cleared at daemon start, and elevations end with the
daemon. `EndElevation` stops it immediately. A token holding both base scopes can be
elevated for either kind, so the settings screen lets the owner choose the kind (offering
only `policy::elevation::elevatable_kinds` of the token) and sends exactly that choice.

### Owner-only capabilities

`GrantAdmin`, `ApiAdmin` (API settings write), `ElevationAdmin` and `BulkElevated`
(`Capability::OWNER_ONLY`) are held only by the desktop bootstrap grant. No scope maps to
them, and `GrantCommand::Issue` refuses them, so a delegate can never mint grants, change
the API exposure or approve elevations. Every delegated grant also expires
(`scopes::lifetime_for_capabilities`, same limits as scoped tokens; omitted expiry = the
default); only the bootstrap grant lives until revoked.

## API settings (listener: [rest-api](rest-api.md), #27)

`ApiSettings { enabled = false, bind = 127.0.0.1 | ::1, port = 39271 (1024..=65535),
export_dirs }` in the singleton `api_settings` row; the table CHECKs repeat the loopback and
port rules. Export folders must be absolute, existing and reached without symlinks, and may
not be `$HOME`, the library, anything inside it (its private state included) or an
ancestor of it. `exports:write` tokens may only create files inside them: the path must be
lexically inside a folder, the parent chain must contain that folder by device and inode,
and the extension must match the format (`.bib`/`.ris`); on top of the #12 writer's
no-symlink/create-new rules. Every export (owner included) is capped at
`MAX_EXPORT_RECORDS` (50 000) and `MAX_EXPORT_BYTES` (64 MiB) before any file is created.
Arbitrary paths stay with the owner's `LocalFileWrite` (GUI save dialog). Saving settings
needs the owner-only `ApiAdmin`. #27 reads `ApiCommand::Overview`/`ApiService::settings` and
must not listen while `enabled` is false. Since #27 `Overview` reports the real
`ListenerState` (`Stopped`/`Listening`) and `SaveSettings` wakes the listener manager.

## Secret store (`cm_protocol::client::secrets`, std-only)

| Platform | Store | Notes |
|---|---|---|
| macOS | login Keychain via `/usr/bin/security -i` (commands on stdin, so the secret is not in argv) | **not verified**; keychain ACL/prompt behavior for `papersd` vs. the GUI is #34 |
| Linux | Secret Service via `/usr/bin/secret-tool` (secret on stdin) when `DBUS_SESSION_BUS_ADDRESS` is set | not exercised here (no session bus) |
| fallback | `$XDG_DATA_HOME` or `~/.local/share` (macOS `~/Library/Application Support`) `/citation-manager/secrets/<library key>/<account>.secret`, 0600 in a 0700 dir **outside the library** (so syncing or copying the library folder does not carry token copies; `<library>/daemon/secrets` only without a home directory) | **weaker**: readable by any same-UID process, copied by home backups; the GUI shows a warning |

Helpers run by absolute path with a 20 s timeout and are always reaped (killed and waited
for on every error path). Clients give commands that wait on the store (issue: write plus
Keychain read-back; re-issue: also deletes secrets; revoke) at least the whole helper
sequence as their deadline (`ApiCommand::secret_store_steps`, `ClientTimeouts::for_command`),
so a slow or locked keyring cannot make the client discard the reply carrying the new
secret while the daemon completes the action. The fallback reads with `lstat` + open + `fstat` and refuses a
symlink, a non-0600/multi-link file or an inode swap between the calls (std has no
`O_NOFOLLOW`); writes go through a `create_new` temp file and rename. A failed OS write
falls back to the file. At daemon start, stored copies of revoked or expired tokens are
deleted in the background. Revocation order is the same on both paths (`ApiCommand::Revoke`
and `GrantCommand::Revoke`): live registry, then the grant's jobs, then the stored secret.
A re-issue whose old token cannot be revoked revokes the new token too.
`CM_SECRET_STORE=file` forces the fallback (headless/CI). Entries are keyed
`SECRET_SERVICE = "citation-manager"`, account `"<library key>:<grant id>"`
(`DaemonPaths::library_key`). No crate dependency was added. Tests use `MemorySecretStore`.
The owner's desktop credential stays the #25 0600 file.

## Audit log

Two sinks under `<library>/daemon/`, each 0600, opened with `O_NOFOLLOW`, rotated to `.1`:
`audit-admin.jsonl` (256 KiB) for grant issue/revoke/scope change, elevation, API settings
and bulk decisions, and `audit.jsonl` (512 KiB) for everything else. Ordinary denials never
reach the admin sink, so they cannot evict who changed what. Identical denials (grant,
operation, reason) within 60 s are written once and then counted: the count is written as
one record (`count`) when the window ends, before the tail is read, and at daemon exit.
Records: time, grant id, issued kind, operation, outcome, fixed reason code, target grant,
count. Never content, queries, paths, labels or secrets. Recorded: every denial and every
non-read operation; reads and UI preference saves are not. A sink file that is unsafe
(symlink, extra links, group/other bits) is moved aside to `*.unsafe-<ms>`, a fresh file is
started and the problem is reported as `ApiOverview::audit_warning`; a write failure is
reported the same way instead of being dropped silently. Files, not the library database,
for the preview-invalidation reason above. Readable by the owner through `ApiCommand::Audit`.

## CLI bootstrap (for #28)

`papersd token issue|list|revoke|elevate` and `papersd api show|set` connect (or start the
daemon) with the owner's desktop credential and send the same `Command::Api`. A new token
is printed exactly once on stdout; when stdout is not a terminal the command refuses
**before issuing** (exit 3) unless `--allow-non-tty` is given. If writing the secret fails
(closed pipe, full disk) nobody received it: the token is revoked and the command exits 1.
Labels are stripped of control characters on input and output. Diagnostics go to stderr.
#28 may wrap these or send `Command::Api` itself with an owner credential.

## Entry points for #27/#28/#29

- Scope table: `cm_core::policy::scopes::{ENDPOINTS, endpoint, Scope, profiles}` (also
  re-exported as `cm_protocol::api::*`). Add rows there; the core tests then cover them.
- Token verification: out of process, present the token as `Hello::credential` (IPC
  handshake); in process (#27 inside papersd), `Authenticator::authenticate` then
  `Authenticator::authorize(grant, None, operation, now)` per request. The extra checks
  (bulk guard, export folders) apply too. Done in #27: `ipc::admit`/`ipc::guard` are shared
  free functions and REST dispatches through `Session::dispatch` (`ipc::gateway`), see
  [rest-api](rest-api.md). `POST /imports` also lists `ImportCancel` (same scope).
- Settings: `ApiCommand::Overview` / `SaveSettings`; in the daemon `ApiService::settings()`.
- Secrets: `PlatformSecretStore::detect(paths.secrets_dir(), uid).get(&account(...))` lets
  `papers mcp` load its profile token without a config-file secret.

## GUI secret handling

The settings screen keeps a new secret in a non-`Clone` wrapper that is overwritten on drop,
shows it masked until "Reveal", and builds the plaintext string only while revealed. Done,
leaving the screen or issuing another token drops it and clears the clipboard if the
clipboard still holds that secret; a copied secret is also cleared after 30 s. List replies
carry a request sequence number (stale ones are ignored) and a refresh never overwrites an
unsaved settings draft. An `Issued` reply that arrives after the person left the screen is
never shown: that token is revoked at once (nobody received its secret).

## Not verified / limits

macOS Keychain and Linux Secret Service backends (code only; file fallback and memory store
tested); keychain ACL prompts between `papersd`, the GUI and `papers`. Same-UID processes
can still read the fallback file and the desktop credential (security ADR). The bulk guard
is a rate limit, not a transaction: up to 10 deletes/25 moves per minute succeed without
elevation. Last-use timestamps are lost if the daemon crashes. The GUI wrapper cannot
zeroize copies GPUI or the OS clipboard make while a secret is revealed or copied.
