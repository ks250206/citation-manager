# #27: authenticated local REST API and its OpenAPI contract

Status: adopted on Linux. Builds on [daemon-ipc](daemon-ipc.md) (#25),
[api-tokens-grants](api-tokens-grants.md) (#26) and the [security ADR](security.md).
Contract: [`docs/openapi.json`](../openapi.json). Evidence: [ISSUE_27](../validation/ISSUE_27.md).

## Process model

The listener lives **inside papersd** (`crates/daemon/src/rest/`), so it reuses the single
library writer, the job engine and the search readers. papersd is not async: a manager
thread (`rest::Manager`) watches the persisted `ApiSettings` (`ApiService::settings`, woken
on every `SaveSettings`) and starts, moves or stops a listener. A listener owns a small
multi-thread tokio runtime (2 workers, blocking pool = in-flight limit + 4) on its own
threads. Library work never runs on the async workers: each request runs its commands on a
blocking thread through `ipc::gateway::InternalSession`, which calls the **same
`Session::dispatch` as an IPC connection** and waits for the reply.

- Off by default; nothing is bound while `enabled` is false. Binds only the configured
  `127.0.0.1` or `::1` and port (1024–65535, #26 validation + a loopback re-check).
- Settings changes apply at runtime (start, port/address move, stop); a port in use is
  retried every second and reported as `ListenerState::Stopped` (GUI hint), logged once.
- REST clients cannot start papersd, so an enabled listener **pins** the daemon
  (`Activity::pin`): no idle exit while it listens. Disabling releases it. Shutdown stops
  the listener before the IPC connections are drained.

## One principal, one decision (no transport bypass)

Per request (the `Ctx` extractor, the first argument of every handler, so nothing is parsed
before it passes):

1. `Authorization: Bearer <token>` (exactly one header, ≤512 visible ASCII). Missing,
   malformed, unknown, revoked and expired tokens are all `401 unauthenticated`
   (`Authenticator::authenticate`, the IPC handshake check). Every request re-authenticates,
   so revocation and narrowing apply to the next request of a live keep-alive connection.
2. Only scoped token kinds (`api`, `mcp`, `cli`) are accepted over HTTP. The owner's
   desktop credential (GrantAdmin, arbitrary local paths, permanent bulk elevation) and
   native-host grants get `403 token_not_allowed`. This only narrows what IPC allows.
3. Every operation of the endpoint's `ENDPOINTS` row is authorized up front
   (`Gateway::preauthorize` → `Authenticator::authorize`, denials audited), so a
   multi-command endpoint never half-runs. `403 forbidden` lists `required_scopes`; bulk
   rows need the owner's permanent `BulkElevated` or a running elevation of exactly their
   kind (#26 per-kind elevation: `bulk-delete` = Delete, `bulk-move` = Move), otherwise
   `403 elevation_required`.
4. Each command then goes through `Session::dispatch`, which calls `ipc::admit`: the live
   grant decision again plus the #26 guards (`ipc::guard`: bulk burst limit, export
   folders), now one shared function for IPC and REST. Preview grant binding, duplicate
   redaction without read scopes and the audit log are therefore identical.

The table rows are data for the adapter and the contract tests; authority stays the daemon.

## Never more than the token may read

- `GET /search` needs `papers:read files:read annotations:read` (snippets carry page and
  annotation text); with fewer scopes it is refused, so lower scopes never receive page or
  annotation text. `GET /papers?keyword=` searches bibliographic fields only.
- Write responses echo stored data only when the token can read it: `PATCH /papers/{id}`
  returns `{id, updated_at}` plus `paper` only with `papers:read`; `PATCH /annotations/{id}`
  returns `annotation` only with `annotations:read`; deletes return 204 or `{id, deleted}`.
  `PATCH /papers/{id}` is a merge-patch applied on the library worker in one step
  (`LibraryRequest::Patch`), so a write-only token needs no read. `PATCH /collections/{id}`
  takes the full name/parent/order for the same reason.
- `POST /imports` dry runs return candidate titles, duplicate ids and kinds only (never the
  stored records). Without a read scope the daemon redacts every preview (any transport):
  matches, the exact-match id, citation-key collision notes and the re-allocated key are
  removed and the warning is one constant text, so a dry run of a duplicate is identical to
  one of a new record. On commit the daemon skips rows the unredacted preview marked as exact
  duplicates (`Create` becomes `Skip`), so such a token never creates a duplicate.
- Annotation provenance: tokens create only `source=ai` annotations (shown as unreviewed
  AI proposals in the GUI); `human`/`import` are refused, and `ConfirmAi` (the person's
  review) and `Undo`/`Redo` (which replay whole client-supplied rows) need the owner's
  GrantAdmin. Enforced in the daemon for every transport.
- `GET /papers/{id}` omits attachment storage keys. Renderer errors use fixed messages.

**Remaining existence oracles** (documented, inherent to a write API): a write-only import
commit reports how many rows were `skipped` and which rows got a paper id, so the token learns
that a skipped row matched an existing DOI/external id (the #22 capture preview states
the same up front as `hidden_duplicates`, only to `CapturePreview` holders, i.e. the native host); creating or renaming to a tag name that already exists is refused (unique tag keys); ids that
exist answer differently from ids that do not (`404` vs success) for every id-addressed
write. None of them returns stored content.

## Endpoints (exactly the `ENDPOINTS` REST rows; base `/api/v1`)

| Method / path | Scopes | Notes |
|---|---|---|
| `GET /health` | none | `{"status":"ok"}` only; Host/Origin checks still apply |
| `GET /papers` | papers:read | filters, sort, `limit` 1..=500 (50), opaque `cursor` (repeat filters) |
| `GET /papers/{id}` | papers:read | paper + collections, tags, attachment summaries |
| `POST /papers` | papers:write | body = `PaperPatch` with `title` |
| `PATCH /papers/{id}` | papers:write | merge-patch (`null` clears) |
| `DELETE /papers/{id}`, `POST /papers/{id}/restore` | papers:delete | trash / restore |
| `POST /papers/bulk-delete` | papers:delete + bulk:elevated | ≤500 ids, per-item results |
| `POST /papers/resolve-doi` | papers:write | DOI → draft preview, nothing saved |
| `GET /search` | papers:read + files:read + annotations:read | §13.3 shape + coverage; limits from #18 |
| `GET /papers/{id}/fulltext/status` | papers:read | extraction state |
| `GET /papers/{id}/fulltext` | files:read | `from_page`, `max_pages` 1..=50 (10), `max_chars` 1..=400000 (100000) |
| `GET /attachments/{id}/pages/{page}` | files:read | RGBA as base64 JSON (unchanged by the #85 binary IPC frames), `zoom` 0.1..=8, at most 4 000 000 pixels |
| `GET /papers/{id}/annotations` | annotations:read | |
| `POST /attachments/{id}/annotations` | annotations:write | `source` is always `ai` (unreviewed in the GUI) |
| `PATCH /annotations/{id}`, `DELETE /annotations/{id}` | annotations:write | color/comment |
| `GET /collections`, `GET /tags` | papers:read | |
| `POST /collections`, `PATCH /collections/{id}`, `POST /collections/{id}/papers`, `POST /tags` | papers:write | |
| `POST /collections/{id}/papers/bulk-move` | papers:write + bulk:elevated | add, optional `from` removal (one transaction per paper: a failed item changes nothing) |
| `DELETE /collections/{id}`, `DELETE /tags/{id}` | papers:delete | |
| `POST /imports` | papers:write | BibTeX/RIS text ≤2 MiB, ≤100 records per call (`start`/`next_start`), `dry_run`; exact duplicates skipped |
| `POST /exports` | papers:read + exports:write | only into an owner-configured export folder |
| `GET /jobs/{id}`, `POST /jobs/{id}/cancel` | jobs:read / jobs:write | the token's own jobs (others: 404) |
| `POST /jobs`, `POST /jobs/{id}/retry` | jobs:write | submit (idempotent on kind + key) / retry the token's own jobs; each kind adds its capabilities (`attachment.import` needs a local-file capability: 403) |

`POST /imports` gained `ImportCancel` in its row (dry runs and failed commits release the
retained preview; same `papers:write` requirement, core tests cover it).

## HTTP defenses and limits

- **Host** must be the bound address — `127.0.0.1:<port>` when bound to 127.0.0.1,
  `[::1]:<port>` when bound to ::1 — or `localhost:<port>` (case-insensitive; the port may
  be omitted only when it is 80) — otherwise `400 host_not_allowed`. Exactly one Host header
  (HTTP/1.0 without Host is refused). An absolute-form target must name the same authority.
  A DNS-rebinding page sends its own host name and is refused before routing, for every path
  including `/health` and unknown routes.
- **Origin**: any request with an `Origin` header is refused (`403 origin_not_allowed`)
  unless it exactly matches a configured origin (`RestConfig::allowed_origins`, papersd
  `--api-allow-origin`, default empty). `Sec-Fetch-Site: cross-site|same-site` without
  an Origin is refused too. **No CORS** by default; configured origins get
  `Access-Control-Allow-Origin` and preflight answers (with
  `Access-Control-Allow-Private-Network: true` for Chrome's Private Network Access), nobody
  else does.
- HTTP/1.1 only (no h2), no TLS (loopback). Limits (`rest::Limits`):

| Limit | Default | Result |
|---|---|---|
| JSON body | 1 MiB | 413 `too_large` |
| `POST /imports` body / text | 5 MiB / 2 MiB | 413 |
| path + query | 16 KiB | 414 `uri_too_long` (431 from hyper for oversized header blocks) |
| header block | 32 KiB read buffer, 64 headers | connection error / 431 |
| header read | 10 s | connection closed |
| request | 30 s (120 s imports, exports, DOI, bulk) | 504 `timeout`; blocking calls stop waiting at the deadline and cancel |
| connections | 64, each closed after 10 min | extra connections closed at accept |
| in-flight requests | 32 | 503 `busy` + `Retry-After: 1` |
| ids per bulk request | 500 | 400 |
| rendered page | 4 000 000 pixels | 400 `too_large` (lower the zoom) |

- Bodies must be `application/json` (415 otherwise) and are read only after
  authentication; unknown fields are rejected (`400 invalid_json`). Query strings reject
  unknown parameters (`400 invalid_input`).
- A panic in a handler or the middleware becomes `500 internal` with the standard body and
  one `panic` log line (no message text); the connection stays usable.
- Bulk endpoints run item by item and stop at a denial (token revoked, elevation ended), the
  deadline or a client disconnect (a shared cancel flag checked before every command); the
  `200` reply lists every id (`not_attempted` after the stop) and `stopped` names the reason.
  Before every item the row's bulk operation is authorized again (`Calls::reauthorize`, the
  same check as step 3), so an elevation that expired or was ended after admission (e.g.
  while the body was uploading) stops the batch with `elevation_required` instead of
  letting items run on the ordinary per-command burst allowance.
- Every error: `{"error":{"code","message","details"?,"request_id"}}`; the server-generated
  request id is also in `X-Request-Id`. Responses carry `Cache-Control: no-store` and
  `X-Content-Type-Options: nosniff`; 401 carries `WWW-Authenticate: Bearer`.
- Log (stderr, or `RestConfig::log`): one line per request with request id, method, route
  **template**, status and duration — never bodies, query strings, Host values or tokens.
  Writes and denials also go to the #26 audit log (ids and fixed codes only).

## Export and paths

`POST /exports` takes a plain `file_name` (no separators, no leading dot, ≤255 bytes) and an
optional `folder` that must equal one of the owner's `ApiSettings::export_dirs` (required
when several exist; none configured → 403). The request always goes through the daemon's
#26 export guard (`ipc::guard` → `export_allowed`): lexical containment plus device/inode
containment of the real parent chain, the extension must match the format (`.bib`/`.ris`,
otherwise 403), the folders themselves may not be `$HOME`, the library or an ancestor of it,
and every export is capped at 50 000 records / 64 MiB. The #12 writer then refuses symlinks
in the parent chain and existing files (`create_new`). No REST endpoint accepts or returns an arbitrary server path; imports take the
text in the body (local-file parsing stays `OwnerOnly`).

## OpenAPI contract

`docs/openapi.json` (OpenAPI 3.1) is generated by `rest::openapi::document()` from the route
table, the `ENDPOINTS` rows (`x-required-scopes`, `x-daemon-operations`) and the limits; a
unit test fails when the committed file differs (`CM_UPDATE_OPENAPI=1 cargo test -p cm-daemon
openapi` regenerates). Contract tests: router routes = OpenAPI operations = `ENDPOINTS` REST
rows in both directions; documented scopes equal the table; every `$ref` resolves; the
integration tests check **every** response's status is documented for its endpoint and its
body validates against the schema. utoipa was not used to avoid a proc-macro dependency and
duplicated annotations.

## Dependencies

`axum 0.8.9` (declared in the workspace since #7; features `http1 json query tokio
matched-path`, defaults off), `hyper 1.12.0` and `hyper-util 0.1.21` (already locked through
reqwest; used directly to set the header-read timeout, header limits and the per-connection
lifetime, which `axum::serve` does not expose), `tokio 1.54.1` (already locked; `net time
sync`). New crates in the lock: `axum-core 0.5.6`, `matchit 0.8.4`, `mime 0.3.18`,
`serde_path_to_error 0.1.20` (axum's own). No tower-http/CORS layer: the edge middleware is
small and its behavior is tested exactly.

## Protocol additions (additive)

`papers::PaperPatch` + `LibraryRequest::Patch` (PaperUpdate), `FulltextRequest::PaperPages`
(FulltextRead; same limits as `Pages`, checked before lookup), `Organization::item` (the
created/updated collection or tag), `ListenerState` now reported (`Stopped`/`Listening`).

The OpenAPI document is canonicalized (object keys sorted) before serialization: GPUI enables
serde_json's `preserve_order` in the desktop lanes through feature unification, and the
committed contract must be byte-identical for every feature set.

## Not verified / limits

- macOS (`.app`, launchd) and the GUI listener hint were not exercised interactively (#34).
- `GET /attachments/{id}/pages/{page}` success needs PDFium; the default test build reports
  `503 pdf_runtime_unavailable` (only that path is tested). `POST /papers/resolve-doi`
  success needs network access; tests cover authorization and input rejection only.
- A timed-out write that already reached the library worker may still complete (the reply
  is discarded); a stopped bulk request keeps the items done before the stop (listed).
- Same-UID processes can read token files and connect to loopback (security ADR); the API
  is for local tools, not a multi-user boundary. HTTP is cleartext on loopback.
- **Local connection exhaustion** is a known limit: any local process (no token needed) can
  hold the 64 connections or the 32 request slots; the header-read timeout and the 10-minute
  connection lifetime bound each connection, but a determined local process can keep the API
  unavailable to other local clients. IPC (GUI, CLI, MCP) is unaffected.
