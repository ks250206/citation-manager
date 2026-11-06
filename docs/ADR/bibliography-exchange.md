# #13: BibTeX/RIS exchange on shared import and library query

Status: implemented worker-side adapters and export service, based on `18a1eb5` (#33).
No schema, normalization, deduplication, citation allocation, commit, GUI or IPC replacement.

## Calling the adapters

`cm_metadata::{bibtex,ris}::parse(&[u8])` returns a neutral `exchange::ParseReport`:
ordered #33 `PaperDraft`s with original physical start lines and the SHA-256 of **all input
bytes**, file diagnostics (including rejected blocks), accounted draft bytes and a migration
notice. It does not open files, run commands, call providers or write a database. Parser
warnings stay on drafts for the real retained preview/report. Syntax errors that prevent
recovering a draft stay in file diagnostics; callers must display both levels, including
when there are no candidates. An `InvalidInput` resource/encoding error returns no partial
report. Invalid-but-recoverable drafts have error diagnostics and can only be skipped by #33.

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
  exact types on export when RIS or a safe BibTeX type cannot express them.
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
  their raw BibTeX fields are retained using `raw:<field>` entries. Author command spelling is
  retained in the contributor. Full TeX accent conversion/rendering is unsupported; Unicode
  accents are recommended. Protection groups are decoded, escaped literal braces survive.
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
| Record | 64 KiB input/draft/adapter output; at most 256 native fields, #33's contributor/ID/diagnostic limits |
| Nesting | 32 braced/JSON levels; no disabled serde recursion guard |
| Identifiers | BibTeX field/type 128 bytes, key 256 bytes; final normalization/validation in #33 |
| Expansion | **Zero** macro, concatenation, crossref/xdata inheritance or executable TeX processing |
| Export | 10,000 selected records, 20,000 scanned IDs, 32 MiB final text; core port caps each returned record at 256 KiB; these adapters cap at 64 KiB for reparsing |
| Export diagnostics | at most 32 per record, field 128/message 1024 bytes |

Serialization preflights bounded metadata/depth before creating temporary strings; escaping
and hex expansion have fixed factors. Output that cannot fit a round-trip record fails
explicitly, not by truncating text. Core rejects oversized legacy metadata before encoding;
#12 itself still materializes up to 500 complete Papers per page. These output limits are
not a process-RSS guarantee for existing huge library values or a large-library speed claim.

BibTeX accepts braced/quoted/numeric values and both entry delimiters. Macro references,
`#` concatenation and directives are not expanded; they have explicit diagnostics. `crossref`
and `xdata` are preserved without inheritance. Recovery after broken syntax is only at a
physical-line entry start, not an arbitrary `@` in an email/URL. An unescaped line starting
with `@` inside a multiline field is ambiguous and rejected conservatively; the serializer
escapes literal `@` so its own multiline records reparse. RIS starts at TY, ends at ER,
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
test dependencies were activated. #14's DOI/providers/metadata_resolve, #34 packaging/browser
runner, #23/#25, PDF runtime, native GUI, VM and signing are untouched.
