# #33: Shared import application service and PDF policy

Status: implemented for worker-side bibliographic import, including real SQLite commits.
BibTeX/RIS/HTTP DOI adapters (#13/#14), GUI/PDF parsers (#15/#28), IPC/auth (#25),
packaging (#34) and filesystem ingestion orchestration remain separate.

## Composition and boundaries

```rust,ignore
use cm_core::{import::ImportCoordinator, ports::ImportService};
let mut service = ImportCoordinator::new(db.imports());
let preview = service.preview(request)?; // no persistent writes
// Review candidates, diagnostics, exact/fuzzy matches and per-field differences.
let result = service.commit(decisions_bound_to_preview)?;
```

`PaperDraft::titled` preserves the lightweight title entrypoint while the additive draft
now includes complete `Bibliography`, ordered contributors/external IDs, `ImportSource`
(adapter/locator/row/input SHA-256) and structured row diagnostics. Source/diagnostics
are retained in previews and row reports; they are not a new persisted provenance table.
Invalid metadata rows are reviewable but can only be skipped; edit candidates and preview
again. Resource-limit violations reject the request. Successful rows do not hide errors.

All pure policies live under `cm_core::import`; `cm_metadata` reexports those exact modules
(`normalize`, `dedup`, `citation_keys`, `file_metadata`) for future adapters. No competing
normalizer and no core→metadata/storage or metadata→storage edge is introduced. Storage
implements the small `ImportBackend` port, with the same shared normalization and plan
validator used by the real service. The existing `ImportService` preview/commit port and
compile-fail raw-request check remain, with additive cancel support. Existing mocks were
intentionally extended for the new result fields, not replaced by production claims.

## Normalization and duplicate policy

- Title and contributor name whitespace uses Rust Unicode `split_whitespace`; contributor
  order, author/editor roles, literal organizations, ORCID, accents/combining marks and
  all unmodified bibliography/extra JSON fields survive. Roles become lowercase.
  No name splitting, transliteration, NFC/NFKC or locale-specific name inference.
- `papers::normalize_doi` is still the ONLY DOI canonicalization point, also called by
  manual CRUD. Resolver wrappers decode the suffix once as UTF-8, drop URL query/fragment,
  and reject malformed escapes/decoded whitespace/control characters. The registrant's
  slash must be literal. Raw DOI / `doi:` suffix percent escapes stay literal. Thus
  raw `10.12/a%2Fb` differs from resolver `https://doi.org/10.12/a%2Fb` (`10.12/a/b`).
  This resolves the #8 deferral; double encoding decodes only once.
- DOI bibliography/external-ID disagreement is a row error. Known DOI/arXiv/PMID namespaces
  canonicalize; PMID leading zeros are removed, arXiv prefixes (`arxiv:` and HTTP/HTTPS/schemeless arxiv.org/abs/) compare ASCII
  case-insensitively and normalize while
  version suffixes remain part of the retained identifier. Unknown namespace/value spelling
  is preserved apart from edge whitespace. Canonical duplicate IDs within a row are errors.
- Dates accept `YYYY`, `YYYY-M[M]`, `YYYY-M[M]-D[D]` and `/` separators. Calendar validation
  includes leap years; output has zero-padded precision, with year consistency required.
  Absent dates stay absent. Page ranges whose endpoints are decimal numbers with the same optional ASCII letter
  prefix normalize hyphen/double hyphen/em dash to en dash (e.g.123-145 or S1--S12).
  Mismatched-prefix e-locators such as e2020-001 and S12-3 retain spelling.
- Supplied citation keys are trimmed and retained if available, including when the key
  belongs to the row's single exact persisted match. That is not a self-collision. Otherwise generate from
  first contributor/title/year, preserving Unicode letters/numbers. Collisions get `_2`,
  `_3`, etc., including trash and earlier batch rows. Keys are at most256 UTF-8 bytes;
  generated/collision keys shorten at a scalar boundary to leave suffix space, with a
  diagnostic naming the original and proposed key. No paper field is silently shortened.
- Matching priority: canonical DOI, then canonical arXiv/PMID, then title+first-author+year,
  then similar title. Only identifiers are exact. Bibliographic matches are suggestions
  even when all those fields agree. Similar titles use an explicit heuristic: Unicode
  lowercase/whitespace, first512 scalars, character bigram Jaccard overlap >=80%, >=4 distinct
  bigrams. This bounds comparison work; it neither rewrites nor truncates stored titles.
- Exact matches include trash and the current batch. `Create` cannot bypass an existing
  exact match. A batch cannot create duplicate identifiers or merge two rows into one
  target; skip redundant rows and review again if needed. Fuzzy candidates may explicitly
  create a distinct paper. Existing multiple exact matches are all exposed, not auto-selected.

## Review, revision and transaction semantics

Each candidate exposes its normalized draft, source diagnostics, all bounded persisted
matches (including deleted status), exact batch row indices, and before/after differences.
`existing_match` remains a convenience for the first exact persisted match only. It is
not approval. `Merge { match_index, fields }` selects a reviewed target and a unique subset
of its displayed field differences. Empty fields means retain metadata and add approved
membership. The client cannot submit a replacement paper or new destination at commit.
External-ID diffs are an additive union; unselected fields remain verbatim. DOI disagreement
with a target is rejected even if the caller tries to ignore that field. Trash must be
restored explicitly outside import. No implicit restore, overwrite, merge or attachment move.

Each preview gets a fresh UUIDv7 handle rather than a process-restarting counter.
An old process's handle cannot alias the first retained preview after restart; a real
two-process regression covers this boundary. UUIDs are not caller grants.
A preview lives ONLY inside that coordinator; it is bound to its backend/connection and
retained destination IDs. Handles carry no authorization and are not durable credentials.
#25 must resolve caller grants and restrict preview/commit/cancel access before invoking
this service, including native-host approval. Sharing a handle does not establish a grant.
Returned preview objects are copies: editing them cannot change the service's retained state.

The SQLite adapter holds one borrowed worker connection for its lifetime. Its revision
combines an adapter-instance ID, same-connection `PRAGMA data_version`, and `total_changes`.
This deliberately invalidates **any intervening database write**, rather than only changes
to a matched paper. It catches concurrent edits including A→B→A, ID/key/link/attachment
changes, and local writes/rolled-back attempts without depending on millisecond timestamps.
It is a conservative connection-bound preview revision, not a persisted per-paper version.
It is never compared across connections or used after reopen. Unrelated committed writes
may require a fresh preview; this is an explicit concurrency tradeoff.

Commit starts `BEGIN IMMEDIATE`, validates the retained destination through #12 helpers,
loads a fresh consistent inventory/revision (including trash), and calls the shared
`PreparedImport::writes`. It revalidates normalization, the complete decision set, reviewed
merge targets, final external-ID/DOI/citation-key conflicts across the whole approved batch,
and stale revisions while holding the writer lock. DOI/citation SQLite UNIQUE constraints
remain the last defense. Paper metadata/contributor/external-ID writes and **all** additive
collection/tag memberships run in that SAME transaction. It reuses transaction-non-owning
paper helpers and #12 destination helpers; it never chains public create/apply transactions.
Tag names continue to use `TagKey::new(raw)` in the existing domain/tag API; commit takes IDs,
not alternate tag normalizers or auto-created tags. Duplicate tag IDs retain set semantics.

Success consumes the preview; repeated commit returns NotFound (no second write). Cancel
removes it with no writes; repeated cancel returns NotFound. Missing, expired, foreign-service
and restarted-service handles are NotFound. Decisions must cover every row exactly once;
out-of-range, duplicate/missing indices, invalid rows, unknown match/field and repeated merge
targets are rejected before writes. Validation errors permit corrected decisions. A rolled-back
write may advance `total_changes`; cancel then obtain a fresh preview before retry. Conflict
always requires refresh/review. No automatic retry or silent key reassignment after review.
A lost response after success has no durable replay ledger: inspect stored results before
retrying with a new preview; durable transport request deduplication is #25.

## Input, inventory and retained-state bounds

- 1..=100 input rows; each serialized draft <=64 KiB, title <=16 KiB UTF-8, <=256 contributors,
  <=128 external IDs and <=128 adapter diagnostics. Source adapter <=256 bytes, locator4096,
  diagnostic field128/message1024, external namespace128/value4096, citation key256.
- Metadata malformed within those bounds gives per-row diagnostics. Oversized requests fail
  before inventory access. No legacy stored field is truncated or rewritten to fit import.
- Inventory includes <=20,000 papers and <=100,000 contributor/external-ID relationships,
  <=32 MiB raw selected text before loading, and <=32 MiB serialized full papers afterward.
  Exceeding the budget fails explicitly; partitioning the input cannot bypass that inventory
  limit. This M1 implementation scans the library; it makes no large-library speed claim.
- At most20 persisted candidates per row and a2 MiB accounted preview payload budget
  (drafts, retained existing papers, differences and diagnostics). Exceeding either fails;
  matches are not silently dropped. ID/index/container overhead is additional bounded memory.
- Decision payloads are checked before retention: <=100 decisions, <=32 fields per merge,
  <=128 bytes per field name; invalid decisions are discarded.
- Maximum4 retained previews per service, each expiring after at most300 seconds measured
  monotonically. Both limits may be reduced through `ImportPolicy`. Capacity errors never
  evict an unexpired approved preview. Pruning occurs on service calls; dropping the service
  releases all retained state. Idle services retain at most their four bounded previews.

16 KiB titles also stay well within #12's1 MiB escaped cursor limit for new imported records.
Legacy/manual CRUD retains compatibility and its existing title sizes; the existing #12
oversized-title error plus ID-sort workaround remains valid and tested. This issue does not
impose new global/manual CRUD restrictions or claim to repair older enormous titles.

## PDF and folder contracts (no real parser or filesystem traversal)

`PdfIntent::AttachExisting(id)` returns an attachment-only proposal; embedded metadata never
updates that paper. Later #15 integration validates active-paper existence and invokes #9's
immutable attachment API. `RegisterNew` fills missing fields from a valid parser/DOI candidate,
then applies explicit bibliography/title/identifier namespace overrides. Invalid embedded
metadata falls back; invalid explicit fields remain errors. If title remains absent, a PDF
basename becomes a provisional title. Missing contributor/date fields and embedded/provisional
metadata stay explicitly unresolved/reviewable. Editing yields a NEW preview. Neither path
claims the file is valid PDF: parsing, valid XMP extraction and DOI lookup are #15/#28/#14.

`FolderScope` distinguishes explicit selected files from a selected directory with an explicit
recursion flag. The future trusted inventory adapter must verify containment, all ancestors,
regular files, no symlinks, and no hard-link/path-policy bypass. The planner accepts ONLY its
supplied inventory; it never walks a directory. Outside-scope and non-PDF-extension entries
are excluded; unsafe/symlink/nonregular entries and files over the policy's256 MiB default
are reported individually. Duplicate known hashes within this selected batch are skipped;
unknown hashes stay candidates until the real adapter hashes them. The #9 store independently
validates bytes and may share a verified blob across explicitly selected papers.

`FolderReport` retains per-file index/disposition/retryability and planning progress
(`completed` = assessed entries), cancellation and total. Cancellation at a boundary marks
remaining files Cancelled; earlier results survive. Future execution replaces Ready with
Imported(attachment ID) or Failed(diagnostic), isolates per-file failures, publishes progress,
and retries only explicitly selected failed/cancelled files from a new inventory/preview.
The mock test demonstrates these report transitions, not an implemented filesystem job runner.
Partial success is per file; bibliographic batches themselves remain all-or-nothing.

#9 remains the real ingestion boundary: immutable original publication precedes DB attachment
association and may leave orphans after failures. This issue performs no file writes during
bibliographic preview/commit and makes NO atomic file+database/parser guarantee. Its synthetic
attachment regression uses the real #9 ingestion API only to prove an approved metadata merge
preserves existing original bytes and relationships. No actual user library/profile is opened.

Official API evidence: [SQLite data_version](https://www.sqlite.org/pragma.html#pragma_data_version),
[rusqlite0.41.1 Transaction](https://docs.rs/rusqlite/0.41.1/rusqlite/struct.Transaction.html).
Context7 version-specific docs were checked and local pinned `src/transaction.rs` and `src/lib.rs`
confirmed IMMEDIATE, rollback-on-drop and `total_changes` signatures. Runtime evidence is in
[ISSUE_33](../validation/ISSUE_33.md).
