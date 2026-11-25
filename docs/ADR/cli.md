# #28: `papers` CLI over papersd IPC

Status: adopted for M4 on Linux/Unix (subprocess-tested on Linux). macOS `.app` placement of
`papers` next to `papersd` is #34. MCP tools (#29), REST (#27) and the CLI token flow (#26)
are separate issues (all merged); this ADR records the seams they plug into.

## Shape

`crates/cli` (package `cm-cli`, binary `papers`) is a clap-derive adapter over the #25
protocol. It depends only on `cm-protocol` and `cm-mcp` (architecture check), plus clap,
clap_complete (=4.6.11, no default features), serde/serde_json and rustix (`process`: EUID and
`server stop`). It never opens SQLite, the attachment store or a parser: every read and write is
a daemon `Command`, authorized against the daemon's stored grant.

| Module | Role |
|---|---|
| `args` | clap grammar, `--help` examples |
| `connect` | `Target` (library, runtime dir, lazy start), `Connection` (handshake + per-request timeouts), error mapping |
| `credentials` | **the only credential/capability module** (see below) |
| `decide` | duplicate/merge/skip policy over a #33 preview row (pure, unit-tested) |
| `paths` | input path resolution, export target checks, private scratch + atomic publish |
| `commands::*` | one module per subcommand family |
| `output` | `papers.v1` envelope, exit codes |

Library selection: `--library DIR` or `PAPERS_LIBRARY` is required (no implicit default user
library, matching the GUI). `--runtime-dir`/`PAPERS_RUNTIME_DIR` overrides the socket directory.

## Lazy start

`Connection::open` uses `LocalConnector` with `Launch { papersd: <dir of current_exe>/papersd }`
(never PATH), 15 s start timeout, optional `--daemon-idle-timeout-ms`. `--no-start` and
`server status`/`stop` never start a daemon. One retry on "closed during handshake" covers the
idle-exit race, as in `connect_and_handshake` (a typed `HandshakeFailure::Closed`: any
transport error after connect except a timeout). Per-request socket timeouts: 30 s control,
5 min interactive, 60 min long (import/export/PDF); on expiry a best-effort `Cancel` is sent and
the command exits 6 saying the result is unknown (check with `papers show`/`papers list`).

## Credentials and capabilities (`credentials.rs`)

- Default: `CredentialSource::DesktopBootstrap` (the owner's `<library>/daemon/desktop.credential`),
  presented with `client_kind = "cli"` (diagnostic only).
- `--credential-file FILE` / `PAPERS_CREDENTIAL_FILE`: an explicitly issued credential
  (`GrantCommand::Issue`). Opened with `O_NOFOLLOW` and validated by `fstat` on the open
  descriptor: regular, single link, owned by the user, `mode & 0o177 == 0` (0600 or 0400), at
  most 256 bytes, in a parent directory owned by the user and not group/other-writable.
- Every session sends `Hello.requested` = the minimum for the subcommand (`Need`):

| Need | Commands | Requested |
|---|---|---|
| Status | server status/start | (none) |
| Admin | server stop | GrantAdmin (proven by the daemon answering `GrantCommand::List`) |
| Read | list, show, search, fulltext, annotations | LibraryRead |
| LookupPreview | add doi --dry-run | NetworkLookup, LibraryRead, CapturePreview |
| LookupWrite | add doi | NetworkLookup, LibraryRead, LibraryWrite |
| FilePreview | import --dry-run | LocalFileRead, LibraryRead, CapturePreview |
| FilePreview | add file --dry-run (new paper) | LocalFileRead, LibraryRead, CapturePreview |
| FileWrite | import, add file (new; attach) | LocalFileRead, LibraryRead, LibraryWrite |
| FileInspect | add file --attach-to --dry-run | LocalFileRead, LibraryRead |
| Export | export | LibraryRead, LocalFileWrite |

`--read-only` replaces any request with `{LibraryRead}`; the daemon then refuses writes
(`Denied(Forbidden)` → exit 5). The CLI never pre-judges authorization.

**#26 hook:** add a `CliCredential` variant (e.g. a token from the CLI bootstrap/pairing flow or a
keychain entry) and map it in `CliCredential::source`; `Need` stays the narrowing policy.

## Output contract

stdout carries exactly one JSON document per invocation:

```json
{"schema":"papers.v1","command":"add doi","ok":false,
 "data":{...partial result, e.g. the preview...},"warnings":["..."],
 "error":{"code":"duplicate","exit_code":4,"message":"..."}}
```

`data` and `error` are present when applicable; keys are additive within `papers.v1`. `--text`
prints human lines instead. Diagnostics (warnings, errors) always go to stderr as
`papers: warning: …` / `papers: error: …`. Exceptions: `export` without `-o` writes the raw
bibliography to stdout (summary on stderr); `completions` writes the script; `mcp` never
writes to stdout (reserved for the MCP protocol); clap usage errors print to stderr only.

Writing stdout: if the reader closed the pipe (`EPIPE`, e.g. `papers list | head`), output is
abandoned quietly and the command keeps its own exit code (0 on success; a closed pipe never
turns a failure into success or the reverse). Any other stdout write error exits 1 with a
stderr line. The same rule applies to `export` to stdout and `completions`.

`--text` output and all stderr diagnostics strip control characters (ESC, CR, BEL, bidi
overrides; newlines are kept only in multi-line text such as page text), since titles,
snippets and daemon messages are untrusted. JSON is escaped by serde.

Paths: the IPC is JSON, so input/output paths that are not UTF-8 are refused up front (exit 2).
Paths placed in JSON output use a lossy string (U+FFFD for invalid bytes), never a panic.

| Exit | Code(s) | Meaning |
|---|---|---|
| 0 | — | success (also dry runs and `--on-duplicate skip` with nothing saved) |
| 1 | `internal` | unexpected failure, protocol error, cancelled request, PDF attach failure after a saved paper |
| 2 | `usage`, `invalid_input` | bad arguments, or input rejected by the CLI or the daemon (`InvalidInput`, invalid search DSL, too large) |
| 3 | `not_found` | paper/attachment/file/collection/tag not found |
| 4 | `conflict`, `duplicate`, `confirmation_required`, `unresolved` | stale/conflicting state, duplicate candidate needing a decision, write without `--yes` when non-interactive, metadata needing input |
| 5 | `permission_denied` | grant scope refusal, unknown/revoked/expired credential, unsafe credential file or endpoint, export into the library |
| 6 | `daemon_unavailable` | papersd missing/not starting/not running with `--no-start`, timeouts, `Unavailable`/`Busy` |
| 7 | `feature_unavailable` | `server start --rest` (REST follows the API settings), MCP server failure |

Limits: `search --limit` 1–100 (default 20), `list --limit` 1–500 (default 50, keyset
`--cursor`), `fulltext` `--page N` / `--pages A-B` (1-based, ≤50 pages) and `--max-chars`
1–400000 (default 20000), `annotations --limit` 1–1000 (default 200, `truncated` flag),
`import --batch` 1–100. Pages are reported as 0-based `page_index` plus 1-based `page_number`
and `paper://<paper>/attachment/<attachment>?page=N` URIs.

## Write discipline

- `add doi`: `Resolve` → (blank title → exit 4 `unresolved`, `--title` completes it) → common
  `Import::Preview` → `decide` → commit only with `--yes` or an interactive "y".
- `add file`: `Pdf::Inspect` (daemon hashes and checks the file) → new paper: `Pdf::PreviewNew`
  with explicit `--title/--doi` first, then PDF metadata, else the provisional filename title
  (#33 `propose`); unresolved items are listed in `data.unresolved` and as warnings →
  `Pdf::CommitNew` (paper + attachment through the shared ImportService/attachment store).
  `--attach-to <id>`: `Pdf::Attach` only; metadata is never changed by the file.
- `import`: `Parse` → review *every* batch (`PreviewFile` + cancel) → if any row needs a
  decision, nothing is committed (exit 4) → confirmation → per batch a fresh preview with the
  same policy and `Commit`. Invalid rows (shared normalizer errors) are skipped and reported.
  The review carries the exact identifiers (DOI and external IDs as normalized in the preview)
  of rows that will be created into later batches, so a repeat in another batch is a duplicate
  before anything is committed. Citation keys are not identifiers: the service suffixes
  colliding keys. Later batches still re-check against the library; a change made by someone
  else meanwhile stops with exit 4 and reports earlier saved batches.
  Interrupting (Ctrl-C) between batches leaves earlier batches saved; each batch is atomic.
- Duplicates: exact DOI/arXiv/PMID matches can only be skipped (`--on-duplicate skip`) or merged
  (`--merge <id>`); approximate matches may also `--on-duplicate create`. A merge needs the
  explicit target among the reviewed candidates and `--merge-fields none|all|f1,f2` (default
  none: keep metadata). A trashed target is refused (exit 4): the service never merges into the
  trash and merging never restores. Bulk import never merges. Non-interactive runs never merge
  on their own.
- `add file --dry-run` builds the same #33 proposal locally and uses the common import preview
  (CapturePreview), so it never asks for LibraryWrite.
- `--dry-run` previews and cancels the daemon's preview state; nothing is saved.

## Export path safety

The daemon writes (create-new, 0600, no symlink ancestors) into a CLI-created 0700 scratch
directory: in the destination directory for `-o`, else in the temp directory for stdout. The
CLI publishes with `hard_link` (atomic, never clobbers) or, with `--force`, `rename` over an
existing *regular* file, then fsyncs the directory. Before publishing, the file is given the
mode a new file gets under the current umask (observed with a probe file; no umask change) and
fsynced. Refused: a destination directory that is, or lies inside, the library directory,
compared by `(dev, inode)` of the destination and every ancestor (exit 5; an uninspectable
library is an error, not a pass), an existing file without `--force`, a symlink or non-regular
destination (exit 4). The scratch directory is removed in all cases. Because papersd always
writes a file (LocalFileWrite), `--read-only` also blocks export to stdout.

## server / mcp

`server status` probes without starting; `start` lazily starts and reports `started`. `stop`:
refused under `--read-only`; requests GrantAdmin and proves it by a daemon-authorized
`GrantCommand::List` (read-only or restricted grants exit 5); refuses while other clients or
active jobs exist unless `--force`; signals SIGTERM only when the `SO_PEERCRED` PID of the
authenticated socket equals the PID reported by `Status` (non-Linux: refused with exit 7 until
peer PIDs are verified there, #34); then waits until the socket is gone or refuses connections
(other probe errors are "not yet stopped"). There is no daemon-side graceful `Shutdown` command
yet: SIGTERM ends papersd immediately and its crash recovery (#25/#51, job lease reclaim)
covers in-flight work. The REST listener (#27) is not a start option: papersd binds it from the
persisted API settings (API & integrations, or `papersd api set --enable`), so `--rest` exits 7
with that hint.

`papers mcp` serves MCP (#29) for the library the CLI resolved: it builds its `Target`
(`--library`/`PAPERS_LIBRARY`, `--runtime-dir`, `--credential-file`, `--no-start`, idle timeout)
and calls `cm_mcp::run_stdio_with(StdioConfig::from_connector(..))`. The session is read-only;
the owner's desktop credential is never presented (without `--credential-file` the default MCP
credential file is used). stdout carries MCP frames only, so `main` passes an unlocked `Stdout`
(a lock held for the whole run would deadlock the server's writer thread). No library: exit 2;
the server fails to start: exit 7. Grants are managed with `papers-mcp grant|revoke|grants`.
MVP final acceptance still requires #29/#30 integration.

## Completions

`papers completions bash|zsh|fish` prints clap_complete output; checked-in copies live in
`crates/cli/completions/` (`papers.bash`, `_papers`, `papers.fish`) and a unit test fails when
they drift from the grammar. Install e.g. `papers completions zsh > ~/.zfunc/_papers`.
