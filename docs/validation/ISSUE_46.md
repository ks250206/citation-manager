# #46 search DSL execution evidence

2026-11-07 JST. Prepared checkout:
`/Users/ryojikanno/.t3/worktrees/citation-manager/t3-8cc09827`, branch
`t3/search-dsl-parser-ast`, base main
`fd50af1fe385c30b8581f06c90247630f27caf55`. Rust 1.99.1
(`58cf8f9c3`, 2026-10-13), Python 3.14.0. Only this prepared checkout was edited.

This evaluates the feature-local #46 contract, not completion of parent #18.
The exact grammar, normalization, duplicate handling, byte spans, recovery and
limits are in the [DSL ADR](../ADR/search-dsl.md). Owned changes are only the
core search module, its `lib.rs` registration, focused core tests and these two
DSL documents. No benchmark/example/script or shared DESIGN/UI evidence changes.
No PR #45 source was imported.

## Meaningful exercised behavior

The focused tests in `crates/core/tests/search_query.rs` cover:

- All five DESIGN §10.3 example strings and each expected typed clause. Multiple
  conditions retain order and duplicates (including contradictory year ranges)
  as conjunctions, without last-value-wins or a partially accepted AST.
- Unicode whitespace, Japanese and emoji text, byte-accurate source clause spans,
  precise error substrings and EOF positions; non-whitespace controls fail.
- Phrase whitespace preservation, quote/backslash escapes, malformed embedded
  quotes, unclosed quotes, unsupported escapes, empty input/values, unknown keys
  and unsupported `has` values. Corrections are bounded example edits of whole
  clauses and reparse successfully when the isolated error is replaced.
- ASCII-insensitive field keys and has values; preserved term/phrase case;
  existing DOI wrapper/percent-literal/one-decode behavior; common TagKey Unicode
  lowercase expansion and distinct non-NFC/non-full-folded identities; shared
  case-preserving collection display-name rules. No duplicate normalizer exists.
- Exact inclusive four-digit year ranges; reversed/open/malformed/zero/signed/
  Unicode-digit ranges fail. OR/explicit AND/NOT/NEAR/grouping/wildcards/boosts
  fail as syntax, while quoted apparent SQL/FTS and field punctuation remain
  untrusted AST data. There is no execution or SQL/FTS output to test here.
- Exact 8192-byte input boundary and a boundary inside a multibyte character;
  1 MiB input rejected early; 64/65 clauses, counting invalid clauses too;
  1024/1025 raw bytes, quotes counting against raw size, 4-byte scalar boundaries;
  256/257 shared name scalars and 128/129 expanding `İ` tag inputs; 15/16/17/64
  invalid clauses distinguish examined errors and unexamined suffixes.

Reproducible bounded malformed-input regression (no new package or fuzz runtime):

```sh
cargo test -p cm-core --test search_query --locked bounded_malformed_corpus_is_reproducible_and_utf8_safe -- --exact
```

One test streams 22,621 exhaustive strings of length 0–4 over the documented
12-scalar alphabet (including Unicode, quote, backslash, colon, whitespace and
NUL), then 2,048 fixed-seed xorshift64 longer combinations (0–511 body scalars)
with rotating field/quote prefixes. The 24,669 cases include valid and invalid
inputs. Each case checks determinism, successful AST bounds, diagnostic/correction
bounds and UTF-8-safe spans; every static replacement is independently parseable.
This is finite regression evidence, not exhaustive fuzz coverage, a memory/latency
measurement or 24,669 separate tests. Focused tests initially reported 14/15:
`C:\papers` had a wrong expected escape error; the colon correctly makes it an
unknown field. The fixture was corrected and both branches are now tested.
The initial clippy run rejected hex seed digit grouping; only grouping was fixed.

## Required checks

Original `f34398f` results after the complete workspace run: 171 unit/integration tests and
1 compile-fail doctest passed, including the 15 new DSL tests, with 0 failures or
ignored tests. Both pre-existing 1000-record BibTeX/RIS real-SQLite roundtrips
completed successfully (their 3-test target took 127.66 s). No test filters were
used for the workspace checks. `target/issue-46-workspace-tests.log` contains local
full test output (generated, not committed).

| Command | Result |
|---|---|
| `cargo test -p cm-core --test search_query --locked` | passed: 15 new tests, including the finite corpus |
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed after seed-format correction |
| `cargo test --workspace --locked` | passed: 171 unit/integration + 1 compile-fail doctest |
| `cargo build --workspace --locked` | passed: default workspace build |
| `python3 scripts/check_architecture.py` | passed: 12-crate boundary/platform isolation |
| `python3 scripts/check_migrations.py --base fd50af1fe385c30b8581f06c90247630f27caf55` | passed: 2 unchanged checksums/identities, immutable base preserved |
| `python3 -m unittest discover -s scripts -p 'test_*.py' -v` | passed: 18 tests |
| `git diff --check` | passed; staged whitespace check also run before commit |

Direct byte comparison of Cargo.lock to the base succeeds. Python `tomllib`
confirms exactly 636 locked package entries; no identities/versions/checksums
changed. Workspace/core manifests, toolchain, schema and migration files remain
unchanged. The parser has no new dependency or transport serialization.

## Independent review follow-up

The [Opus 5.5 medium review of `f34398f`](https://github.com/ks250206/citation-manager/pull/48#issuecomment-6695994891)
found no blockers and verified 218 tracked hashes unchanged. It ran the 15 focused
tests plus an independent 32-input probe. Root addressed its three low/nit notes:
uppercase-only reserved operator words preserve natural English connectives;
known-field lexical corrections preserve the predicate kind; and the EBNF now
defines its character/value productions explicitly. Two new semantic regressions
exercise lowercase/mixed-case English and all five fields' empty/escaped/quoted
errors with Japanese prefixes and complete replacement spans. There are now 17
focused tests; the finite 24,669-case regression is retained.

Follow-up checks passed: `cargo test -p cm-core --test search_query --locked`
(17 tests), `cargo test -p cm-core --locked` (38 total including the compile-fail
doctest, overlapping the 17), `cargo fmt --all -- --check`, warnings-denied locked
workspace all-target Clippy, locked workspace build, architecture, immutable
migration checks against `fd50af1` and whitespace. Root's focused core output is
`/tmp/citation-manager-pr48-review/root-core-tests.log`. The full workspace and
Python suites were not repeated locally for this four-file follow-up; their earlier
counts above belong to `f34398f`. Final-head full CI and an independent narrow
recheck are separately required. No dependency/manifest/SQL or AST shape changed.

## Unrun / integration handoff

Native-feature/GPUI builds, GUI/browser/VM, native host, signing/notarization,
PDFium runtime, actual user library, search execution/FTS/ranking, persistent jobs,
daemon startup/IPC/grants and performance measurements were **not run** for #46.
Default workspace tests exercise pre-existing temporary-DB and loopback fixtures;
their success does not prove those unconnected product paths. Python PDFium-tool
tests check their existing fixture/pin contracts, not a new native runtime run.

No child agents or CLI reviewers were used. Root owns independent Opus 5.5 medium
read-only review, CI integration and merge; independent review is outstanding at
this implementation handoff. CI is inspected once after PR creation and reported
with the exact pushed SHA at handoff, without polling/watching or merging.
#15 GUI acceptance remains incomplete, PR #45 remains unrelated/Draft, #34 remains
HELD, and #23/#25 gates remain closed. This PR closes only #46 when integrated.
