# #13 BibTeX/RIS worker adapters and export validation

Original base: `18a1eb57597508d274dccae558a26e8dad043fd0` (#33), isolated T3 checkout.
Integration base: `427f130e0ba56d40327451bd5ef28b963dffefb6` (merged #14).
The published #13 implementation was fast-forwarded intact before review fixes; current main
was then normally merged, preserving both features.
Contract/fidelity/resource decisions: [bibliography-exchange ADR](../ADR/bibliography-exchange.md).
No GUI, IPC, provider, packaging, native runtime or schema implementation is claimed.

## Executed checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/check_architecture.py
python3 scripts/check_migrations.py --base 427f130e0ba56d40327451bd5ef28b963dffefb6
python3 -m unittest discover -s scripts -p 'test_*.py' -v
git diff --check
git diff --exit-code 427f130e0ba56d40327451bd5ef28b963dffefb6 -- crates/storage/migrations
```

Passed: fmt, warnings-as-errors workspace clippy, **156 Rust tests including doctests**,
locked workspace build, architecture (12 crates), migration validator (**2 immutable
migrations**, explicit base preserved), **18 Python tests**, whitespace and unchanged
migration-tree comparison. The real-DB adapter tests run in the workspace suite; the
focused behavior command was also executed directly:

```sh
cargo test -p cm-daemon --locked --test bibliography_behavior
```

Integration tests live in `cm-daemon`, the allowed metadata+storage composition layer.
There is **no metadata→storage dev dependency** and no added driver/framework in core/domain/
protocol. Combined main's DOI resolver/providers, production reqwest/Tokio HTTP/TLS features,
exact httpdate pin, core metadata_resolve, and registered `doi_import` test target remain intact.
The main lockfile was used as the baseline and `cargo metadata --format-version 1`
resolved only local dependency edges;
no lock package entries were hand-spliced. Parsed lock inspection verifies **636→636 package
identities/checksums retained**, including all **573 original + 63 HTTP/TLS/helper packages**.
Only cm-daemon's local test serde_json/sha2 edges are added relative to current main; no new
packages, upgrades, checksum edits, pin changes or relaxed features. Both original-base and
current-main migration comparisons preserve the same two immutable migrations.

Combined workspace testing includes **core resolver tests, 20 DOI metadata tests and 3 DOI
SQLite integration tests**, along with all existing library/import/attachment tests and the
BibTeX/RIS suites below. No #14 provider code or DOI behavior was rewritten to integrate #13.
The combined workspace test command completed in 177.56 s; its two 1,000-record DB tests
ran in parallel and completed in 124.24 s. This is execution evidence, not a speed target.

## Observable acceptance evidence

`crates/daemon/tests/bibliography_roundtrip.rs`: **3 tests**, including one real-DB
end-to-end run for each of the **1,000-record BibTeX and 1,000-record RIS source files**.
The authored source files, generator, sizes and SHA-256 are in [fixtures](../../fixtures/README.md).

For each format the test:

1. Parses all 1,000 source records and checks zero file syntax diagnostics, source hash,
   representative original field values and ordered literal/person/editor names.
2. Rejects a 1,000-row batch request, then explicitly selects ten ranges of 100. Reviews
   each actual normalized retained preview and commits exactly 100 `Create` decisions.
   Thus **20 explicit atomic batch commits / 2,000 created fixture papers** overall;
   no importer policy/budget override. Database and WAL bytes stay identical during each
   preview. Commit row outcomes retain original whole-file hashes and physical source lines.
3. Reopens the real on-disk SQLite library and compares every saved title, Bibliography
   (including extra_json and citation keys), and ordered external IDs with the reviewed draft.
4. Exports all **1,000**, explicit-ID **3**, and direct collection **300** records through the
   common export service and real #12 repository, reparses each output, and compares all
   normalized model fields. The first three reviewed batches alone belong to that collection.
5. Additionally exports all 1,000 through **each** format serializer and compares reparsed
   common fields. Across both source formats this is **6,606 reparsed exported record
   comparisons**, plus **2,000 reopened DB comparisons** (8,606 total). The exported comparisons
   use this implementation's own serializers/parsers **with cm-v1 enabled**; they verify DB,
   batching, selection and model preservation, not independent external interoperability.
   The 1,000-record fixtures repeat templates and do not establish varied exporter coverage.
   Original source-file bytes remain unchanged.

`crates/daemon/tests/bibliography_behavior.rs`: **7 tests** covering observable behavior:

- Unterminated BibTeX values/macros, missing RIS ER and malformed lines recover valid
  neighboring records with original physical row/file diagnostics; error-marked drafts
  cannot Create, but explicit Skip permits valid neighbors to commit.
- Invalid UTF-8/UTF-16/NUL, input/record/record-count/brace/JSON/output limits, no macro or
  crossref expansion, repeated mapped field/alias conflicts, invalid private extensions.
- Native Unicode, escaped punctuation/literal braces/backslashes, protected groups,
  multiline `and`/corporate names and ordering, unknown native fields, native publisher/
  custom output; finite standard accents decode to Unicode and unknown commands remain
  literal with diagnostics.
- For **both adapters**, exact DOI duplicate, supplied self-key preservation, conflicting
  key `Collision_2`, missing-key proposal, byte-unchanged preview/cancel, cancelled-handle
  rejection, and one explicit selected-title Merge / one Create / one Skip. Original PDF
  source bytes and managed blob bytes, SHA-256/attachment metadata/association, existing
  collection/tag memberships and approved additive membership remain intact.
- Exact extension retention of ORCID, custom roles/order, arbitrary non-object extra_json,
  external IDs/date/type; edited public fields with stale extensions fail the row.
- Literal multiline record markers, folded/empty lines and CRLF framing preserve records;
  standalone CR export fails explicitly. Core output-budget failure returns no partial
  report; a subsequent normal export still sees all 129 original budget-probe papers.
- All exports exclude trash; selected missing/trashed/duplicate IDs and unsafe BibTeX keys
  fail explicitly. Reports contain the PDF/annotation/relationship migration notice.

`crates/daemon/tests/bibliography_interop.rs`: **12 new regression tests** address the posted
[Opus findings](https://github.com/ks250206/citation-manager/pull/43#issuecomment-6689942186):

- Independently authored exporter-shaped fixtures/expected JSON check **4 source records and
  8 cm-v1-stripped same/cross-format exports**, plus direct expected native author/DOI/URL/@
  text. Expected names are actual Unicode (Jörg Schröder), not serializer-generated strings.
- Standard months retain usable bibliography with warnings. Defined/undefined identifiers
  and `#` expressions retain exact raw syntax without expansion and cannot Create until
  resolved; an explicit Skip allows usable neighbors to commit through the real #33 service.
- Every requested accent family, ss/o/aa/l, protected/person/corporate variants, unknown raw
  commands and literal braces/backslash are observable. `{Smith}, {John}` is a person.
- Normal/bad/normal tests in both formats recover physical start lines after 300 authors,
  64 KiB fields/folded values, excessive fields, serialized DTO overhead and brace/JSON depth.
  Deep extension JSON is rejected iteratively before recursive parsing. No partial record
  is accepted, while the **32 MiB total draft budget remains a fatal error**. Original
  16 MiB and 10,000-block fatal checks remain in the behavior suite.
- Public verbatim DOI/URL punctuation is exact; unsafe braces/backslashes/newlines explicitly
  fail. General text emits literal `{@}`, not `\@`. Both date/year field orders produce
  non-creatable diagnostic rows; RIS type conversions and case-protection losses are specific
  per type/field, on both exports where applicable.

Regression sensitivity was checked in this isolated checkout by temporarily replacing only
three metadata source files with their `ff52a37` contents, running this new suite, and restoring
all corrected bytes in a `finally` block: **0 passed / 12 failed on original**. Corrected suite:
**12 passed**. This did not create another checkout, use the original implementation worktree,
execute TeX/macros, or read any actual user bibliography. It is implementation testing, not
an independent review. Full workspace checks were rerun after restoring the fixes.

The remaining existing workspace tests exercise #33 atomic rollback, retained decisions,
stale previews, duplicate/legacy DOI ambiguity and #12 organization/query semantics. Their
policies were reused without source edits.

## API/source verification

The find-docs skill was executed with:

```sh
npx ctx7@latest library serde_json 'Rust serde_json Value from_str to_string preserving unknown metadata fields'
npx ctx7@latest docs /serde-rs/json/v1.0.154 'Value from_str to_string to_value from_value recursive depth limit'
```

Results pointed to the primary tagged [serde-rs/json v1.0.154 README](https://github.com/serde-rs/json/blob/v1.0.154/README.md).
API usage was checked against [pinned serde_json docs](https://docs.rs/serde_json/1.0.154/serde_json/),
[pinned SHA-256 Digest docs](https://docs.rs/sha2/0.10.9/sha2/), pinned local registry sources
and actual locked compilation. No third-party parser dependency was predeclared; the adapters
implement the explicitly bounded subset described in the ADR rather than adding an unreviewed
parser/transcoder or executable TeX path. Format references consulted were the
[BibTeX format overview](https://www.bibtex.org/Format/),
[RIS tag map](https://github.com/aurimasv/translators/wiki/RIS-Tag-Map), and primary
[Zotero RIS translator](https://github.com/zotero/translators/blob/master/RIS.js).
These references do not imply complete compatibility with all variants.

For this review revision, find-docs was run first with
`npx ctx7@latest library biblatex 'BibTeX verbatim DOI URL fields brace protection TeX accents'`.
Its results covered style/utility packages, not the core format. Primary fallback verification:
[biblatex manual source](https://github.com/plk/biblatex/blob/dev/doc/latex/biblatex/biblatex.tex)
§Data Types (DOI verbatim, URL URI/verbatim) and §Case Changing (brace protection);
[Biber character recoding data](https://github.com/plk/biber/blob/dev/lib/Biber/LaTeX/recode_data.xml)
for accent/letter semantics; [LaTeX kernel ltspace.dtx](https://github.com/latex3/latex2e/blob/develop/base/ltspace.dtx)
for `\@` as `\spacefactor`, not a printable at-sign. These primary sources were read, never
executed. No dependency activation, version or checksum change was needed. No Biber/TeX
runtime/rendering compatibility test is claimed.

## Limits and handoff

### Final P3 corrections after independent re-review

Opus 5.5 medium re-reviewed `b858e4d` and confirmed the original nine findings resolved,
including independent probes and the public-field, DOI and 1,000-record tests. The report
is recorded at PR comment6690698482. Root addressed the four new nonblocking P3s:

- All twelve standard month tokens survive cm-v1-free public BibTeX output as bare tokens;
  RIS reports a field-specific month loss instead of relying on the generic extension notice.
- Once a multiline `@` boundary is ambiguous, **all** subsequently recovered drafts carry
  an error identifying the original line. Real #33 preview cannot commit them as Create;
  earlier safe records can still commit if recovered rows are explicitly skipped.
- Literal general-text caret/tilde use `\textasciicircum{}` / `\textasciitilde{}` and decode
  without cm-v1. The [official LaTeX2e source](https://latex-project.org/help/documentation/source2e.pdf)
  defines these text commands. No TeX/Biber rendering or execution is claimed.
- One outer-group predicate is shared by BibTeX and RIS; separately protected RIS family/
  given components remain person names, while a whole grouped organization remains literal.

Four additional regression tests cover these public expectations and actual rejected/Skip
commit behavior. Focused bibliography behavior7 + interoperability16 tests pass. Root reran
the full workspace:153 Rust tests including the two1,000-record round-trips,18 Python tests,
fmt, locked warnings-denied clippy/build, architecture, immutable migrations against427f130
and whitespace all passed. Exact-head independent re-review and CI remain pending.
Dependency pins, Cargo.lock and immutable migrations are unchanged by this correction.

### Tie regression correction

The independent `bee779d` recheck found that raw BibTeX ties were being emitted as visible
tilde glyphs. The finite text decoder now maps unescaped ties to U+00A0; BibTeX output maps
U+00A0 back to `~`, keeping literal `\textasciitilde{}` distinct. The authored
`fixtures/bibtex/ties.bib` includes Fig.~1, D.~E. Knuth, corporate ties, literal tildes,
accent syntax and verbatim DOI/URL tildes.

Two new regressions check public output without cm-v1 and genuine #33 preview/SQLite save/
read/export. Shared normalization still collapses NBSP to ordinary spaces; the warning and
raw source syntax survive review/storage, and both public exports report the field-level
spacing limitation. Saved titles and names never acquire visible tildes from input ties.
No shared normalization rule was changed. A third regression fixes the existing optional
RIS protected-family comma observation, including public RIS reparse.

Focused bibliography behavior7 + interoperability19 tests passed. Root also reran the full
workspace:156 Rust tests including both1,000-record SQLite round-trips and the compile-fail
doctest,18 Python tests, fmt, locked clippy/build, architecture, immutable migrations against
427f130 and whitespace all passed. Exact-head re-review/CI remain pending. No actual TeX
rendering or user library was used.

- BibTeX/RIS transfer bibliography only. PDFs, attachments, annotations, collection/tag
  relationships and internal IDs need separate migration. Source provenance is reported,
  not newly persisted. Existing physical files/relationships are preserved, not exported.
- Full fidelity for otherwise nonrepresentable data depends on the private cm-v1 extension;
  other tools may ignore/drop it. Unsupported constructs and losses are explicit diagnostics.
- Files larger than #33's preview budget require smaller, separately reviewed batches;
  failure/cancel of a later batch does not roll back prior commits. Export pages do not
  provide a file-wide snapshot. Neither 1,000-record test establishes a performance target.
- #33's bibliography-only legacy percent-escaped DOI ambiguity remains documented in the
  ADR; no bulk rewrite, broadened identity rule or migration was introduced.
- Default build does **not** prove native GUI, PDFium runtime, VM, browser/packaging or signing.
  Those lanes were not started. Main's #14 DOI/providers/metadata_resolve are integrated
  unchanged; held #34, #23/#25, future GUI/IPC and other worktrees/configuration stay untouched.
- Independent different-model review, final CI/review assessment and merge belong to the root
  agent. No reviewer/subagent/CLI review was launched and no PR was merged. One CI
  inspection is made at PR delivery; its live state is reported separately, not called passed
  in this document.
