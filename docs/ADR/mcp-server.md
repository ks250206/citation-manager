# #29: Read-only-by-default MCP stdio server

Status: adopted for M4 on Linux/Unix. Implements DESIGN §3.2/§14 on the #25 daemon
([daemon-ipc](daemon-ipc.md), [security](security.md)). Credential profiles, token storage and
elevation UX are #26; the `papers` CLI wrapper is #28.

## Shape

`crates/mcp` (`cm-mcp`, depends only on `cm-protocol`) is a thin adapter: AI client ⇄ stdio
JSON-RPC (rmcp) ⇄ `papers-mcp` ⇄ authenticated Unix-socket IPC ⇄ `papersd`. It never opens the
library, storage or a REST listener; there is none. Every tool is a composition of existing
`Command`s, each authorized by papersd against the **current** grant of the presented
credential, so grants, revocations and expiry apply identically to MCP, the GUI and direct IPC
(tested: the same command over IPC with the MCP credential gets the same `Forbidden(op)`).

## MCP SDK: rmcp 3.9.0 (exact pin)

DESIGN §4/§14.1 choose the official Rust SDK and `Cargo.toml` already pinned
`rmcp = "=3.9.0"` (`server`, `transport-io`, no default features; see
[DEPENDENCIES](../validation/DEPENDENCIES.md)). It is used without macros: tools and their JSON
Schemas are hand-written. Its async stack (tokio, tokio-util, futures, schemars) lives only in
`cm-mcp`; `domain/core/protocol` stay framework-free and `scripts/check_architecture.py` passes
unchanged. `tokio-util = "=0.7.20"` is added (exact, the version rmcp resolves) only to name
rmcp's `CancellationToken`. Lock additions: rmcp, tokio-util, and sha2 0.11 with its digest
stack (an rmcp dependency next to the existing sha2 0.10.9). A hand-written JSON-RPC server was
not needed.

One deviation from stock rmcp: its stdio transport silently drops unparsable lines. `transport.rs`
replaces it (same rmcp `Transport` trait) to answer Parse error (-32700, id null) and Invalid
Request (-32600, with the id when present), discard lines over 4 MiB, and continue. All stdout
writes go through one writer task, so stdout carries complete protocol frames only; diagnostics
go to stderr (`PAPERS_MCP_LOG`), never secrets or library text.

## Entry point (contract for `papers mcp`, #28)

- Binary `papers-mcp`, installed next to `papersd` (which it starts on demand from its own
  directory, never via PATH). `papers mcp ARGS` must call `cm_mcp::main(ARGS)` (returns the exit
  code: 0 ok, 1 failure, 2 usage) — identical behaviour; `cm_mcp::run_stdio()` (environment-only
  configuration) and `cm_mcp::serve(ServeConfig)` remain for embedding.
- `papers-mcp [serve] --library ABS [--runtime-dir ABS] [--credential-file ABS] [--read-only]
  [--papersd ABS] [--daemon-idle-timeout-ms N] [--idle-disconnect-ms N]`
- Environment: `PAPERS_LIBRARY`, `PAPERS_RUNTIME_DIR`, `PAPERS_MCP_CREDENTIAL_FILE` (a path),
  `PAPERS_MCP_LOG`. Exits 0 on stdin EOF.
- Owner subcommands: `grant [--write] [--export]`, `revoke [--grant UUID]`, `grants`.

## Credentials (interim; `credential.rs` is what #26 replaces)

- Hello: `client_kind = "mcp"`, `requested` = the MCP ceiling {LibraryRead, JobsRead,
  LibraryWrite, NetworkLookup, LocalFileWrite} (`--read-only`: only the first two). Requested
  scopes only narrow: even a misplaced owner credential can never delete, administer grants,
  read local paths, change settings or control jobs through MCP.
- The secret is read from a private file (0600, single link, owned by the user, ≤ 256 bytes):
  `--credential-file`, else `$PAPERS_MCP_CREDENTIAL_FILE`, else `<library>/daemon/mcp.credential`.
  Never from argv (`--credential/--token` are rejected) or an environment value. The file is
  re-read before each call: replacing it reconnects with the new grant without restart.
- Dev/bootstrap path (like #23's `pair`): `papers-mcp grant` opens a session with the owner's
  desktop bootstrap credential (GrantAdmin) and issues `mcp read-only` = `presets::mcp_read_only`
  (LibraryRead, JobsRead). `--write` adds LibraryWrite + NetworkLookup; `--export` adds
  LocalFileWrite. The secret is written atomically; the grant the file held before is revoked.
- #26 plugs in by implementing `credential::CredentialStore` (keychain/profile-backed load) and
  replacing `issue/revoke/list` with its consent UI and profiles; the daemon side is unchanged.

## Tools (DESIGN §14.2, exact names)

| Tool | IPC commands | Needs |
|---|---|---|
| search_papers | Search (+ Library Get per hit for authors/year) | read |
| get_paper | Library Get, Pdf List, Fulltext Status, Organization ForPaper | read |
| get_paper_fulltext | Library Get, Fulltext Status/Pages | read |
| get_paper_annotations | Library Get, Annotations ForPaper | read |
| add_paper_by_doi | Resolve, Import Preview/Commit | write (NetworkLookup + LibraryWrite) |
| add_paper_from_url | (Resolve if a DOI is given/in the URL), Import Preview/Commit | write |
| update_paper | Library Get/Update | write |
| create_collection | Organization List/CreateCollection | write |
| move_paper_to_collection | Organization ForPaper/CollectionMember | write |
| add_tag_to_paper | Organization List/CreateTag/TagMember | write |
| add_annotation | Pdf List, Fulltext Pages (stored label), Annotations Create | write |
| export_citations | Export into a private 0700 temp dir, read back | export (LibraryRead + LocalFileWrite) |

Common contract:

- Inputs are validated against the published `inputSchema` (closed objects, count limits, page
  ranges, character caps; one validator, so the advertised schema is what is enforced) before
  any daemon call. Violations, unknown papers, denials and daemon failures are tool errors
  (`isError: true`, `structuredContent.error.kind`); only an unknown tool name is a protocol
  error (-32602).
- Every item carries `source` {paper_id, doi, title, authors, year, attachment_id, page_index,
  page_label, uri} next to its `text`. Locations come from stored data (search hit, extracted
  page, annotation anchor); page labels are never guessed.
- Internal URIs: `paper://<paper>[/attachment/<attachment>][?page=<page_index+1>][&annotation=<id>]`
  (`page` is the 1-based physical page, not the printed label).
- Bounds: search limit 1–50, snippet 32–1000; full text page_count 1–20, max_chars ≤ 100 000
  (daemon caps 50/400 000); annotations limit ≤ 500, max_chars ≤ 200 000; export 1–200 IDs,
  ≤ 1 000 000 chars; one result ≤ 4 MiB.
- Imports follow #33: preview first; `confirm: true` commits only a preview without any possible
  duplicate (exact or fuzzy) or error; otherwise the preview (duplicates, differences) is
  returned and `preview_id` + `decision` (create/merge/skip) commits. Previews are grant-bound and
  invalidated by library writes (`conflict` → request a new preview). The server does not fetch
  web pages (SSRF surface stays in the daemon's DOI adapter): URLs without a DOI need
  caller-supplied metadata.
- Annotations are always `source=ai` (not caller-selectable) and unconfirmed until the owner
  confirms them; positions without rects become unresolved text anchors for the owner to place.
- `Forbidden(op)` names the operation, the capabilities it needs and the owner command
  (`--write`/`--export`); `Revoked`/`Expired` say a new grant is needed. Delete/trash and bulk
  operations have no tool and LibraryDelete is never requested (elevation is out of scope).

## Lifecycle, cancellation, restart

One multiplexed connection (reader thread routes replies by id). papersd is started on demand
(`LocalConnector`); the connection is closed after 60 s without calls so papersd can idle-exit.
`notifications/cancelled` cancels the call: rmcp suppresses the response and the client sends
`ClientFrame::Cancel{id}` and drops the late reply. Note papersd answers Search/Fulltext on the
connection thread, so their cancel only frees the MCP side. A lost daemon fails waiting calls;
the next call reconnects/restarts. A request that could not be written, and an effect-free read
whose connection dropped before the reply, are retried once; writes are never re-sent
(`daemon_restarted` asks the caller to check first). Client timeouts: 30 s search/full text,
3 min library work, 10 min import/export.

## Verification and limits

Automated (Linux): unit tests and `crates/mcp/tests/stdio_server.rs`, which drives the real
`papers-mcp` binary over stdio against a real lazily started `papersd` on a temp library:
initialize/tools/list/search/fulltext/annotations, refusal of all 8 write tools under the
read-only grant and identical refusal over direct IPC, write after `grant --write` in the live
server, duplicate confirmation, export grant, `--read-only`, live revocation, malformed JSON,
bounds of every tool, cancellation (SIGSTOPped papersd) and restart (SIGKILL, also mid-read).
That papersd has no PDF runtime, so page text is only checked as `pending`; a manual run with
the pinned PDFium (`pdf-metadata` papersd) returned extracted pages with labels `iv`/`S12`.
Not verified: macOS, a third-party MCP client application, real DOI provider lookups through
the tool, and `papers mcp` (#28 not landed).
