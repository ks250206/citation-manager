# #13: BibTeX/RIS exchange on shared import and library query

Status: implemented worker-side adapters and export service, originally based on `18a1eb5`
(#33), integrated with main `427f130` (#14) by a normal merge.
No schema, normalization, deduplication, citation allocation, commit, GUI or IPC replacement.

## Calling the adapters

`cm_metadata::{bibtex,ris}::parse(&[u8])` returns a neutral `exchange::ParseReport`:
ordered #33 `PaperDraft`s with original physical start lines and the SHA-256 of **all input
bytes**, file diagnostics (including rejected blocks), accounted draft bytes and a migration
notice. It does not open files, run commands, call providers or write a database. Parser
warnings stay on drafts for the real retained preview/report. Syntax errors that prevent
recovering a draft stay in file diagnostics; callers must display both levels, including
when there are no candidates. File-wide encoding/input/block/total-draft failures return
`InvalidInput` with no partial report. Record-local limits instead produce a line-specific file
diagnostic and skip that entire record. Invalid-but-recoverable drafts have error diagnostics
and can only be skipped by #33.

```rust,ignore
let parsed = cm_metadata::bibtex::parse(&authorized_file_bytes)?;
// Display parsed.diagnostics and parsed.migration_notice, even for an empty report.
let mut service = cm_core::import::ImportCoordinator::new(db.imports());
let preview = service.preview(parsed.batch(0, 100, destination)? )?;
// Review the actual normalized candidates, matches, key changes and differences.
let result = service.commit(explicit_decisions_bound_to_preview)?;
// Only after that result, select the next range and obtain a NEW preview.
```

`batch(start,count,destination)` accepts an explicitly chosen 1..=100 range; it does not
preview/approve/commit anything or raise #33's real limits. A 1000-record file needs ten
separate reviewed previews/atomic commits. Previously committed batches remain if a later
batch is cancelled, invalid, stale or fails. There is **no file-wide atomic transaction**,
auto retry, resumable job/IPC credential or automatic approval. #33's 2 MiB retained-preview,
20,000-paper inventory, matching, revision, decision and expiry limits still apply. A batch
may need to be smaller than 100 due to byte/match limits. The DTOs are useful application
data for a future preview/progress/report client; they do not implement such a client.

`cm_core::export::{ExportService, BibliographySerializer}` reads actual #12 complete Papers
using ID-sorted keyset pages. `ExportSelection::{All,Ids,Collection}` selects active records;
collection means direct membership, and unique explicit IDs are emitted in library ID order.
Missing/trashed explicit IDs fail `NotFound`, duplicate/nil IDs fail `InvalidInput`; an empty
ID selection returns an empty report. Missing collection filters match nothing, as in #12.
No paper summaries or independent SQL/filter semantics are used. As with #12, pages observe
latest committed state, **not one snapshot across the whole file**. Export detects repeated
IDs/non-progressing cursors and caps scans, but concurrent deletion/insertion can still
change the set. Export from a quiescent library for a reproducible migration.

## Fields and fidelity

- BibTeX major entry types stay lowercased as supplied. RIS JOUR/BOOK/CHAP/CONF/CPAPER/THES/
  RPRT/UNPB/ELEC/GEN map to article/book/incollection/inproceedings/phdthesis/techreport/
  unpublished/misc. Unknown types are retained with warnings; private extension preserves
  exact types on export when RIS or a safe BibTeX type cannot express them. Every lossy RIS
  type mapping also names the original type, emitted TY and public reimport type in a specific
  export diagnostic (e.g. mastersthesis → THES → phdthesis, inbook → CHAP → incollection,
  manual → GEN → misc).
- Ordered authors/editors, comma and simple given/family names, `and` outside groups,
  braced literal/corporate names, Unicode, escaped punctuation, title, journal/container,
  year/date, volume/issue/pages, DOI, URL, abstract, language and citation keys are mapped.
  BibTeX three-part suffix names stay literal with a warning, rather than guessing identity.
  RIS has no universal literal/person marker: braced names use the adapter convention;
  ambiguous unmarked names use the documented simple name split. There is no locale inference.
- Keywords and accessed date live in `extra_json.keywords` / `.accessed`: BibTeX preserves
  the original keyword string; RIS preserves ordered repeated KW values as an array. Accessed
  spelling stays verbatim. #33 owns bibliographic date/page/DOI normalization, not these adapters.
- Unsupported BibTeX field/value pairs and RIS tag/value pairs live in ordered arrays under
  `extra_json.bibtex` / `.ris`. Safe non-conflicting names/tags are also emitted as native
  fields. Repeated unknown RIS tags survive. Unknown TeX commands stay literal with warnings;
  their raw BibTeX fields (including author/editor spelling) are retained using `raw:<field>`
  entries. A finite, nonexecuting character conversion handles diaeresis, acute, grave,
  circumflex, tilde, cedilla, caron, breve, double acute, ogonek, macron and dot accents, plus
  ss/o/aa/l and common ligatures, in protected/braced/unbraced forms. Common Latin pairs use
  precomposed Unicode; other ASCII letters use the fixed combining mark. This is not global
  Unicode normalization or a TeX renderer; unknown commands are never expanded.
- Only one outer group enclosing the *whole* name marks a corporation; `{Smith}, {John}`
  remains a person. Text protection groups such as `{DNA}` are decoded to plain text. Their
  raw values stay in `protection:<field>` entries and **each affected field has an explicit
  case-protection-loss diagnostic on import and either export**. Public output does not replay
  arbitrary TeX syntax. Another application's case-changing style may therefore lowercase
  DNA; cm-v1 retains the raw evidence, not a promise of external style fidelity. Escaped
  literal braces/backslashes remain literal characters.
- Bare identifiers and `#` expressions are syntactically read without evaluation and stored
  exactly under `extra_json.bibtex` as `expression:<field>` pairs. Standard month identifiers
  jan…dec are warning-only; all other unresolved expressions make the retained draft
  review-only, preserving its usable title/authors/DOI. Definitions are never looked up.
  Only the fixed twelve standard month tokens are replayed publicly as bare BibTeX month
  values, without evaluation. RIS emits a month-specific loss diagnostic; cm-v1 retains
  the syntax. Other expressions are not replayed as public native fields; exact raw syntax
  requires extra_json/cm-v1 and explicit review. Both year and date stay present on
  disagreement with a row error.
- Native DOI/URL are verbatim, on both reading and writing: `_`, `%`, `&`, `#` and balanced
  braces are not TeX-escaped or decoded. Export explicitly rejects unmatched/deep braces,
  backslashes and CR/LF in these fields. General text uses grouped literal `{@}` (including
  line boundaries), never the LaTeX spacing command `\@`. General-text caret and tilde use
  `\textasciicircum{}` / `\textasciitilde{}`, decoded back to literal characters without TeX
  execution; verbatim DOI/URL remain unchanged.
- Ordinary fields are written publicly. Every export additionally emits a **private,
  versioned `cm-v1` extension** (`cm_data` in BibTeX, `C9  - cm-v1:` in RIS): bounded hex-encoded
  JSON for exact contributors/order/roles/ORCID, external IDs, arbitrary extra_json, publication
  date and type. It contains no internal paper/attachment IDs or source files. Each record's
  export diagnostic explains that other tools may ignore this extension. They can lose
  details such as literal distinction, role/order, extra JSON and precise type; this is not
  claimed to be lossless migration through arbitrary other applications.
- The extension includes a SHA-256 of the actual parsed public representation (including
  native unknown fields, keywords and accessed date). Public-field edits with an unchanged
  extension produce a row error before restoration. It is an integrity check, **not a
  signature or authority**; all restored data still passes #33 preview/commit validation.
  Removing the extension deliberately uses public-format fidelity and diagnostics.
  Malformed/unknown extension versions fail the row; no silent fallback/overwriting.

There is no new DOI compatibility rule. The known #33 **bibliography-only legacy DOI**
ambiguity remains: escaped old values with no retained resolver external-ID witness cannot
be distinguished from intentional percent-literal DOIs. A later decoded resolver import may
still offer Create. Export does not normalize/repair old stored metadata. An exported legacy
bibliography/external-ID pair is also new input on reimport and may fail strict DOI agreement;
review/edit and repreview instead of bypassing #33 checks. See
[import-service](import-service.md#legacy-doi-compatibility-p1-repair) before reviewing duplicates.

## Recovery and resource policy

| Boundary | Limit / behavior |
|---|---|
| Input encoding | UTF-8 only, optional UTF-8 BOM; invalid UTF-8/NUL/UTF-16/32 explicitly rejected; CRLF supported |
| File | 16 MiB input, 10,000 records/blocks and bounded file diagnostics; 32 MiB serialized drafts |
| Record | 64 KiB input/draft/adapter output; at most 256 native fields and 256 authors; #33's existing limits also apply. Input violations skip the entire record, never truncate it |
| Nesting | 32 braced/JSON levels; iterative extension JSON preflight before serde, existing recursion guard retained |
| Identifiers | BibTeX field/type 128 bytes, key 256 bytes; final normalization/validation in #33 |
| Expansion | **Zero** macro, concatenation, crossref/xdata inheritance or executable TeX processing |
| Export | 10,000 selected records, 20,000 scanned IDs, 32 MiB final text; core port caps each returned record at 256 KiB; these adapters cap at 64 KiB for reparsing |
| Export diagnostics | at most 32 per record, field 128/message 1024 bytes |

Serialization preflights bounded metadata/depth before creating temporary strings; escaping
and hex expansion have fixed factors. Output that cannot fit a round-trip record fails
explicitly, not by truncating text. Core rejects oversized legacy metadata before encoding;
#12 itself still materializes up to 500 complete Papers per page. These output limits are
not a process-RSS guarantee for existing huge library values or a large-library speed claim.

BibTeX accepts braced/quoted/numeric values, raw identifiers/concatenations and both entry
delimiters. Macro references, `#` concatenation and directives are never evaluated; they have
explicit diagnostics. Directives are ignored with a file diagnostic, without parsing/executing
their definitions. `crossref`
and `xdata` are preserved without inheritance. Recovery after broken syntax is only at a
physical-line entry start, not an arbitrary `@` in an email/URL. An unescaped line starting
with `@` inside a multiline field is ambiguous and rejected conservatively; the serializer
groups literal `@` so multiline records reparse without changing its TeX meaning. After such
an ambiguous boundary, all subsequently recovered drafts have a source-line row error and
cannot Create/Merge; file syntax must be corrected and reparsed, or those drafts explicitly
skipped. The parser cannot prove whether later apparent entries were embedded in the broken
value. Records recovered after ordinary local limit failures retain the previous behavior.
Record-local
byte/field/author/draft/depth failures skip only the offending record and recover at the next
physical `@` entry or RIS TY. Rejected content is not retained or truncated into a false draft;
all examined blocks still count against the file-wide block budget. RIS starts at TY, ends at ER,
accepts indented continuation lines, and rejects incomplete records before the next TY/EOF.
Malformed non-indented content makes that draft review-only; valid neighboring records survive.
Conflicting mapped singleton fields/aliases do not silently win. RIS standalone carriage
returns cannot be represented exactly and fail export explicitly (CRLF file framing works).

## Migration boundary and ownership

**BibTeX/RIS do not move PDFs, attachments or annotations.** Collection/tag relationships,
internal IDs and persisted source provenance are also not transferred. The notice is present
in parse/export reports. Existing attachment bytes/hashes/associations and additive collection/
tag memberships survive merge, skip, cancel and export. ImportSource hashes/lines are retained
in preview/outcome reports per #33; no provenance persistence table is invented here.

Adapters live in metadata and the neutral export port/service in core. DB composition tests
live in daemon so there is no metadata→storage edge, even through dev dependencies. No
framework was added to domain/core/protocol. Only existing pinned serde/serde_json/SHA-256 and
test dependencies were activated for #13. Main's #14 DOI/providers/metadata_resolve and
HTTP/TLS dependencies/features coexist unchanged with these adapters; the combined workspace
runs the registered DOI integration tests. #34 packaging/browser runner, #23/#25, PDF runtime,
native GUI, VM and signing are untouched.
