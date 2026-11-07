# #53: bundled SQLite tokenizer and short-query experiment

This is an isolated measurement child of #18, not a product search backend. It
creates only an owned private TempDir DB (canonicalized for macOS `/var`) and
removes it on exit. There is no database path argument. No user library, production
migration, source projection, LibraryQuery, DSL, daemon/job or GUI is used.

```sh
python3 scripts/search-experiments/run.py --out my-new-run
# Default: release, locked; 31 repetitions and 5 warmups per case
cargo test --locked -p cm-storage --example search_experiment
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

`--out` is a new single directory name under `target/search-experiments`. Existing
output is refused, including existing symlinks. It is never an arbitrary DB path.
Repetitions are 1..100 and warmups 1..20. The binary accepts only these two bounded
integer arguments. Do not run checks concurrently with a measurement. Host activity
from other processes is uncontrolled. No cold-cache or application performance
claim follows from this tiny fixture.

## Corpus and independent expectations

[fixture.json](fixture.json) is a fixed enumeration of 30 original synthetic strings,
with no random generation, imported papers, PDF text or private data. The synthetic
strings and [expected.json](expected.json) are dedicated under
[CC0-1.0](LICENSE.fixture). The original claim of hand enumeration before execution
rests on the author's report: git history cannot establish that ordering because
the first oracle and instrument shared one commit. The review correction manually
enumerates isolated variant expectations using fixture text and the earlier measured
tokenizer semantics; those expectations are committed before the corrected run,
not generated from its results. Rust asserts the literal, FTS-semantic and fallback expected ID
sets; a mismatch fails the run. Regression failures must be investigated, not
resolved by automatically regenerating the oracle from measured results.

Coverage: Japanese contiguous/separated/mixed strings; English word/substring;
1, 2, 3 and longer Unicode scalar queries; quotes, DOI punctuation, `%`, `_`;
accented composed/decomposed Latin; half/full-width and composed/decomposed kana;
ASCII and non-ASCII case differences. Length counts scalars, not UTF-8 bytes or
grapheme clusters. In particular `e` + combining acute counts as two.
Width, kana and accent variants occupy separate records. The CI test additionally
asserts their unicode61 tokens, including the decomposed dakuten split into `カ`
and `ラス`. A plain-kana phrase has its own record and query to expose that false
positive rather than masking it inside a document containing every spelling.

**Literal rule:** fold ASCII A–Z only, then exact substring. Do not NFC/NFKC,
remove accents, fold width, lowercase Greek/accented capitals, or fully fold ß.
Thus `ABC` matches `abc`, while `Ω` vs `ω`, `CAFÉ` vs `café`, and `é` vs `e◌́`
remain distinct. FTS has its own observed tokenizer semantics; differences from
this literal oracle are preserved. This experiment does not select a product-wide
case/normalization policy.

## SQLite observations

The storage test-only example uses workspace `rusqlite =0.41.1` with bundled
`libsqlite3-sys =0.39.1`; no Python or system SQLite is executed. Runner records
source/package hashes, Cargo library artifact feature sets, runtime version/source
ID and all `PRAGMA compile_options`. It requires bundled features, rejects the eight
SQLite-specific overrides listed in `SQLITE_BUILD_OVERRIDES` (including `LIBSQLITE3_FLAGS`
and the three `SQLITE_MAX_*` build settings), even when present with an empty or `0` value,
and alternate engine features, and matches the runtime ID/version to the pinned
C source. It records build flags, binary hash, CPU/RAM/OS, toolchains and a hash of
the environment record (canonical sorted JSON before adding its hash). Existing
636 locked packages and immutable migrations are untouched.
Unset those eight variables rather than setting a disabling value. Other compiler
flags are recorded, not exhaustively prohibited; this is not a
hermetic toolchain or a guarantee that every possible compiler override is rejected.

Experiment-only FTS tables each have one `text` column, `detail=full`,
`columnsize=1`, no prefixes, default stored content:

- `unicode61 remove_diacritics 1` (other unicode61 options default).
- `trigram case_sensitive 0 remove_diacritics 0`.

Every corpus token is read using `fts5vocab(..., instance)` as term/document/column/
offset. A separate probe table receives each raw query text through a bound insert;
its instance vocabulary reports **index-time probe tokens**, not an instrumented
query-time tokenizer callback. Exact results execute real `MATCH` with a quoted
FTS phrase (embedded quotes doubled). Inputs are not interpreted as the product
DSL. All SQL text values are bound.

`candidate_ids` is a real SQL join of corpus/probe vocabularies on term, unioning
documents sharing **any** token. It is a diagnostic superset, not the internal FTS
planner's candidate list. Phrase MATCH can eliminate these candidates. Vocabulary
and candidate probing are outside timers; the exact ordered MATCH results are
validated on every warm and measured repetition.

## Separate bounded short-query series

Only 1/2-scalar inputs are accepted. `bucket = ?` is an explicit experiment filter;
it does not establish collection/PDF/field projection contracts. SQL counts eligible
rows, then fetches `id,text WHERE bucket=? ORDER BY id LIMIT ?`. The literal checker
sees at most `cap` records, cap 1..64. Result count is consequently at most cap;
there is no hidden exhaustive text scan before the LIMIT. This is a **row-count**
bound on a fixed small corpus, not a general byte, SQL-operation or elapsed-time
bound. Eligible count may read the entire filtered bucket index. The instrument
reports compared bytes. Product text budgets, timeouts, continuation and result
limits remain #18/#25 work.

Each result records actual candidates/hits, cap, filtered total, truncated flag,
`partial_unsearched` or `complete_within_filter`, unsearched eligible IDs and IDs
excluded by the filter. ID-only diagnostics are deliberately outside the timed
fallback and do not search unselected text. Empty hits plus `partial_unsearched`
never means the filtered search found no matches. Even complete results describe
only the filter. In this synthetic experiment only, the independent oracle makes
recall within the filter calculable. Empty oracle denominators report JSON null,
not fabricated 0%/100% recall. Oracle knowledge of unsearched hits is not a product
capability or permission to label unsearched scope zero.

## Timing and size definitions

All 69 cases run sequentially: 30 queries × 2 tokenizers, then 9 fallback cases.
Each case has one correctness observation plus 5 untimed calls before its 31 timed
calls, using the same populated connection. Setup, vocabulary/oracle probes and
OS caching make this warm; there is no cold-cache control. The default run has
2,139 timed calls. Repeated empty trigram results are still timed SQL calls.

FTS timer: `Instant` before phrase quoting, SQL prepare/bind/step and ordered rowid
collection, stopping before expected-result comparison/serialization. Fallback
timer: before cap/length validation, eligible SQL count, capped ordered id+text
prepare/bind/step and ASCII literal comparison/ID/byte-count collection. Diagnostic
scope ID queries and oracle comparisons are excluded. Seed/index construction,
probe insertion and import/PDF extraction are not benchmarked. This is **direct
fixture SQL**, not an ImportService or application engine benchmark.

Raw samples are milliseconds. p50/p95 use nearest rank `sorted[ceil(p*N)-1]`, no
interpolation or confidence interval; at N=31 they select ranks 16 and 30. Rust
calculates them and Python independently recomputes/validates them. Summary groups
never pool fallback with FTS or combine tokenizers.

`allocated_index_bytes` uses actual bundled `dbstat`: sum(pgsize) of the FTS `_data`
and `_idx` B-trees, including unused space. It excludes stored `_content`, `_docsize`,
`_config`, corpus/bucket index, probe tables, freelist and database header pages.
Per-object allocated/payload/unused bytes are included. Also report logical SQL
payload: sum length(block) in `_data`, plus length(segid)+length(term)+length(pgno)
in `_idx`; numeric `length()` measures text-representation length. These definitions
are explicit and differ from disk file size, total FTS storage and any production
field projection. Measurements occur after the seeding transaction commits, before
query probing for each tokenizer; probe objects are excluded by exact name.

## Evidence and CI

`raw.json` contains runtime configuration, all token instances, query candidates,
expected/actual hitsets, literal recall, bounded scopes, sizes and raw timings.
`summary.json` and CSV validate coverage/quantiles/hitsets/bounds. `environment.json`
contains exact source and environment hashes; `build.log` records Cargo artifacts.
The repository's existing workspace `cargo test` lane executes three SQLite/literal/
bounds tests from the registered example. Python discovery executes five summary/
coverage/output-preservation/symlink/engine-override regressions. No workflow changes are necessary.

For a retained result, regenerate summary with `summarize(raw, fixture, expected,
raw['repetitions_per_case'], raw['warmups_per_case'])` and CSV with `csv_text(rows)`
from `run.py`; these should exactly match the retained files. Timings themselves
are host-dependent and are not CI latency thresholds.

API lookup used find-docs/Context7's `/websites/sqlite_docs` and the
[official SQLite FTS5 documentation](https://sqlite.org/fts5.html), particularly
[unicode61](https://sqlite.org/fts5.html#unicode61_tokenizer),
[trigram](https://sqlite.org/fts5.html#the_trigram_tokenizer) and
[fts5vocab](https://sqlite.org/fts5.html#the_fts5vocab_virtual_table_module).
[dbstat](https://sqlite.org/dbstat.html) defines allocated B-tree page statistics.
Pinned local `sqlite3.c` (`fts5TriCreate`/`fts5TriTokenize`) and the crate build.rs
were checked for option and bundled-build behavior. Documentation guides API usage;
all reported tokenizer behavior is measured SQL. These sources do not substitute
for this run or prove full #18, #21 or #30 acceptance.
