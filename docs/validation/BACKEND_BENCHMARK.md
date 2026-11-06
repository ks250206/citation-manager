# #47 release headless backend baseline

Measured 2026-11-07 JST at implementation commit
`b7c0dd6df5557feb8c491bac68757883af706432`, based on merged main
`d9157ff0f5db7bc06d37521d6878b0383160fdc4`. The measured working tree was clean;
subsequent changes are measurement evidence and this report. #46 is on that main,
but its new DSL parser is not used by these existing #8/#12/#13/#33 operations.
No product optimization, schema/migration, dependency/pin/lock, PR #45 worker or GUI
operation is included. This replaces the initial measurements at `f273466d`; their
raw files remain available in commit `d5287d6` and are not mixed with the new samples.

## Environment and reproduction

- Mac16,6, Apple M4 Max, 14 physical / 14 logical CPU cores;
  arm64 macOS 27.2 (26B123).
- **36 GiB RAM (38,654,705,664 bytes)**, different from DESIGN §16's 16 GB target.
- Rust 1.99.1 / Cargo 1.99.1, bundled SQLite 3.54.1.
- Locked default-feature release opt-level 3, no debug assertions/debuginfo;
  actual artifact paths/profiles/features/hashes and build environment are recorded.
  No explicit `--features` or RUSTFLAGS override was used. Default features may be active.
- Query fixture: seed 47, 10,000 papers, 10 collections, 8 tags, 20,000 links in each
  membership table, 20,000 contributors and 20,000 external IDs. No PDF/FTS/jobs.
- Query: 21 samples per case/page/condition, **546** timed calls, executed sequentially.
- Import: three independent empty DBs, 1,000 new papers each, ten explicit 100-row
  preview/decision/commit batches per repeat.
- No workspace checks or other agent work were run by root concurrently with the
  representative measurement. Other host activity and OS caches were not controlled.

```sh
python3 scripts/benchmarks/run.py --representative --out root-reviewed-release
```

See [instrument and exact method](../../scripts/benchmarks/README.md). The run is
retained under `target/backend-benchmark/root-reviewed-release`. Each private temp
DB is owned/deleted by its TempDir; there is no user-library path argument.
Committed [environment](BACKEND_BENCHMARK_2026-11-07/environment.json),
[raw query](BACKEND_BENCHMARK_2026-11-07/query.json),
[raw import](BACKEND_BENCHMARK_2026-11-07/import.json),
[summary JSON](BACKEND_BENCHMARK_2026-11-07/summary.json),
[CSV](BACKEND_BENCHMARK_2026-11-07/summary.csv) and
[batch log](BACKEND_BENCHMARK_2026-11-07/import.log) are inspectable.
CSV and SVG regenerate exactly from raw data with the documented Python functions.
Build logs remain in target. Summary commands and environment artifacts contain the
actual Cargo-returned executable paths, including custom CARGO_TARGET_DIR support.

## Query correctness and timings

Direct SQL seeds only the dedicated fixture connection, with FK ON, WAL/FULL and
one transaction. This is not CRUD/ImportService throughput. Generation, serialization,
insert and checkpoint seed time was **469.183 ms**, outside query samples.
Integrity was ok, FK violations zero, and exact table/membership contents passed.
The production factory revalidated migration history. Complete ordered IDs,
full Paper metadata, cursor progress and terminal pages were checked outside timers
in **385.923 ms**. Ordered fixture SHA-256:
`0386c3881aadab6e8ed95e33c0d3a97b3541258d83a5fff12f3c462d92f704b4`.

| Case | Expected = actual hits | Validated pages (limit 50) |
|---|---:|---:|
| list | 10,000 | 200 |
| metadata_keyword | 1,429 | 29 |
| metadata_no_hit | 0 | 1 |
| collection | 2,000 | 40 |
| tag_all | 1,250 | 25 |
| tag_any | 3,750 | 75 |
| combined | 71 | 2 |

These are **bounded API page calls**, not full-result scans or GUI latency.
Page 0 is first; page 1 uses its real cursor. Reopened means a fresh production
connection for each call, not OS cold cache; validation already read the DB.
Connection open/history/baseline time is separate from the page timer. Warm uses
one retained connection and an untimed priming call. Case/condition order is fixed.
All timings are milliseconds; p50/p95 use nearest rank `ceil(p*N)-1`, N=21.

| Case | Page | Returned / call | Reopened p50 / p95 (ms) | Warm p50 / p95 (ms) |
|---|---:|---:|---:|---:|
| collection | 0 | 50 | 0.818 / 0.943 | 0.770 / 0.858 |
| collection | 1 | 50 | 0.973 / 1.050 | 0.860 / 0.938 |
| combined | 0 | 50 | 27.531 / 31.412 | 27.769 / 27.954 |
| combined | 1 | 21 | 11.641 / 12.025 | 11.819 / 11.873 |
| list | 0 | 50 | 0.681 / 0.751 | 0.689 / 0.744 |
| list | 1 | 50 | 0.795 / 0.917 | 0.794 / 0.862 |
| metadata_keyword | 0 | 50 | 2.804 / 3.048 | 2.105 / 2.443 |
| metadata_keyword | 1 | 50 | 2.376 / 2.569 | 2.079 / 2.355 |
| metadata_no_hit | 0 | 0 | 41.582 / 42.797 | 42.307 / 43.341 |
| tag_all | 0 | 50 | 1.084 / 1.262 | 0.985 / 1.098 |
| tag_all | 1 | 50 | 1.207 / 1.312 | 0.993 / 1.119 |
| tag_any | 0 | 50 | 1.005 / 1.105 | 0.915 / 1.010 |
| tag_any | 1 | 50 | 1.049 / 1.303 | 1.011 / 1.029 |

Summary `connection_open` rows exist only for reopened conditions. Warm raw
`open_ms=0` means no timed opening, **not** a measured zero-latency open. There are
71 summary rows (26 query, 13 real connection-open, 32 import), with no synthetic
warm-open metrics. Query timers include service validation/SQL/full materialization/
cursor construction; expected-value comparisons and output serialization are outside.

## Actual parser / confirmed import timings

Input: 1,091,560 synthetic UTF-8 BibTeX bytes generated before timing;
SHA-256 `22fe0a077c9ae8c2acc322fbab3f0087937dc932ce66645d908e8e73acf3f335`. Unique SHA-derived titles deliberately avoid saturating
near-duplicate budgets. This is a controlled unique-record workload, not a natural
researcher/duplicate-heavy distribution. No matching limits or policy were relaxed.
Setup and collection/tag destination creation are outside timing. Real #13 parser
and #33 default TTL/capacity/budgets are used. The synthetic caller explicitly checks
valid rows, source hash and absence of exact/batch duplicates before Create decisions;
this does not model human, GUI, IPC or native-host approval. No retry/automatic merge.

All 30 previews left DB and WAL byte-identical. A separate production read connection
observed 0→100→…→1000. All 30 commits created exactly 100 rows with no merge/skip.
Each reopened DB contained all 1,000 fully equal bibliographies/external IDs and exact
collection/tag memberships. Per-stage values remain in raw JSON/logs.
`total_ms` runs from parser start through final batch count validation, including hashes,
readback, decision construction and instrumentation. It excludes source generation,
setup and final reopen/full-content validation, recorded separately. It is not human
waiting time and is not equal to the sum of measured service intervals.

| Repeat | Parser (ms) | Total wall (ms) | Reopened stored count |
|---|---:|---:|---:|
| 0 | 16.123 | 8017.860 | 1000 |
| 1 | 11.684 | 8030.446 | 1000 |
| 2 | 11.855 | 8052.964 | 1000 |

| Batch | Existing papers before | Preview p50 / p95 (ms) | Commit p50 / p95 (ms) | Created / commit |
|---|---:|---:|---:|---:|
| 1 | 0 | 47.823 / 53.760 | 29.121 / 29.189 | 100 |
| 2 | 100 | 149.757 / 150.947 | 78.412 / 78.515 | 100 |
| 3 | 200 | 254.163 / 254.936 | 129.488 / 132.365 | 100 |
| 4 | 300 | 356.984 / 359.663 | 178.855 / 180.495 | 100 |
| 5 | 400 | 459.469 / 461.724 | 227.131 / 228.615 | 100 |
| 6 | 500 | 565.527 / 566.116 | 280.521 / 281.965 | 100 |
| 7 | 600 | 667.457 / 691.020 | 327.628 / 328.409 | 100 |
| 8 | 700 | 769.932 / 772.297 | 376.449 / 378.191 | 100 |
| 9 | 800 | 876.930 / 884.967 | 430.057 / 431.749 | 100 |
| 10 | 900 | 979.189 / 986.427 | 475.574 / 477.095 | 100 |

Total p50 / p95: **8030.446 / 8052.964 ms**, over three repeats.
N=3 p95 is the maximum observation, not a robust tail estimate or confidence interval.
Different inventory sizes are not pooled into one latency distribution. Decision times
are also retained in JSON/CSV. The reproducible chart uses p50 bars and p95 whiskers:

![Measured import batch medians and p95](BACKEND_BENCHMARK_2026-11-07/import-batches.svg)

## Evidence-supported follow-up, deferred

First/last preview medians: 47.823 / 979.189 ms;
commit medians: 29.121 / 475.574 ms.
Each batch still has 100 rows. This is inventory-associated growth, not proof of a
particular bottleneck. Unchanged inventory/revision materialization and pairwise
classification code are candidates for future profiling, preserving DOI compatibility,
reviewed matches, stale/ABA protection, byte budgets and atomic decisions. No measured
latency is attributed to an untimed subphase; this PR changes no product algorithm.

## Scope limits and validation

This 36 GiB headless baseline does not complete parent #30 or DESIGN §16. No 16 GB
whole-app workload, GUI collection-switch/frame/startup latency, CPU/I/O/RSS,
1,000 PDFs/20,000 pages, FTS/Japanese recall, jobs, crash/security regression or total
PDF process memory was measured. #15 GUI gate, #34 HELD and #23/#25 gates stay intact.

The [initial check report](BACKEND_BENCHMARK_2026-11-07/checks.json) and numbered logs
record the earlier implementation validation: fmt, locked workspace Clippy/build,
159 Rust tests including the old 1000-paper roundtrips, architecture/migrations,
Python 21 and whitespace. These logs are historical, not checks on the new harness.
Root's follow-up checks and final independent review/CI are recorded separately below.

The original Opus review at d5287d6 was approval-equivalent. Root addressed its three
notes: absent warm opens removed from metric summaries, actual executable paths
recorded, and SVG generation now part of the reproducible runner. Default-feature
wording is explicit. Root merged current main d9157ff; benchmark Rust sources and
production query/import algorithms are unchanged. Python 22 (including chart geometry
and absent-open regressions), fmt, locked workspace Clippy/build passed before this
representative rerun. Full integrated workspace validation passed 176 tests including the compile-fail
doctest, with zero failed/ignored/filtered; both existing 1000-record BibTeX/RIS
roundtrips ran. Architecture and immutable migration checks against d9157ff passed.
[Root follow-up check report](BACKEND_BENCHMARK_2026-11-07/root-follow-up-checks.json)
and [workspace log](BACKEND_BENCHMARK_2026-11-07/root-workspace.log) retain these
results separately from initial historical logs. Final-head CI and independent
recheck remain required. GUI/FTS/jobs/signing were not exercised.
