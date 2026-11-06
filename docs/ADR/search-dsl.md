# #46: Bounded pure-core search DSL

Status: adopted for the #46 input contract, evaluated within #46. This does not
complete parent #18 or change its #25 dependency or search/job acceptance criteria.
The additive `cm_core::search::query` module implements a framework-free parser;
`cm-search`, storage, protocol and desktop are not connected to it. No dependency,
serialization derive, migration or existing public contract changes are required.

## Exact minimal grammar

```text
query       = whitespace* clause (whitespace+ clause)* whitespace*
clause      = bare-term | quoted | field ":" value
field       = "doi" | "tag" | "year" | "collection" | "has"
value       = bare-value | quoted
quoted      = '"' (quoted-char | '\"' | '\\')* '"'
year-value  = digit digit digit digit ".." digit digit digit digit
bare-value  = bare-char+
bare-term   = term-char+
whitespace  = ? Unicode scalar satisfying char::is_whitespace ?
quoted-char = ? Unicode scalar except double quote, backslash and non-whitespace controls ?
bare-char   = ? Unicode scalar except whitespace, double quote, backslash and controls ?
term-char   = ? bare-char except colon and ()[]{}*^|&! ?
```

This notation's `\"` and `\\` are exactly the two-character source escape
sequences backslash+double-quote and backslash+backslash. `digit` is ASCII 0–9.
There is no optional whitespace around `:` and no concatenation of quoted and
bare segments. A field value may be quoted, including year and has values.

- Whitespace is the pinned Rust toolchain's Unicode `char::is_whitespace` set,
  matching `str::trim`/`split_whitespace`. Space, tab, LF/CR, NBSP and full-width
  space separate clauses; zero-width space does not. No normalization is applied
  to ordinary text. Empty or whitespace-only queries fail, rather than selecting
  every record. Every decoded value needs at least one non-whitespace character.
- Bare values are nonempty runs without whitespace, double quote, backslash or
  non-whitespace control characters. Colons after the field separator are value
  data (e.g. DOI resolver URLs). Quoted characters can contain Unicode whitespace,
  colons and punctuation; quotes are removed, the two escapes are decoded, and
  other text is preserved exactly. Non-whitespace controls, other escapes and
  bare backslashes fail. `\n`, `\t`, `\uXXXX` and single-quote quoting are not
  supported; an apostrophe remains an ordinary literal character.
- Bare terms obey the bare-value rules, additionally excluding `:` and
  `()[]{}*^|&!` anywhere, a leading `+`/`-`, and whole-word uppercase
  `OR`, `AND`, `NOT`, `NEAR`. These receive explicit unsupported-syntax errors.
  Lowercase and mixed-case words (`cats and dogs`, `near`, `And`) remain literal
  terms, so ordinary English does not need quoting. This does not add Boolean
  execution: all accepted terms remain conjunctive, untrusted data. These reserved
  words and the leading-sign checks are additional semantic restrictions on `bare-term`.
  Hyphens within a word (`solid-state`), apostrophes and `%` remain literal.
  Quote text such as `"OR"`, `"a*"`, `"(a)"`, `"https://example.com"` when intended
  literally. An unquoted colon always introduces a field; `C:\papers` and bare
  URLs therefore fail as unknown fields, rather than implying extra syntax.
- Keys compare ASCII-case-insensitively: `DOI`, `Tag`, etc. are valid. Full-width
  keys and unknown/empty keys fail. No OR, explicit Boolean AND/NOT, grouping,
  prefix/wildcard, boost, SQL or FTS query language is implemented. Operators in
  field names/values are never executable: `tag:a*` is the literal tag key `a*`,
  and `collection:OR` is the literal name `OR`.

`Query.clauses` is a nonempty, flat conjunction in source order. Ordinary unquoted
text is `Term`, quoted text is `Phrase`. Every duplicate is preserved and counts
toward the term budget: repeated identical clauses are redundant AND conditions;
different tags/collections are all required; repeated disjoint year ranges are
syntactically valid conjunctions that cannot match. No last-value-wins behavior,
deduplication, conflict solving, DB lookup or satisfiability check occurs here.

## Typed values and existing normalization

- `Doi(String)` delegates **only** to `papers::normalize_doi`. Its existing wrapper
  recognition, resolver-suffix decoding exactly once, query/fragment removal from
  resolver URLs, raw-percent-literal distinction and ASCII lowercasing apply.
  Example: `doi:https://doi.org/10.1234/%41BC` becomes `10.1234/abc`, while
  `doi:10.1234/%41BC` becomes `10.1234/%41bc`. No second DOI normalizer is introduced.
- `Tag(TagKey)` delegates to `cm_domain::TagKey::new`: Unicode whitespace collapse,
  shared display-name limits and Unicode lowercase (including expansion) apply.
  No NFC/NFKC, locale or full case folding is added. `é`/decomposed `é`, `Ａ`/`A`,
  `ß`/`ss` and final/ordinary sigma remain distinct keys.
- `Collection(String)` delegates to `cm_domain::organization_name`, preserving
  case while collapsing Unicode whitespace. Names remain unresolved. Same-name
  collections can exist; future resolution must define ambiguity handling instead
  of guessing an ID or silently expanding to descendants.
- `YearRange { from: u16, to: u16 }` requires exactly `YYYY..YYYY`, each in
  0001–9999, with inclusive endpoints and `from <= to`. Single/open/signed years,
  Unicode digits, whitespace within/around the decoded range and reverse ranges
  fail. This is a deliberate narrow DSL grammar, not a restriction on stored years.
- `HasPdf` accepts only ASCII-case-insensitive `pdf`, without whitespace padding.
  It is a requested predicate, not evidence of a file or PDF hit. Future execution
  must preserve #12's attachment-metadata semantics before claiming results.

## Resource and position contract

| Budget | Exact behavior |
|---|---|
| Input | 8192 UTF-8 bytes; greater input fails before scanning/copying content |
| Clauses | 64 total, including invalid clauses and duplicates |
| Value | 1024 UTF-8 bytes, both raw encoded value **including quotes/escapes** and decoded value; no silent truncation |
| Field key | Only the five keys above (longest `collection`, 10 ASCII bytes); unknown keys are borrowed, never copied |
| Shared tag/collection names | Existing 256 Unicode scalar display-name limit and TagKey's 256-scalar post-lowercase limit also apply |
| Diagnostics | At most 16, at most one per examined clause |
| Correction candidates | At most 3 per diagnostic; static full-clause replacement examples, no input echo or automatic repair |

`parse(&str) -> Result<Query, ParseErrors>` guarantees these limits on its outputs.
Public AST structs are in-process data; manually constructing them is not proof
of parser validation. There is no premature transport schema or serialization.

Spans are half-open **byte** offsets into the original input before normalization
or unescaping, with `0 <= start <= end <= input.len()` and both endpoints on UTF-8
character boundaries. `Diagnostic.span.start` is the error position; there is no
ambiguous scalar/UTF-16/line-column unit. Clause spans include field keys/quotes.
Unknown-field errors underline the key, semantic errors underline the raw value,
invalid quote/control/escape errors underline the offending character/sequence.
Unclosed quotes point at EOF with a zero-width span. A trailing backslash has the
more specific invalid-escape error. Empty input points at EOF; empty field values
can have zero-width spans. Over-byte-budget input points at the last boundary at
or before byte 8192; over-term-budget input points at the next clause start.

Parsing is iterative, with no recursive structures or input-sized reserve before
the byte check. It scans only bounded input; decoding and shared normalizers see
bounded values; AST/diagnostic/correction allocation follows the fixed budgets.
No arbitrary user text is incorporated into diagnostic messages or candidates.

Recovery splits at the next whitespace outside double quotes (backslash escapes
one character while inside quotes for boundary detection). Malformed embedded
quotes may therefore consume multiple apparent words; an unclosed quote consumes
the remainder. Keys are checked before decoding values; a known field or term
then checks raw-value size, decoding and semantic validity in that order. The
first error for that clause is reported. Parsing stops at the diagnostic budget.
`ParseErrors.truncated` means an input/term/diagnostic budget left input unexamined;
exactly 16 errors at end-of-input need not set it. Any error rejects the complete
query: **no partially valid AST is returned**. An open quote alone is an error,
but is not marked truncated merely because it reached EOF.

Each correction has its own original-input span and a static, syntactically valid
full-clause example. For unknown fields the examples are `tag:anode`,
`year:2020..2026`, `has:pdf`; they are not fuzzy matching or guessed intent. Other
syntax/semantic errors have at most one example. Size/count/control errors have
no candidate because dropping or rewriting arbitrary input would be misleading.
For a known field's empty/malformed quoted/escaped value, the example keeps the
predicate kind (`tag:anode`, `DOI:10.1234/example`, `year:2020..2026`,
`collection:reading`, `has:pdf`) instead of replacing it with an ordinary term.
Applying one example does not guarantee other errors or whole-query budgets are
fixed; callers must reparse the entire edited input.

## Execution and rollout boundaries

All AST strings, including apparently valid phrases and names, remain untrusted
data. Never concatenate raw input **or AST text** into SQL/MATCH expressions.
Future #18 execution must compile typed clauses with its own validation, parameter
binding and safe FTS literal encoding, resolve names under library scope, and
respect visibility/limits. This parser supplies no SQL/FTS string, repository port,
results, score, continuation, indexing, extraction job, grant or authorization.
No conversion to M1 `LibraryQuery` is supplied: that contract's substring semantics
and limits must not be silently changed to phrase/FTS semantics.

GUI (#15/#21), parent #18's FTS/ranking/PDF/annotation/jobs, #25 IPC envelope and
JobStatus, #28/#29 transports and #30 performance remain separate work. #34 remains
HELD and #23/#25 gates remain closed. PR #45 is not a dependency or source of code.
Headless parser tests neither prove GUI interaction nor FTS/daemon integration.
See [#46 validation](../validation/ISSUE_46.md) for actual commands and evidence.
