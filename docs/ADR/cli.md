# #28: `papers` CLI over papersd IPC

Status: adopted for M4 on Linux/Unix (subprocess-tested on Linux). macOS `.app` placement of
`papers` next to `papersd` is #34. MCP tools (#29), REST (#27) and the CLI token flow (#26)
are separate issues; this ADR records the seams they plug into.

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
idle-exit race, as in `connect_and_handshake`. Per-request socket timeouts: 30 s control,
5 min interactive, 60 min long (import/export/PDF); on expiry a best-effort `Cancel` is sent and
the command exits 6.

## Credentials and capabilities (`credentials.rs`)

- Default: `CredentialSource::DesktopBootstrap` (the owner's `<library>/daemon/desktop.credential`),
  presented with `client_kind = "cli"` (diagnostic only).
- `--credential-file FILE` / `PAPERS_CREDENTIAL_FILE`: an explicitly issued credential
  (`GrantCommand::Issue`), read only from a regular, single-link, 0600 file owned by the user.
- Every session sends `Hello.requested` = the minimum for the subcommand (`Need`):

| Need | Commands | Requested |
|---|---|---|
| Status | server status/start/stop | (none) |
| Read | list, show, search, fulltext, annotations | LibraryRead |
| LookupPreview | add doi --dry-run | NetworkLookup, LibraryRead, CapturePreview |
| LookupWrite | add doi | NetworkLookup, LibraryRead, LibraryWrite |
| FilePreview | import --dry-run | LocalFileRead, LibraryRead, CapturePreview |
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

| Exit | Code(s) | Meaning |
|---|---|---|
| 0 | — | success (also dry runs and `--on-duplicate skip` with nothing saved) |
| 1 | `internal` | unexpected failure, protocol error, cancelled request, PDF attach failure after a saved paper |
| 2 | `usage`, `invalid_input` | bad arguments, or input rejected by the CLI or the daemon (`InvalidInput`, invalid search DSL, too large) |
| 3 | `not_found` | paper/attachment/file/collection/tag not found |
| 4 | `conflict`, `duplicate`, `confirmation_required`, `unresolved` | stale/conflicting state, duplicate candidate needing a decision, write without `--yes` when non-interactive, metadata needing input |
| 5 | `permission_denied` | grant scope refusal, unknown/revoked/expired credential, unsafe credential file or endpoint, export into the library |
| 6 | `daemon_unavailable` | papersd missing/not starting/not running with `--no-start`, timeouts, `Unavailable`/`Busy` |
| 7 | `feature_unavailable` | `papers mcp` before #29, `server start --rest` before #27 |

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
  Later batches re-check; a new conflict stops with exit 4 and reports earlier saved batches.
- Duplicates: exact DOI/arXiv/PMID matches can only be skipped (`--on-duplicate skip`) or merged
  (`--merge <id>`); approximate matches may also `--on-duplicate create`. A merge needs the
  explicit target among the reviewed candidates and `--merge-fields none|all|f1,f2` (default
  none: keep metadata). Bulk import never merges. Non-interactive runs never merge on their own.
- `--dry-run` previews and cancels the daemon's preview state; nothing is saved.

## Export path safety

The daemon writes (create-new, 0600, no symlink ancestors) into a CLI-created 0700 scratch
directory: in the destination directory for `-o`, else in the temp directory for stdout. The
CLI publishes with `hard_link` (atomic, never clobbers) or, with `--force`, `rename` over an
existing *regular* file, then fsyncs the directory. Refused: a destination directory inside the
canonical library directory (exit 5), an existing file without `--force`, a symlink or
non-regular destination (exit 4). The scratch directory is removed in all cases.

## server / mcp

`server status` probes without starting; `start` lazily starts and reports `started`; `stop`
sends SIGTERM to the PID reported by an authenticated `Status`, refusing while other clients or
active jobs exist unless `--force`, then waits for the socket to stop answering. papersd's
crash recovery (#25/#51) covers the abrupt stop. There is no REST listener; `--rest` exits 7.

`papers mcp` calls the stable `cm_mcp::run_stdio()` entry point from #7. Until #29 it returns
`StdioError::Unavailable` and the CLI exits 7 with "MCP is not available" on stderr. **#29 must**
implement serving in `cm_mcp` (ideally `run_stdio_with(config)` taking a
`cm_protocol::client::local::LocalConnector` or library/runtime paths, with `mcp_read_only`
requested by default) and the CLI passes its resolved `Target`; until then no library is opened.
MVP final acceptance still requires #29/#30 integration.

## Completions

`papers completions bash|zsh|fish` prints clap_complete output; checked-in copies live in
`crates/cli/completions/` (`papers.bash`, `_papers`, `papers.fish`) and a unit test fails when
they drift from the grammar. Install e.g. `papers completions zsh > ~/.zfunc/_papers`.
