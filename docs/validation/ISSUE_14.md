# #14 execution evidence

2026-11-07 (JST), T3-prepared isolated checkout `t3-be24c577`, branch
`t3/doi-metadata-resolver`, base `18a1eb57597508d274dccae558a26e8dad043fd0` (#33).
Only synthetic HTTP records, loopback test servers and temporary SQLite libraries were
used. No actual library/profile, other worktree, root checkout or agent configuration was
written. No public API smoke was run; CI requires no real Crossref/DataCite availability.

## Meaningful exercised scenarios

- Real pinned reqwest HTTP transport against private loopback servers: complete Crossref
  success skips DataCite; incomplete primary gets DataCite complement and explicit conflicting
  field evidence; primary404/invalid JSON gets DataCite fallback; both404 or both offline
  produce editable DOI-bearing manual drafts with diagnostics.
- Strict response envelope/field types, malformed JSON, invalid leap-day, excessive authors,
  declared/chunked oversized body, mismatched primary/alternate DOI and DataCite data.id,
  raw-percent vs decoded identity; redirect is rejected without a follow-up fetch. Safe URL
  segment construction cannot move DOI characters into query/fragment/host; candidate URLs
  reject credentials, nonweb schemes, local/private IPs and unexpected ports.
- Identifying public app UA and absence of Authorization on wire.429 Retry-After seconds
  and HTTP-date; long delay suppresses later fetches without early retry; advertised provider
  rate headers, exhausted500 retries, network recovery, closed-connection failure, timeout
  and in-flight/pre-cancellation. Transient failures are not cached. Fake monotonic/wall clocks
  and waits make retry/cache tests deterministic; only the real transport deadline is a
  short40ms timeout with a2s test bound, no multi-second backoff sleeps.
- Positive/negative TTL, exact expiry, canonical wrapper input reuses cache, provider cache
  isolation, capacity/FIFO eviction, bounded config and async-context constructor rejection.
  Full Unicode author/editor/organization order, ORCID/roles, Crossref day and DataCite month
  precision, field mappings and raw external-ID/unmapped metadata/source SHA provenance.
- Core port tests reject mismatched DOI representations, preserve primary date/year
  consistency, avoid provider calls for invalid/pre-cancelled input, and fall back explicitly
  when combined evidence exceeds common preview limits.
- Real SQLite composition: resolve -> **existing** ImportCoordinator preview -> duplicate/
  differences -> cancel leaves database **and WAL bytes** unchanged. Rejected Create writes
  nothing. Explicit title-only merge changes only title/timestamp, preserves all unselected
  metadata/citation key and survives reopen. Additional PMID and title/author/year candidates
  and generated citation key use #33's common code.
- Existing actual old-API SQL fixture loaded into a private real SQLite library, reopened:
  resolved decoded DOI matches the persisted raw resolver witness, cancel is byte-unchanged,
  approved title-only merge preserves old DOI/raw external IDs/all unselected bibliography.
  Unwitnessed percent-literal ambiguity remains visible without guessed aliases or rewrites.

The SQLite harness uses neutral provider-port replies; real HTTP/schema tests above cover
that adapter separately. Its file is `tests/integration/doi_import.rs`, registered only as a
composition test target in the existing cm-daemon manifest, whose existing dependency
edges already permit core+storage+metadata. No daemon implementation, new driver edge or
reverse metadata->storage dependency was added. Fixture setup uses Python's SQLite only
to load the existing historical INSERTs after closing the test library; production writes
use the real shared coordinator and storage backend exclusively.

## Required checks

All checks below exited0 after the implementation:122 workspace unit/integration tests
and1 compile-fail doctest (123 total), including4 new core,17 metadata and3 SQLite
composition tests;18 Python tests,12-crate architecture and2 unchanged migrations.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/check_architecture.py
python3 scripts/check_migrations.py --base 18a1eb57597508d274dccae558a26e8dad043fd0
python3 -m unittest discover -s scripts -p 'test_*.py' -v
git diff --check
```

The initial unrestricted `cargo generate-lockfile --offline` proposed unrelated upgrades;
that result was discarded. Cargo then resolved the newly used dependencies against the
unchanged base lock. Inspection confirms zero removed/replaced existing package versions,
zero existing package checksum changes,63 newly required package versions for the pinned
HTTP/TLS stack and httpdate helper. Existing dependency references gain version qualifiers
where a new second version makes the package name ambiguous; Tokio gains the HTTP runtime
features. Existing workspace pins/features, migration SQL/manifest/checksums are unchanged.
The predeclared reqwest pin/features were reused exactly.

Repeated parallel loopback runs exposed an accepted-socket readiness race on macOS:
nonblocking accepted streams could be read before headers arrived, consuming a fixture
without a request. The server explicitly switches accepted sockets to blocking mode with
a read deadline and requires complete request headers before recording/consuming a reply.
The offline fixture also retains its port, and responses are buffered with explicit write
shutdown. After correction,15 complete parallel17-test runs passed (255 test executions).
No production retry policy was relaxed to mask test failures.

## Limits and handoff

No native feature builds, GUI/browser/VM, PDFium runtime, packaging, signing/notarization,
public API smoke, actual library migration/backup, daemon/IPC/auth or #13 adapter/export
integration was performed. #34 stays held and #23/#25 gates stay closed. Local default
workspace build is compilation evidence; SQLite/loopback scenarios are runtime evidence.

Independent different-model review and merge remain root responsibilities. This agent
ran no child/reviewer, merged nothing and did not watch/poll CI. CI is inspected once at
handoff for the exact pushed PR head; an in-progress state is not a pass.

## Independent-review corrections

Opus 5.5 medium reviewed `cad25b5` read-only, reproduced P2 date handling issues and three
P3s, and verified all193 tracked file hashes stayed unchanged. Full findings are recorded
in PR comment6689942729. Root verified Crossref's official null-issued example and DataCite
4.6's timestamp/range allowance (links in ADR); these were not verified by the reviewer.

- Null Crossref date parts preserve usable metadata and try print/online fallback. Raw date
  fields remain in source metadata, including the null value.
- DataCite timestamps use common-validated calendar precision; ranges, unrepresentable dates
  and conflicting years preserve the record/year/raw dates with a non-error diagnostic.
- Excluded landing URLs retain their raw value and a diagnostic, preserving the rest of the
  record. Unsafe PDF candidates remain excluded.
- Invalid external IDs from a replacement core port are provider InvalidResponse outcomes,
  so a valid secondary record or manual fallback remains available.
- Calling an existing HTTP adapter in an async runtime returns Unavailable with zero fetches;
  it remains usable after returning to its worker. Its owned runtime must be dropped outside
  async contexts, as documented. Pinned Tokio1.54.1 Runtime/drop and blocking/shutdown sources
  confirm that constraint; Context7's tagged query returned reactor internals rather than
  the relevant Runtime API, so it was supplemented by these primary local sources.

Three new real-loopback tests cover Crossref unknown/print/online dates; nine DataCite
timestamp/range/leap-day/year/unsupported precision cases; and both providers' excluded URLs
without weakening PDF policy. One new core-port test covers invalid external IDs with valid
secondary and both-invalid manual fallback. The existing async-context test now exercises
lookup rejection, no fetch, and subsequent synchronous success. Source hashes and original
SQLite confirmation policies remain intact. Root reran fmt, locked warnings-denied workspace
clippy/tests/build, architecture, immutable migrations, Python and whitespace checks:
126 unit/integration +1 compile-fail doctest (127 total),18 Python tests, all passed.
No dependency, lockfile, schema or migration changes were needed for these corrections.
Independent re-review and the new head's CI are pending; this is not a review approval.
