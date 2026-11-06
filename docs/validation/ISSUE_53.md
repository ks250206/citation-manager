# #53: measured bundled SQLite tokenizer and finite short-query fallback

Measured on 2026-11-07 JST at clean implementation commit
`0d7942fbe5767886b254af1c2d2ac4dd9c99e819`, based on prepared main
`9e7e66e3e284a047096eae0707b823233206fe8c`. Subsequent changes retain evidence
and this report. This completes the independent measurement scope of #53 only.
#18 remains dependent on #25; #30 full-load acceptance remains open. #34 is HELD
and #23/#25 gates stay closed. #15's GUI worker remains the sole GUI operator;
this task performed no GUI/browser/native changes or GUI actions.

## Reproduction and provenance

```sh
python3 scripts/search-experiments/run.py --out a-new-issue53-run
```

[Exact method and limitations](../../scripts/search-experiments/README.md),
[23-record synthetic CC0 corpus](../../scripts/search-experiments/fixture.json),
[independent expected hit sets](../../scripts/search-experiments/expected.json), and
[license](../../scripts/search-experiments/LICENSE.fixture) are committed separately
from observations. The hand-enumerated expectations predate SQLite execution and
were not regenerated from its results. No third-party/user literature is present.
There is no DB path option: the Rust instrument owns a new private TempDir DB,
uses a canonical path with SQLite NOFOLLOW, and deletes it on exit. The runner
refuses to overwrite its dedicated output directory.

Retained [environment/source/binary hashes](SEARCH_EXPERIMENT_2026-11-07/environment.json),
[raw SQL observations and all timings](SEARCH_EXPERIMENT_2026-11-07/raw.json),
[validated summary JSON](SEARCH_EXPERIMENT_2026-11-07/summary.json),
[CSV](SEARCH_EXPERIMENT_2026-11-07/summary.csv), and
[Cargo artifact build log](SEARCH_EXPERIMENT_2026-11-07/build.log) are inspectable.
The raw file SHA-256 is in summary.json; environment_sha256 is sorted JSON before
adding its own hash. Source hashes cover fixture/oracle, Rust/Python instrument
and pinned SQLite C/build sources. Summary and CSV regenerate exactly from raw
with the functions documented in the method. Build artifacts live in target and
are not required to inspect the retained JSON.

Environment: Mac16,6 Apple M4 Max, arm64, 14 physical/logical cores,
**36 GiB RAM (38,654,705,664 bytes)**, macOS 27.2 (26B123), Rust/Cargo 1.99.1.
Default release opt-level 3 with no debug assertions; recorded build overrides
were unset. No workspace checks were started by this implementation during the
measurement. Other processes/parallel tasks and OS caches were uncontrolled.

The instrument ran `rusqlite 0.41.1` / `libsqlite3-sys 0.39.1`, both with Cargo's
bundled feature, and runtime **SQLite 3.54.1**. Runtime source ID exactly matched
the pinned C source:

```text
2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76
```

All 51 compile options are in raw.json, including `ENABLE_FTS5`,
`ENABLE_DBSTAT_VTAB`, `THREADSAFE=1`, and `COMPILER=clang-21.0.0`. The runner
checked compiled artifact features and runtime source/version against pinned source;
it did not execute system or Python SQLite. Experiment DB settings: 4,096-byte
pages, DELETE journal, synchronous FULL. Tables use stored content, detail full,
columnsize 1; tokenizers explicitly use `unicode61 remove_diacritics 1` and
`trigram case_sensitive 0 remove_diacritics 0`, with remaining options default.

## Measured behavior

Actual instance vocabulary contains 72 unicode61 token occurrences / 45 distinct
terms, and 281 trigram occurrences / 186 distinct terms. Query text is inserted
into separate probe indexes to observe index-time tokens. Candidates are a SQL
union of documents sharing any probe token, not an instrumented FTS planner list.
The exact hit column executes separately quoted-phrase MATCH. All corpus/query
term/doc/offset records and every expected/actual ID set are retained in raw.json.

**Literal reference:** ASCII case-insensitive exact substring, preserving all
Unicode normalization, non-ASCII case, width and accent differences. Length is
Unicode scalar count. No silent NFC/NFKC/full folding is performed. FTS tokenizer
semantics differ deliberately; recall against this literal reference is diagnostic,
not a claim that all FTS matches have literal precision.

| Query | Literal expected IDs | unicode61 MATCH IDs | trigram MATCH IDs |
|---|---|---|---|
| `電` (1 scalar) | 1,2,3,9,10,16,17,22 | 9 | none: no tokens |
| `電池` (2) | 3,9,10,16,17,22 | 9,17,22 | none: no tokens |
| `固体電` (3) | 1,3,16 | none | 1,3,16 |
| `電解質` (3) | 1,2,16 | 2 | 1,2,16 |
| `alpha beta` | 11 | 11,12 | 11 |
| `alpha, beta` | 12 | 11,12 | 12 |
| `café` (NFC) | 6 | 6,23 | 6,23 |
| `café` (NFD) | 6 | 6,23 | 6 |
| `ω` | 15 | 15,23 | none: no tokens |

For example, row 1 `固体電解質リチウム` is one unicode61 token; row 2's spaces
split it. Probe `固体電` has no shared unicode61 corpus token, while trigram has
that token at rows 1,3,16. The `alpha beta` vocabulary union returns candidates
11,12,13 for both tokenizers: unicode61 phrase order removes 13; trigram requires
the actual space sequence and also removes 12. `café` has unicode61 probe `cafe`,
but trigram probes `caf`, `afe`, `fé`; its candidates include 23 via `caf`, while
exact trigram MATCH removes 23. These are SQL observations, not documentation-based
predictions substituted for execution. Extra FTS accent/case hits are preserved.

## Separate bounded literal fallback

`bucket='materials'` selects ten records in ascending ID order:
1,2,3,4,5,9,10,16,18,22. Each call counts that filter's records, then reads id/text
with SQL LIMIT before literal comparison. Cap is 1..64; this is a row-count bound,
not a timeout/byte/general SQL-work guarantee. The count may scan the filtered
index; unsearched/excluded ID diagnostics occur outside the timer. The product's
PDF/collection/field scopes and continuation are not implemented.

| Case | Cap / read | Returned IDs | Unsearched eligible | Oracle recall within filter | Scope status |
|---|---:|---|---:|---:|---|
| `電池` cap 2 | 2 / 2 | empty | 8 | 0/5 | partial_unsearched |
| `電池` cap 4 | 4 / 4 | 3 | 6 | 1/5 | partial_unsearched |
| `電池` cap 64 | 64 / 10 | 3,9,10,16,22 | 0 | 5/5 | complete_within_filter |
| `電` cap 4 | 4 / 4 | 1,2,3 | 6 | 3/7 | partial_unsearched |
| `電` cap 64 | 64 / 10 | 1,2,3,9,10,16,22 | 0 | 7/7 | complete_within_filter |
| `li` cap 64 | 64 / 10 | 3,4,5 | 0 | 3/3 | complete_within_filter |
| `_`, language filter | 3 / 3 | empty | 8 | 0/1 | partial_unsearched |
| `雪` cap 2 | 2 / 2 | empty | 8 | null (empty oracle) | partial_unsearched |
| `電`, empty filter | 2 / 0 | empty | 0 | null (empty oracle) | complete_within_filter |

The cap-2 empty return is **not a zero-match conclusion**: five expected matches
remain unsearched. Even the complete materials result excludes 13 corpus records,
including matching outside-filter row 17. Raw output lists every unsearched and
excluded ID. Recall is possible here only because the fixture oracle is known;
a product cannot infer recall from unsearched user data. Empty denominators remain
null even when fixture inspection confirms absence.

## Timing and index size

67 separate cases (29 queries × 2 tokenizers + 9 fallback) each have 5 untimed
warmups and 31 timed repetitions: **2,077 timed calls**. Setup, vocabulary/correctness
probes and the same connection warm the database; no cold-cache measurement occurred.
Exact timers are in raw.json and README: FTS includes phrase quoting/prepare/bind/
step/ordered IDs; fallback includes filtered count/capped text fetch/ASCII literal
comparison/ID collection. Oracle checks, scope-ID diagnostics and serialization are
outside. Seed/indexing/import/extraction are not throughput measurements. p50/p95
are nearest-rank ranks 16/30 of N=31, no interpolation/confidence interval.

FTS normal series, selected cases, milliseconds:

| Query | unicode61 p50 / p95 | trigram p50 / p95 |
|---|---:|---:|
| `電` | 0.007084 / 0.009209 | 0.005166 / 0.005375 |
| `電池` | 0.007167 / 0.008208 | 0.014750 / 0.019917 |
| `固体電` | 0.006834 / 0.007541 | 0.007250 / 0.007916 |
| `電解質` | 0.007459 / 0.008459 | 0.007167 / 0.007875 |
| `alpha beta` | 0.008458 / 0.009167 | 0.010916 / 0.011875 |
| `alpha, beta` | 0.007250 / 0.008500 | 0.029000 / 0.097083 |

Short-query fallback series (separate), milliseconds:

| Case | p50 / p95 |
|---|---:|
| `電池` cap 2 | 0.011875 / 0.013042 |
| `電池` cap 4 | 0.012250 / 0.013209 |
| `電池` cap 64 | 0.014000 / 0.016584 |
| `電` cap 4 | 0.013000 / 0.014292 |
| `電` cap 64 | 0.013542 / 0.014542 |
| `li` cap 64 | 0.013625 / 0.015000 |
| `_`, language | 0.012000 / 0.013291 |
| `雪` cap 2 | 0.011917 / 0.013292 |
| empty filter | 0.011208 / 0.012208 |

These microsecond-scale values and visible variation do not establish comparative
product performance. No pooling across tokenizers/fallback or extrapolation to
10,000 papers/20,000 pages/16GB is justified.

Actual `dbstat` `_data` + `_idx` allocation is **8,192 bytes per tokenizer**,
including unused page space. Each uses two 4,096-byte pages, so equal allocations
here hide payload differences: data BLOBs are 577 bytes unicode61 / 1,949 trigram;
logical SQL-value payloads including `_idx` are 579 / 1,951 bytes. Full definitions
and per-object payload/unused bytes are in raw.json. These exclude stored content,
docsize/config, corpus, probe tables, freelist and file header; neither measure is
total FTS/database storage or a frozen product field-projection decision.

## Design recommendations and unfinished product work

Measured behavior supports keeping a distinct, explicitly bounded short-query
path and reporting its unsearched scope. A trigram index can recover demonstrated
Japanese substrings of at least three scalars; unicode61 alone cannot guarantee
literal Japanese substring recall. These are recommendations from this corpus,
not general recall/performance guarantees or a finalized schema/field policy.

#18 still owns production FTS projection, synchronization triggers, rebuild,
weighted candidate merging/ranking, PDF extraction and annotation integration,
query/result/time/byte bounds, continuation and the chosen case/normalization
contract. #25 owns actual jobs/IPC/lifecycle; #21 owns GUI search. None were wired
or simulated here. #30 still owns representative 16GB/PDF/GUI/total-RSS/CPU/I/O and
cross-interface/failure acceptance. No DESIGN §16 target is claimed achieved.

## Validation and review status

All required local checks passed; [commands, counts and hashes](SEARCH_EXPERIMENT_2026-11-07/checks.json)
and eight accompanying `.log` files retain exact exit status and timings (only trailing
blank lines normalized; both raw/stored SHA-256 retained):

- `cargo fmt --all -- --check`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- `cargo test --workspace --locked`: 189 passed, zero failures/ignored;
  includes the registered example's three real SQLite/literal/bounds regressions,
  existing 1,000-record round trips, and compile-fail doctest.
- `cargo build --workspace --locked`.
- Architecture validator; migration validator against exact prepared base `9e7e66e3`.
- Python discovery: 25 passed, including three new summary/scope/overwrite regressions.
- `git diff --check`; summary/CSV regeneration and source/environment/raw hashes.

[Protected-file integrity record](SEARCH_EXPERIMENT_2026-11-07/integrity.json)
confirms root manifest, Cargo.lock's 636 packages, both SQL files and migration
manifest are byte-identical to the prepared base. The sole shared-file change is
an additive storage example registration using existing dependencies. No dependency
edge, new package, version or production source was needed.
Independent Opus medium review and final CI integration are root-owned; this
implementation did not invoke child agents or CLI reviewers. CI is inspected once
after creating the ready PR; pending checks/review are not passed acceptance.
