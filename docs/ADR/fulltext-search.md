# #18: page extraction jobs, FTS5 indexes and short-term fallback

Status: adopted for M2-2 on Linux (headless). GUI search (#21), REST (#27), CLI (#28),
MCP tools (#29) and the 10k papers / 20k pages load (#30) build on this contract.
Tokenizer decisions follow the measurements of #53 ([ISSUE_53](../validation/ISSUE_53.md));
nothing here assumes behaviour that was not measured on the pinned bundled SQLite 3.54.1.

## Storage (migration `20261124T120000Z_issue-18_001_fulltext-search.sql`)

- `fulltext_pages` (#8 table) holds page text: `extraction_status='complete'` for pages
  with text, `'unavailable'` for pages without a text layer.
- `fulltext_extractions`: one row per PDF attachment with state
  `extracting|complete|partial|no_text|encrypted|malformed|too_large|failed`, `page_count`,
  `next_page` (resume point), `pages_with_text`, `truncated_pages`, `attempts`
  (consecutive transient failures without progress), a fixed `detail` text (never document
  text or passwords). No row = `pending`.
- `search_documents`: the **single index projection**, one row per paper (title,
  contributor display names, abstract, journal, tag names, identifiers = DOI/URL/citation
  key), per annotation (quote + comment; highlights and notes) and per extracted page with
  text. It is written **only by SQLite triggers** on `papers`, `paper_people`, `people`,
  `paper_tags`, `tags`, `annotations`, `fulltext_pages` and `attachments`, inside the same
  transaction as the source write. #19 (annotations) and every other writer update source
  tables only; there is no separate event/job that updates the index.
- `search_fts_words` (`unicode61 remove_diacritics 1`, all 8 fields) and
  `search_fts_trigram` (`trigram`, all fields except identifiers) are external-content FTS5
  tables over the projection, maintained by projection triggers.
- Soft delete/restore only change `papers.deleted_at`; the projection keeps the rows and
  visibility is applied at query time, so trash/restore are immediately reflected.
  A hard delete removes every document of the paper (`papers`/`attachments` delete
  triggers, plus the child-table triggers).
- `search_index_state` (single row): `suspended` (only inside a rebuild transaction),
  `generation`, `rebuilt_at`.

## Rebuild and damage

`SqliteSearch::verify` runs FTS5 `integrity-check` (rank 1: index vs content) on both
tables and compares the projection with the projection views (both directions plus count,
in one statement and therefore one read snapshot, so a concurrent commit cannot look like
drift). Only SQLITE_CORRUPT* (or an FTS5 "malformed" report) counts as damage; the
integrity-check is an INSERT and needs the write lock, so BUSY/LOCKED (or an interrupt) is
returned as an error without a verdict, never as `fts_ok = false`.
`SqliteSearch::rebuild` (job `search.rebuild`) sets `suspended`, clears and refills the
projection from the views, runs FTS5 `rebuild` on both tables and bumps `generation`, all in
one IMMEDIATE transaction. Source tables are only read. Tests show the trigger-maintained
projection equals a rebuild after every kind of source change, and that a corrupted FTS
shadow table is detected and rebuilt with unchanged source rows and identical results.
Storage classifies every SQLite failure (`cm_storage::SqlFault`: `Busy` for
BUSY/LOCKED, `Interrupted`, `Corrupt`, `Other`; `SqliteSearch::fault`). papersd verifies the
index only when a search failed with `Corrupt`; confirmed damage schedules one ownerless
`search.rebuild` job (`auto-rebuild:<generation>`) and the search returns
`InvalidInput("search index is damaged; rebuild job … scheduled")`. Lock contention, during
the search or the verification, returns `WireError::Busy` (retry later) and never schedules
a rebuild.

The rebuild is one transaction: searches see the old or the new index, never a half-built
one. It is not split into batches (a batched rebuild would expose a partial index or need a
second index generation); on a 20k-page library it is a single long write, which is
acceptable for a damage-recovery path (#30 measures it).

**Cursor stability.** A rebuild renumbers `search_documents.id`. `SearchCursor` positions
(`scan_after` window bound and `offset` within a window) refer to those ids, so a cursor
taken before a rebuild can skip or repeat hits afterwards; clients restart from no cursor
after a rebuild (the `search.rebuild` job result reports the new `generation`). Ordinary
writes between pages can likewise shift ranked results (no cross-call snapshot).

## Extraction job `fulltext.extract`

Payload `{"scope": {"kind": "pending"} | {"kind": "attachment", "id": …} |
{"kind": "paper", "id": …}, "force": bool}`; requires `JobsControl` + `LibraryWrite`.
`cm_core::search::extraction::run` drives a `PageTextExtractor` (PDFium:
`cm_pdf::worker::Operation::Text`, 64 pages per call — the worker maximum, since every
call loads and hashes the whole file — text only) and a
`FulltextStore` (storage):

- Bounds: 100,000 Unicode scalars per page (longer → truncated, state `partial`), 5,000
  pages per document (rest skipped, `partial`), 500 attachments and 20 minutes per run
  (the run commits what it has and succeeds with `more_pending`; papersd resubmits), PDF
  worker limits (32 MiB input, 60 s per batch). `max_run` of the kind is 30 minutes, so
  the #25 per-attempt limit is only reached if a single batch hangs past its budget.
- Durability: pages are committed with `next_page` in batches of 64 pages or 4 MiB of
  text, or at document end. A crash, lost lease or cancel resumes at `next_page`; the
  retry is idempotent (page upserts) and reports `AlreadyCommitted` when nothing is left.
- Cancel: checked before every batch (JobStatus cancel → `Cancelled`; committed pages
  stay searchable, state stays `extracting`). Explicit `Retry` resumes.
- Errors: encrypted/password → `encrypted`, damaged → `malformed`, over size → `too_large`,
  native error → `failed`, stored bytes ≠ recorded SHA-256 → `failed`, a stored path that
  is not a safe managed path → `failed` (only that attachment), no text on any page →
  `no_text` (image-only; OCR out of scope). The job continues with the next attachment.
  A batch time budget or read error is **transient**: pages extracted so far are
  committed, the attachment stays `extracting` with `attempts + 1` (a committed batch
  resets it), the run continues with the next attachment and then ends as a retryable job
  error (runner backoff). After 3 consecutive transient failures the attachment becomes
  `failed`. Worker busy / queue full / runtime missing → retryable job error.
- Automatic submission: papersd submits one ownerless `{scope: pending}` job at start,
  after `attachment.import` and #22 `capture.pdf_fetch` jobs, after PDF attach/commit commands
  and whenever an
  extraction job ends (except when it was cancelled), when unfinished attachments exist
  and no extraction job is active. PDFs attached while a run is active (the attach hook is
  a no-op then) are therefore extracted when that run ends. The idempotency key
  `auto:<first pending attachment>:<count>:<committed pages>:<transient attempts>` changes
  whenever the set changed or work was done or attempted, and prevents loops for a set
  that makes no progress; explicit retry stays available. Ownerless jobs run with daemon authority (#25 `owner_denial`) and are
  visible to GrantAdmin sessions only.
- The extractor is registered only when the daemon has a PDF runtime
  (`pdf-metadata` feature and the bundled pinned library next to `papersd`). Otherwise no
  extraction kind exists and searches report PDFs as `PendingExtraction`.
- PDFium runs in one process-wide worker (`cm_daemon::fulltext::pdf_runtime::shared`),
  shared with PDF metadata reads (pdfium-render allows one binding per process).

### Interaction with #33 import previews

Any committed write to `library.sqlite` invalidates open import previews (#33 uses a
conservative `data_version`). Extraction writes are therefore batched (normally one commit
per document) and job bookkeeping stays sparse (#25). A preview opened while extraction
commits must be re-created; no data is lost. Narrowing the revision to paper tables is not
done here (that would change #33's files).

## Search semantics (`cm_core::search::search`)

Input is the #46 DSL. Typed clauses are compiled: `doi:` exact normalized DOI, `tag:`
TagKey → tag ID, `collection:` exact name → ID (several matches → `AmbiguousCollection`
diagnostic; filter by `SearchRequest::collection` ID instead), `year:` inclusive range,
`has:pdf` the #12 attachment-metadata predicate. Unknown tag/collection names return no
hits with `coverage.unresolved`, never a silent broadening. Additional request scope:
`visibility`, `collection` ID, `papers` (≤500), `sources` (bibliographic/annotations/pages).
A blank DSL is accepted only together with a `collection` ID or `papers` scope and then
lists that scope's papers (a filter-only query); without a scope it stays an `EmptyInput`
diagnostic, so a request never silently selects the whole library.

- Paper-level AND: every term/phrase must match some document of the same paper; hits are
  that paper's matching documents (bibliographic record, annotation, page).
- Terms with ≥3 Unicode scalars: unicode61 phrase (tokens, diacritics folded) ∪ trigram
  phrase (substring, case-insensitive), merged per document (no duplicate hits); the
  clause score is the larger weighted bm25. FTS input is always one quoted phrase, so
  `OR`/`*`/`NEAR` inside quotes are data.
- Terms with 1–2 scalars: unicode61 token match plus a **bounded literal scan**
  (`instr(lower(…))`, ASCII case-insensitive, other text exact) over at most
  `candidate_limit` (1..20000, default 2000) documents in id order, checked against
  `timeout_ms` between 256-row chunks and inside SQLite (an interrupted chunk ends the
  window early). Scope `bounded` (default) scans bibliographic and
  annotation documents always and PDF pages only after an explicit narrowing filter
  (doi/tag/collection/year/`collection`/`papers`); otherwise `unsearched` contains
  `pdf_pages_for_short_terms` — missing page hits are unknown, not zero. Scope `all_pages`
  scans pages too. When the window ends early, `scan = partial`, `unsearched` contains
  `documents_after_window` and `continuation` is the cursor for the next window; hits of a
  window are limited to documents inside it, so windows never repeat a hit.
- Ranking: score = Σ over clauses of the best weighted score; field weights default to
  title 10, authors 8, tags 5, identifiers 5, journal 3, abstract 3, notes 2, body 1 and are
  overridable per request (0..100). Order: score desc, document id asc.
- Results: `limit` 1..100 (default 20), `next_page` cursor; per hit `paper_id`,
  `paper_title`, `doi`, `deleted`, `source`, `attachment_id`, `annotation_id`,
  `page_index` (0-based physical), `page_label` (PDF label), `matched_fields`,
  `snippet` (32..1000 scalars, default 160, control characters → spaces, ellipses when cut),
  `highlights` (UTF-8 byte ranges), `snippet_truncated`, `score`. `matched_fields`, the
  snippet position and highlights compare like the unicode61 index: case-insensitive,
  Latin diacritics folded (precomposed and combining) and any whitespace/line-break run as
  one space, mapped back to exact original offsets — a phrase across a PDF line break or
  `muller` → `Müller` gets a centred snippet and highlight.
- Time budget: `timeout_ms` (1..10000, default 2000) bounds the whole request and is
  enforced inside SQLite (progress handler → interrupt on the search read connection).
  An interrupted FTS lookup returns an empty page with `coverage.timed_out` and
  `complete = false`; the final fetch of the returned hits gets 500 ms extra.
- Concurrency: papersd serves searches and full-text status/page reads from a pool of up
  to 4 dedicated read connections (WAL readers block neither each other nor the writer),
  each request in one read snapshot. A request that finds the pool exhausted waits until
  its own deadline and then gets `WireError::Busy`.
- Coverage: `complete` is false when FTS candidates were truncated (5000 per clause and
  tokenizer), the scan timed out or was partial, pages were unsearched for short terms or
  PDFs are still pending extraction. `no_extractable_text` lists image-only/encrypted/
  damaged/failed PDFs in scope (permanent; does not clear `complete`).
- Normalization: none beyond the tokenizers. Width (ＡＢＣ/ABC), NFC/NFD (decomposed dakuten)
  and kana forms stay distinct, as measured in #53; product-level folding is future work.

## IPC and authorization

- `Command::Search(Box<SearchRequest>)` → `Reply::Search(SearchReply::{Results, Invalid})`,
  `Operation::FulltextSearch` (LibraryRead).
- `Command::Fulltext(FulltextRequest::Status(PaperId) | AttachmentStatus(AttachmentId))`
  → `Reply::Fulltext(FulltextReply::Status(Vec<AttachmentExtraction>))`,
  `Operation::FulltextStatus` (LibraryRead).
- `Command::Fulltext(FulltextRequest::Pages(PageTextRequest))` (≤50 pages, ≤400,000
  scalars) → `FulltextReply::Pages(PageTextRead)`, `Operation::FulltextRead` (LibraryRead).
- Jobs `fulltext.extract` and `search.rebuild` use the #25 `JobStatus` (progress =
  attachments done/total, message = current page) and need `LibraryWrite`.
- The native-host preset cannot search; the MCP read-only preset can search/read status and
  pages but cannot submit jobs. DTOs live in `cm_core::search::{types, extraction}` and are
  re-exported as `cm_protocol::search`.

Evidence: [ISSUE_18](../validation/ISSUE_18.md).
