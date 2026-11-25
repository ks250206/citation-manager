# #28 `papers` CLI: verification (2026-11-25)

Linux (container), rustc 1.99.1, branch `claude/issue-28-cli` with
`origin/claude/integration-m2` at c182c7a merged in. Private target directory, `CARGO_INCREMENTAL=0`,
dev/test debuginfo 0, `cargo clean` before the final pass. Contract: [cli ADR](../ADR/cli.md).

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| `cargo test --workspace --locked --no-fail-fast` | passed: 360 tests in 66 suites, 0 failed (cm-cli: 27 unit + 5 subprocess) |
| `python3 scripts/check_architecture.py` | passed (cli depends only on protocol, mcp) |
| `python3 scripts/check_migrations.py --base origin/main` | passed (no migration added) |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | passed: 32 tests |
| manual smoke of `target/debug/papers`, papersd not running beforehand | passed (below) |

## Automated evidence (`crates/cli/tests/cli.rs`)

Real `papers` subprocesses, isolated temp library, private runtime directory under /tmp,
stdin closed (non-interactive). papersd is started lazily by the CLI from `target/debug/papersd`
and stopped at the end of each test.

- Lifecycle: `server status` reports not running without starting it; the first read starts
  papersd; `server start` reports `started`; `server stop` stops it; `--no-start` exits 6.
- `add file`: dry run shows the provisional filename title and unresolved metadata; without
  `--yes` exit 4 `confirmation_required` and nothing saved; `--yes` creates paper + attachment;
  `--attach-to` attaches to the existing paper.
- `show` (collections/tags/attachments, `paper://` URIs), unknown ID exit 3; `fulltext` with
  page/char limits (range >50 pages exit 2); `annotations` (an owner-session note at page index 2
  is reported as page number 3 with its label).
- `import` BibTeX: dry-run plan, exit 4 without `--yes`, commit of 2 rows, re-import exit 4
  `duplicate` (DOI match shown), `--on-duplicate skip` saves nothing, `--on-duplicate create`
  still cannot create an exact DOI duplicate (exit 4).
- `search` hit/limit cap; `list` sort + keyset cursor.
- `export`: raw BibTeX on stdout with summary on stderr; `-o` new file; existing file exit 4;
  `--force` replaces; symlink destination exit 4; destination inside the library exit 5;
  RIS by `--ids`; no scratch directory left behind.
- Scope refusal: `--read-only` add file/import/add doi/export exit 5 `permission_denied`;
  a daemon-issued LibraryRead-only credential via `--credential-file` and `PAPERS_CREDENTIAL_FILE`
  can list but every write exits 5; an unknown credential exits 5. Nothing was saved.
- Usage exit 2 (clap error on stderr, empty stdout; missing library); invalid DOI rejected by
  the daemon before any network lookup (exit 2); `papers mcp` exit 7 with empty stdout;
  `server start --rest` exit 7; `--help` examples; bash/zsh/fish completions.

Unit tests: envelope/exit codes, capability requests per subcommand (`--read-only`), credential
file permissions, duplicate/merge/skip policy, export path checks and atomic no-clobber publish,
DSL quoting, page ranges, error mapping (including the c182c7a handshake-retry rule), checked-in
completions match the grammar, and `add doi` against a scripted daemon with a mocked DOI
resolution (create after `--yes`, refusal without it, duplicate stop, skip, explicit merge with
fields, dry run, blank-title lookup needing `--title`).

## Manual smoke

Fresh temp library, no papersd process before: `server status` (not running) → `add file --yes`
(provisional title, 4 unresolved items listed) → `import fixtures/ris/recovery.ris --yes`
(3 created) → `list` (4) → `search Smoke` (1 hit) → `export --format ris -o out.ris` (4 records),
again exit 4 → `--read-only add file --yes` exit 5 → `mcp` exit 7 → `server stop`; no papersd left.

## Not verified

- Real DOI lookups (Crossref/DataCite) through the CLI: the papersd HTTP providers have no test
  endpoint override, so subprocess tests avoid the network; DOI flow logic is covered only with
  a scripted daemon. In this sandbox the providers are unreachable and `add doi` reports
  `unresolved` (exit 4).
- PDF text extraction and PDF metadata through the CLI: the workspace `papersd` is built without
  the PDFium feature, so `fulltext` returns pending/no pages and `add file` relies on the
  filename title.
- Interactive TTY confirmation prompt (only the `Prompt` abstraction is unit-tested).
- macOS (`.app` placement of `papers` beside `papersd`, peer credentials) — #34.
- MCP serving (#29) and the MCP/CLI/REST combination (#30); MVP final acceptance needs them.
- `server stop` while another client/job is active is guarded but not covered by a test.
- `server stop` on macOS (refused until peer PIDs can be verified there) and a daemon-side
  graceful shutdown command (not added; SIGTERM + papersd crash recovery).
- A stdout write error other than a closed pipe on the real binary (unit-tested only).

## Review fixes (second pass, same commands re-run after `cargo clean`)

Regression tests added: non-UTF-8 import/add/export paths exit 2 with a JSON envelope and a
non-UTF-8 library is reported lossily (subprocess, Linux); closed pipe / other stdout write
errors (unit); duplicate DOI across `--batch 1` batches needs a decision and commits nothing,
`--on-duplicate skip` creates one (subprocess + unit); `server stop` refused for a read-only
grant and under `--read-only` while the daemon keeps running, peer-PID verification (unit);
`add file --dry-run` succeeds with a LocalFileRead+LibraryRead+CapturePreview grant (0400
credential file) and the commit is refused; credential file symlink/0700/0400/unsafe-parent
cases; export containment by (dev, inode) and uninspectable library; published file mode equals
the umask-derived mode; merge into a trashed paper refused; text-mode control characters
stripped; typed handshake retry classification.

Manual smoke (fresh library, no papersd before): status, `add file --dry-run`/`--yes`,
cross-batch duplicate import exit 4 then `--on-duplicate skip` (1 created, 1 skipped),
`list | head -c 1` and `export | head -c 1` exit 0, export into the library exit 5, `-o out.ris`
published as `-rw-r--r--` (umask 022), `--read-only server stop` exit 5, `mcp` exit 7,
`server stop`; no papersd left.

## Update after #29 merged (papers mcp)

`papers mcp` now serves MCP through `cm_mcp::run_stdio_with` for the CLI's library. The earlier
"`mcp` exit 7" observations above describe the pre-#29 build. `crates/cli/tests/cli.rs` checks:
stdin closed before initialization → exit 0 with empty stdout; an `initialize` request is
answered on stdout with `serverInfo.name = "citation-manager"`; no library → exit 2.
