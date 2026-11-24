# #22 validation: browser/URL capture, safe PDF fetch and retry

Run 2026-11-24 by the implementing agent in an isolated worktree (base
`claude/issue-25-daemon-ipc` @ `271edae`). Environment: Linux 6.18 x86_64 container,
rustc/cargo 1.99.1 (pinned), Python 3. Design: [browser capture ADR](../ADR/browser-capture.md).
All tests are offline: synthetic HTML fixtures (`fixtures/html`), an in-process fake DOI
provider and a loopback HTTP fixture server reached through a fixture name resolver.

## Automated (Linux)

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| `cargo test --workspace --locked --no-fail-fast` | passed: 291 tests, 0 failed, 0 ignored (56 test binaries/doctest groups). One earlier full run hit a failure in the pre-existing `daemon_lifecycle::concurrent_starts_leave_exactly_one_writer` (a losing `papersd` exited 1 with `library I/O: File exists`, a first-start directory race outside this change); it passed 3 isolated reruns and the full rerun |
| `python3 scripts/check_architecture.py` | passed (no new crate edge) |
| `python3 scripts/check_migrations.py --base origin/main` | passed (no new migration) |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | passed: 32 |

`Cargo.lock` is unchanged (no new dependency). Cargo commands ran with a worktree-private
`CARGO_TARGET_DIR`, `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_{DEV,TEST}_DEBUG=0` (disk space on
the shared host); no repository setting was changed.

## New tests

- `cm-core` `capture` (4): request bounds, arXiv DataCite DOI derivation, tier order
  (Highwire → JSON-LD → DC → provider → provisional), date/year group, provider failure keeps
  page data, 64 KiB budget shedding. `policy::authorizer`: native host may run every capture
  operation but not `MetadataResolve`, `JobSubmit`, `JobRetry`, `PaperCreate`, `ImportCommit`;
  MCP read-only cannot capture.
- `cm-metadata` `html` (6): publisher/arXiv/PubMed/JSON-LD/Dublin Core/no-metadata fixtures,
  reference-list DOI links not guessed, relative `citation_pdf_url`, malicious page (commented
  and script-written tags ignored, command/path strings kept as inert text, invalid IDs/dates
  dropped with diagnostics, 12 SSRF-style PDF links refused), scanner limits and truncated markup.
- `cm-metadata` `capture` (5): resolution order with a fake provider (no lookup when page data is
  complete; complement fills only missing fields; arXiv DOI added only when confirmed; no PMID
  lookup), provisional fallback, malicious page yields a normalizable draft, oversized requests
  rejected, page-forwarded `client_kind`/`capabilities` ignored.
- `cm-metadata` `fetch_policy` (5): 36 non-public URL forms blocked (file/data/javascript, http,
  local names, IPv4 private/loopback incl. decimal/hex forms, metadata IP, IPv6 loopback/ULA/
  link-local/mapped/NAT64/6to4/doc, userinfo, ports); redirect chain fetch with SHA-256 and **no
  Cookie/Authorization/Referer** even after `Set-Cookie`; every redirect hop and DNS answer
  re-checked (metadata IP, private name, mixed public/private answers, IPv6 ULA, mapped IPv4,
  `file:`, `localhost`, loops); HTML login page / octet-stream without `%PDF-` / gzip / empty →
  not PDF; 401/403 → auth required; 410; 503/429 retryable; 418 not; size limit via
  Content-Length and chunked streaming; stalled body → timeout; cancellation; invalid config.
- `cm-daemon` `jobs::capture` (1): attachment display names are inert.
- `daemon_capture` (3, real daemon over IPC):
  - native host (spoofing `client_kind = "desktop"` and all scopes) previews and commits a
    capture with a destination tag; general `ImportCommit` and `JobCommand::Submit` of
    `capture.pdf_fetch` are denied (also for the desktop); out-of-range PDF index rejected; the
    PDF job follows a redirect, attaches the PDF with the expected SHA-256, sends no cookie; the
    preview is consumed.
  - 503 ×2 then success → succeeded after 3 attempts; HTML login page → failed after 1 attempt
    with the user-download guidance, paper kept, explicit `RetryPdf` succeeds once public;
    persistent 502 → "attempt limit 4 reached", paper kept; redirect to a privately resolving
    name → blocked, never requested.
  - duplicate: native host sees hidden matches, Create refused, Merge forbidden; another grant
    cannot commit/cancel; owner cancel; desktop merge with PDF; same bytes from another URL →
    `detected_committed`, still one attachment; other grant cannot read the job.

## Not verified here

- Real browsers, the native messaging host and extension (#23/#24), macOS (#34).
- Live Crossref/DataCite during a capture and real publisher pages (offline fixtures only).
- Real DNS/TLS fetches from the internet (the policy and pinning are exercised against the
  loopback fixture server with an injected resolver).
