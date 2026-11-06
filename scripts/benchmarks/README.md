# #47 headless backend baseline

Run from the checkout root on macOS, with the pinned Rust toolchain:

```sh
# Small release measurement/output smoke; no large data by default.
python3 scripts/benchmarks/run.py --query-samples 3 --import-samples 1 --out small
# Explicit opt-in: 10,000 query papers and three independent 1,000-paper imports.
python3 scripts/benchmarks/run.py --representative --out representative
```

The runner builds **release**, locked, default-feature examples. Large measurements
are not registered as tests or added to CI. `test = true` registers only the small
instrument tests in `cargo test --workspace --locked`: a 117-paper query fixture
and a 101-paper import checking the 100+1 boundary. Python discovery validates
nearest-rank quantiles, CSV round trips and rejection of missing/invalid samples,
wrong returned counts and preview writes. Tests have no speed thresholds.

Both examples are explicit benchmark entries, not production CLI commands. Their
source is here; two manifest registrations preserve the existing architecture:
storage owns the SQLite fixture/query example, daemon composes the existing
metadata parser and storage import API. No production algorithms or library
connection visibility are changed. No network or native/GUI feature is invoked.

Every invocation creates its own `0700` temporary root using `tempfile`, passes a
fresh subdirectory to `LibraryDirectory::prepare`, and deletes its own DB on drop.
There is no user-library path argument. LibraryDb enforces its existing private
DB/sidecar policy. Seed SQL uses a separate connection to that dedicated fixture
only, with foreign keys ON, WAL, FULL synchronous, no CREATE/URI and NOFOLLOW.
No PR #45 worker is used. Source bytes are generated synthetic data, never user
records. Output is kept in a new `0700 target/backend-benchmark/<name>`; the runner
refuses to overwrite results. Do not run workspace tests/builds or other heavy
work simultaneously with measurements.

Query fixture seed 47 is deterministic. Row `i` has `n=i+47`, two ordered
contributors (author/editor), two external IDs (DOI/synthetic accession), a
roughly 550-byte abstract, complete ordinary bibliography, Japanese/Greek text,
and extra JSON. Every row belongs to collections `n%10` and `(n%10+1)%10`, and tags
`n%8` and `(n%8+1)%8`. There are 10 collections and 8 tags. Titles contain `needle`
exactly when `n%7==0`. IDs and timestamps are fixed. The seed SHA-256 covers the
ordered serialized full fixture Papers. This is **direct SQL seed**, one
transaction plus checkpoint, not measured CRUD or product import throughput.

After seeding, the production migration runner validates history on reopening;
integrity/FK checks, exact table counts, all returned full metadata and expected
filter IDs are checked outside timing. Every query case is fully traversed in
50-paper ID-order pages, verifying the exact ordered expected IDs, next-cursor
presence and terminal page. Measured calls retrieve the first and (if present)
second bounded page, not an unbounded full result set. Cases are list, metadata
`needle`, no-hit sentinel, direct collection 0, tags 0 AND 1, tags 0 OR 1, and
collection 0 AND tag 0 AND `needle`. No sorting, trash, attachment, relevance,
PDF text or fulltext benchmark is claimed.

`reopened_connection_os_cache_uncontrolled` opens a fresh production connection
for **each timed page call**. This clears its connection-local SQLite cache but
does not evict OS storage caches; validation has already read the DB. Connection
opening/migration/history/baseline validation time is recorded separately as
`open_ms`, excluded from `elapsed_ms`. `warm_connection` retains one connection
per case/page and performs one untimed identical query before repeats. Cases and
conditions execute in a fixed order; conditions are not randomized. Query timers
include validation, SQL, full Paper materialization and cursor construction in
the real LibraryQueryService/adapter, excluding the harness's expected-value
comparisons. JSON/CSV serialization is outside those timers.
Only reopened conditions produce `connection_open` summary rows. Warm raw
`open_ms=0` is a sentinel for no timed open, not a zero-millisecond measurement.

Import fixture seed 47 generates BibTeX in memory, with diverse SHA-derived
titles, unique DOI/keys, two authors and an editor, Unicode, dates, journal,
abstract, keywords and URL. These are controlled unique records; the title
distribution deliberately avoids saturating the real match budget and is not a
natural-language duplicate workload. No matching limits or budgets are relaxed.
Each repeat starts with zero papers in an independent empty production DB; one
collection/tag destination is created before timing. Parser/source generation
and DB setup times are separate. Actual #13 `bibtex::parse`, #33 default-policy
ImportCoordinator preview, inspected normalized candidates, explicit Create
decisions and commit are used, in at most 10 sequential batches of at most 100.
There is no batch retry, automatic merge or file-wide transaction.

The fixture decision rule requires no error diagnostics, no exact persisted or
same-batch duplicates, and source hash agreement; it explicitly creates distinct
records even if nonexact suggestions appear. This is a synthetic caller decision,
not human confirmation, authentication, IPC, GUI or native-host approval proof.
Default TTL/capacity, stale/revision checks, atomicity and security semantics stay
unchanged. Before/after every preview the entire DB and WAL are SHA-256 compared,
and a second production connection checks committed counts. Each batch records
preview/decision/commit durations and real before/after counts. Reopening checks
all saved bibliography/external IDs and every collection/tag membership.

Import `total_ms` is wall time from parser start through the final batch's
count validation, including fixture review, hashes, readback and instrumentation
overhead. It excludes source generation, setup, and final reopen/full-content
verification (`post_reopen_validation_ms`). Summed parse+preview+commit times are
service intervals, not that total and not human waiting time. Import repeats are
fresh DBs with uncontrolled OS caches, not a cold-disk benchmark.

Outputs: `environment.json`, raw `query.json` and `import.json`, synthetic-only
per-batch `import.log`, build logs, `summary.json` and `summary.csv`. Raw timings
are kept; summaries use **nearest rank `sorted[ceil(p*N)-1]`**, no interpolation.
With three import repeats p95 is the maximum, not a stable tail estimate or a
confidence interval. Returned counts are part of each query sample and summary;
import rows report created counts. A nonzero exit or missing summary is failure.
The runner records the actual executable path returned by Cargo in its commands
and environment artifact metadata, including when `CARGO_TARGET_DIR` is set.
It also writes the standalone `import-batches.svg` from the same summary rows:
bars are p50 and whiskers are p95 in milliseconds. To regenerate CSV and chart
from existing raw data, without another measurement:

```sh
PYTHONPATH=scripts python3 - <<'PY'
import json
from pathlib import Path
from benchmarks.run import csv_text, import_chart, summarize
p = Path('target/backend-benchmark/representative')
env = json.loads((p / 'environment.json').read_text())
rows = summarize(json.loads((p / 'query.json').read_text()),
                 json.loads((p / 'import.json').read_text()),
                 env['query_samples'], env['import_samples'])
(p / 'summary.csv').write_text(csv_text(rows))
(p / 'import-batches.svg').write_text(import_chart(rows))
PY
```

Environment records Mac model/OS/CPU/RAM, Rust/Cargo, build environment/profile,
Git revision/status, harness and Cargo.lock hashes, dataset seed/size/hash and
sample counts. These headless API timings do **not** establish DESIGN §16's
16 GB whole-app workload, GUI latency/scroll/frame/startup targets, CPU/I/O/RSS,
PDF/FTS/jobs, crash/security regression or parent #30 acceptance. #15/#25/#23
gates and #34 HELD remain unchanged. No optional RSS measurement is made here.

API check: find-docs/Context7 `/rusqlite/rusqlite/v0.41.1` and locally pinned
`rusqlite-0.41.1/src/{lib,transaction}.rs` confirmed explicit open flags,
transactions and checkpoint support. See the
[upstream pinned README](https://github.com/rusqlite/rusqlite/blob/v0.41.1/README.md)
and [SQLite checkpoint documentation](https://sqlite.org/pragma.html#pragma_wal_checkpoint).
