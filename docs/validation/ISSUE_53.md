# #53: corrected bundled-SQLite Japanese tokenizer experiment

The current result is the review-corrected **30-record** corpus, measured at clean
commit `8b7b025ab9413413707237e4a5c88d802b5b5ab0` on 2026-11-07 JST, integrating main
`5f0fb1dfa139afc567a492a309c1f5f4728ffbed`. The source/expected sets were committed
before this corrected run. Later changes retain its evidence and this report.
Only #53 is covered: #18 still depends on #25; #30, GUI #15 and held #34 are not
completed, and #23/#25 gates remain closed.

## Reproduction and provenance

```sh
python3 scripts/search-experiments/run.py --out a-new-issue53-run
cargo test --locked -p cm-storage --example search_experiment
python3 -m unittest discover -s scripts -p 'test_*.py'
```

[Method](../../scripts/search-experiments/README.md),
[synthetic CC0 fixture](../../scripts/search-experiments/fixture.json),
[expected ID sets](../../scripts/search-experiments/expected.json), and
[license](../../scripts/search-experiments/LICENSE.fixture) are retained.
No user/third-party literature or real library is used. The original claim that
expectations were authored before execution rests on the author's report: original
oracle and instrument shared commit `0d7942f`, so git cannot establish that ordering.
The revised expectations were manually enumerated using isolated texts and earlier
observed semantics, not generated from the corrected run's output. They were
committed at `ca1df7662982f9e877144695344075efe6ce2803` before rerunning; that chronology
does not imply that the expectations were uninformed by prior measurements.

The original [23-record data](SEARCH_EXPERIMENT_2026-11-07/raw.json) and
[original check logs](SEARCH_EXPERIMENT_2026-11-07/checks.json) remain historical.
Their co-located width/kana/accent variants masked cross-form behavior; their
67-case/2,077-call timings are superseded, not combined with this run.

Current [environment/source/binary hashes](SEARCH_EXPERIMENT_REVIEW_2026-11-07/environment.json),
[raw observations](SEARCH_EXPERIMENT_REVIEW_2026-11-07/raw.json),
[summary JSON](SEARCH_EXPERIMENT_REVIEW_2026-11-07/summary.json),
[CSV](SEARCH_EXPERIMENT_REVIEW_2026-11-07/summary.csv) and
[Cargo build log](SEARCH_EXPERIMENT_REVIEW_2026-11-07/build.log) record the clean
measured source and real optimized executable. Raw/environment hashes and source
hashes are independently recomputable; summary/CSV regenerate from raw using the
runner. The runtime is pinned bundled SQLite **3.54.1**, through
rusqlite 0.41.1 / libsqlite3-sys 0.39.1, not Python/system SQLite. Its source ID is
`2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. All 51 compile options are in raw;
FTS5 and DBSTAT are enabled, ICU is not. Eight declared SQLite-specific environment overrides, including LIBSQLITE3_FLAGS
and the three SQLITE_MAX_* settings, are rejected by presence before output creation,
even with an empty or `0` value. Their values are also recorded. Other compiler
flags are recorded rather than exhaustively prohibited; this is not a hermetic build.

Environment: Mac16,6 Apple M4 Max, **36 GiB**, arm64, macOS 27.2 (26B123),
Rust/Cargo 1.99.1. Release opt-level 3, debug assertions disabled. No root test/build
checks ran concurrently with the measurement; other processes and OS caches were
uncontrolled. The owned private TempDB uses canonical paths, SQLite NOFOLLOW,
0700, DELETE journal and synchronous FULL, then is removed. There is no DB path
argument. Output rejects overwrite and checks target/parent with lstat before mkdir;
this is not a defense against a malicious same-user path replacement race.

## Corrected behavior: variants in separate records

IDs 7/24/25 contain full-width/ASCII upper/ASCII lower strings separately. IDs
8/26/27 contain half-width/composed/decomposed kana separately, and 28 contains
plain `カ ラス`. Accent forms are separated at 6/30 and 20/29. Literal reference
folds ASCII A-Z only: no NFC/NFKC, accent, width or non-ASCII case folding. Query
length counts Unicode scalars, not bytes or graphemes. Every expected hit set is
asserted by real SQL; CI also asserts the isolated unicode61 token sequences.

| Query | Literal expected IDs | unicode61 MATCH IDs | trigram MATCH IDs |
|---|---|---|---|
| `電` | 1,2,3,9,10,16,17,22 | 9 | none |
| `電池` | 3,9,10,16,17,22 | 9,17,22 | none |
| `固体電` | 1,3,16 | none | 1,3,16 |
| `電解質` | 1,2,16 | 2 | 1,2,16 |
| `ABC` | 18,24,25 | 18,24,25 | 18,24,25 |
| `ＡＢＣ` | 7 | 7 | 7 |
| `ｶﾞﾗｽ` (half-width) | 8 | 8 | 8 |
| `ガラス` (NFC) | 26 | 26 | 26 |
| `ガラス` (NFD / decomposed dakuten) | 27 | 27,28 | 27 |
| `カ ラス` (plain kana with space) | 28 | 27,28 | 28 |
| `café` (NFC) | 6 | 6,23,30 | 6,23 |
| `café` (NFD) | 30 | 6,23,30 | 30 |
| `é` (NFC) | 6,20 | 20,29 | none |
| `é` (NFD) | 29,30 | 20,29 | none |
| `alpha beta` | 11 | 11,12 | 11 |
| `alpha, beta` | 12 | 11,12 | 12 |

The important measured boundary is **dakuten splitting**: unicode61 emits one
`ガラス` token for composed document 26, but `カ` and `ラス` for decomposed document
27. A composed query misses 27; a decomposed query matches both 27 and plain-kana
28. Conversely `カ ラス` hits decomposed glass at 27 as well as its literal record
28. This is a false positive relative to this literal oracle, not Japanese word
understanding. Neither tokenizer performs width/NFC normalization: trigram also
keeps the three kana spellings separate. No general Japanese recall guarantee follows.

For Latin accents, unicode61's configured remove_diacritics=1 maps both isolated
é/e+combining-acute to token e. Trigram has remove_diacritics=0 and distinguishes
NFC/NFD `café`/`café`; the **NFD** probe is caf, afe, fe+combining-acute. unicode61
phrases can match across punctuation (`alpha beta` matches rows 11 and 12).
All instance vocabulary is retained: 73 occurrences/45 terms for unicode61,
261 occurrences/173 terms for trigram. Candidate IDs mean the SQL union of documents
sharing any probe token, not internal planner candidates. Exact MATCH hits remain
separate. All text is bound; quoted phrase escaping is inside the FTS timer.

## Separate bounded short-query series and timings

**69 cases = 30 queries × 2 tokenizers + 9 fallback cases**, with 5 untimed warmups
and 31 timed repetitions each: **2,139 timed calls**. Same connection and prior
setup/probes make these warm observations; there is no cold-cache control.
Nearest-rank p50/p95 use ranks 16/30 of N=31, no interpolation or confidence interval.
FTS includes phrase quoting, prepare/bind/step and ordered IDs. Fallback includes
cap/length checks, filtered count, capped ordered text fetch and ASCII literal
comparison/ID/byte-count collection. Setup, probes/oracle/scope diagnostics and
serialization are excluded. Not import, PDF extraction or product service throughput.

| Query | unicode61 p50 / p95 ms | trigram p50 / p95 ms |
|---|---:|---:|
| `電` | 0.021500 / 0.052834 | 0.004958 / 0.005167 |
| `電池` | 0.017083 / 0.032458 | 0.005292 / 0.006333 |
| `固体電` | 0.018209 / 0.018959 | 0.018500 / 0.026875 |
| `電解質` | 0.006209 / 0.008833 | 0.006958 / 0.015416 |
| `ガラス` (NFD / decomposed dakuten) | 0.006958 / 0.009292 | 0.007417 / 0.008083 |
| `カ ラス` (plain kana with space) | 0.019042 / 0.020583 | 0.007334 / 0.010875 |

Short-query fallback has explicit bucket filter and row cap 1..64; no hidden
unlimited text search precedes LIMIT. Filter counts can scan the bucket index,
so this is a row-count bound, not a timeout/byte/SQL-operation budget. Materials
contains ten records; a complete materials search still excludes 20 other records,
including outside-filter match 17. The expanded language bucket has 18 records.

| Case | cap / read | returned count | unsearched in filter | status | p50 / p95 ms |
|---|---:|---:|---:|---|---:|
| two_cap2 | 2 / 2 | 0 | 8 | partial_unsearched | 0.010959 / 0.011541 |
| two_cap4 | 4 / 4 | 1 | 6 | partial_unsearched | 0.017000 / 0.017458 |
| two_complete | 64 / 10 | 5 | 0 | complete_within_filter | 0.013041 / 0.013417 |
| one_cap4 | 4 / 4 | 3 | 6 | partial_unsearched | 0.021125 / 0.021500 |
| one_complete | 64 / 10 | 7 | 0 | complete_within_filter | 0.012542 / 0.012708 |
| ascii_two | 64 / 10 | 3 | 0 | complete_within_filter | 0.012542 / 0.013125 |
| punctuation_partial | 3 / 3 | 0 | 15 | partial_unsearched | 0.010625 / 0.010792 |
| absent_partial | 2 / 2 | 0 | 8 | partial_unsearched | 0.010416 / 0.010583 |
| empty_filter | 2 / 0 | 0 | 0 | complete_within_filter | 0.018750 / 0.019000 |

Empty hits plus partial_unsearched does not mean zero matches. For two_cap2, all
five expected in-filter hits remain unsearched. Expected-oracle recall is meaningful
only for this synthetic corpus; empty denominators remain null. IDs of unsearched
and excluded records and compared bytes are in raw. Production continuation,
result/time/byte bounds and field/collection/PDF scopes remain unimplemented.

Both index `_data` + `_idx` B-trees allocate **8,192 bytes**, two 4 KiB pages each.
Data BLOBs are 588 bytes unicode61 / 1,818 trigram; logical SQL-value payloads
including `_idx` are 590 / 1,820 bytes. Definitions/per-object unused space are
retained. This excludes content/docsize/config/corpus/probes/freelist/file headers:
it is neither total FTS storage nor production field-projection cost. Tiny timing
and allocation figures cannot be extrapolated to 10,000 papers/20,000 pages/16GB.

## Design consequences and validation status

Keep short-query fallback and unsearched scope distinct. Trigram recovers the
shown 3+ scalar substrings, but width/NFC folding and decomposed dakuten require an
explicit product normalization/position policy, with regression cases like these.
unicode61 alone does not guarantee Japanese substring recall or literal precision.
These are measured constraints/recommendations, not a chosen product schema.
#18 owns projection/trigger synchronization/rebuild/ranking/normalization/extraction;
#25 owns jobs/IPC/lifecycle and #21 the GUI. None were simulated or enabled here.
#30's full-load/RSS/security/cross-interface requirements remain open.

Original baseline checks passed 189 Rust tests and 25 Python tests, with required
fmt/Clippy/build/validators; their retained logs are historical, not claims for the
corrected fixture. The first root follow-up passed the full locked workspace **189 tests**, Python **27 tests**,
fmt, workspace all-targets locked Clippy, locked build, architecture, base-specified
migrations and diff-check. [Root commands/counts/output hashes](SEARCH_EXPERIMENT_REVIEW_2026-11-07/root-checks.json)
record these results. Summary/CSV regenerated exactly; recorded source/environment
hashes matched. Independent exact-head recheck and final CI remain pending. Production source,
Cargo.lock's 636 pins and both immutable migrations/manifest remain unchanged;
the only shared addition is the storage experiment example registration.

## Runner guard and main integration follow-up

The subsequent review found no P0–P2. Root corrected the presence-versus-nonempty
check for build overrides, added the three SQLITE_MAX_* settings to rejection and
recording, and tested empty, `0` and nonempty values for all eight declared names.
Unset those variables; no disabling-value exception is accepted. NFC/NFD and kana
form labels now distinguish visually identical table entries. Main #52 was normally
merged without changing its pure modules or any search production code. The current
clean-source run above replaces the earlier corrected timing files; that earlier
run remains in commit `18b2eb9`, and its expected sets/fixture/Rust instrument are
unchanged. Current timings are not pooled or compared for speedups. Final integrated checks passed **210 workspace tests** (including the compile-fail
doctest and three SQLite example tests), **27 Python tests**, fmt, all-targets locked
workspace Clippy, locked build, architecture, migration against main `5f0fb1d`, and
diff-check. [Final integrated commands/counts/output hashes](SEARCH_EXPERIMENT_REVIEW_2026-11-07/root-integration-checks.json)
are separate from the historical 189-test checks above. Independent final-head
review and CI remain pending.
