# #18: page extraction jobs, FTS5 and Japanese short-term fallback — validation

Environment: Linux x86_64 container (4 vCPU, shared with other builds), Rust 1.99.1,
bundled SQLite 3.54.1 (rusqlite 0.41.1), pinned PDFium chromium/8125 Linux x64 fetched by
`python3 scripts/pdfium/fetch.py --platform linux-x64 target/pdfium-linux` (checksum
verified by the script and again at load). Debug builds; all timings are warm, small-scale
and **not** release or #30 load figures. Contract: [fulltext-search ADR](../ADR/fulltext-search.md).

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p cm-daemon -p cm-pdf --all-targets \
  --features cm-daemon/pdfium-runtime-tests,cm-pdf/runtime --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/check_architecture.py
python3 scripts/check_migrations.py --base origin/main
python3 -m unittest discover -s scripts -p 'test_*.py'
export CM_PDFIUM_LIBRARY="$PWD/target/pdfium-linux/lib/libpdfium.so"
cargo test -p cm-pdf --features runtime --locked --test metadata_runtime --test runtime --test extract_runtime
cargo test -p cm-daemon --features pdfium-runtime-tests --locked --test fulltext_runtime -- --nocapture
cargo test -p cm-storage --lib --locked fts::measure -- --nocapture
```

The CI `linux-native` lane runs the two runtime commands after fetching the pinned library.

## What each test proves

| Test | Evidence |
|---|---|
| `storage fts::tests::triggers_keep_projection_equal_to_a_rebuild_through_every_source_change` | paper create/update (contributor replacement, DOI), tag add/rename/remove/delete, annotation insert/update, page insert/update, soft delete/restore, hard delete: `verify()` (FTS integrity-check + projection == views) passes after each step; a rebuild yields the identical projection |
| `…corrupted_fts_index_is_detected_and_rebuilt_without_touching_sources` | damaged `_data` shadow rows fail `integrity-check`; rebuild restores identical hits, source row counts/titles unchanged, generation +1; a drifted projection is detected and rebuilt |
| `…measured_tokenizer_behaviour_and_short_term_fallback` | unicode61 matches `電池` only as a standalone token; trigram finds `固体電` but nothing for 2 scalars; composed/decomposed `ガラス` stay distinct in trigram; the service finds both `電池` papers through the literal fallback and lists PDF pages as unsearched |
| `…ranking_filters_snippets_pages_and_errors` | title outranks page body by default and reweighting reverses it without duplicate hits; year/tag/collection/has:pdf/sources filters; unresolved tag and ambiguous collection; `UnknownField` diagnostic with span and correction; quoted `OR` is data; ranked pagination |
| `…short_term_scan_is_bounded_with_continuation_and_explicit_scope` | candidate window 4 → ≥3 windows, every hit once, pages unsearched without scope; paper scope searches pages; `AllPages` |
| `…short_term_scan_stops_at_the_deadline_between_chunks` | an expired deadline stops after one 256-row chunk (`timed_out`), the continuation finishes the rest |
| `…extraction_store_commits_batches_resumes_and_records_failures` | batch commit, resume point, `partial` for truncation, `encrypted` failure, forced re-extraction resets, bounded page reads, page hits, `no_extractable_text` coverage |
| `core tests/search_extraction.rs` | 64-page commit batching (64 pages per extractor call), resume from `next_page`, encrypted/hash-mismatch/no-progress failures do not stop the queue, cancel keeps committed batch, busy is retryable, page limit → `partial`, snippet bounds/highlights, request limits |
| `pdf tests/extract_runtime.rs` (real PDFium) | 6-page corpus text incl. `日本語 αβ café`, labels `iv`/`S12`, image-only page empty, page ranges, per-page cut to 5 scalars with `truncated`, scanned PDF, encrypted (error and with password), malformed, admission limits |
| `daemon tests daemon_fulltext` (IPC, deterministic extractor) | automatic extraction after `attachment.import`; encrypted/image-only states while the job succeeds; page hits with label and highlighted snippet; scoped 2-scalar Japanese; page read bounds; DSL error over IPC; soft delete/restore; explicit `search.rebuild` and forced extraction jobs; cancel mid-document, committed pages searchable, `pending_extraction` coverage, retry resumes at the committed page and completes 320 pages; native-host cannot search, MCP read-only can search/status but not submit jobs, no extraction kind without an extractor |
| `daemon tests fulltext_runtime` (real PDFium over IPC) | corpus/scanned/encrypted/malformed imported → extraction job → states `complete/no_text/encrypted/malformed`; `"coordinate proof"` hits pages 0–3 with labels; `日本語` via trigram; `αβ` as a unicode61 token; `日本` unscoped → pages unsearched, scoped → 4 page hits |
| `storage fts::measure` | synthetic JP/EN corpus (below) |
| `storage …busy_writer_is_reported_as_contention_not_damage` (review P2-1) | with another connection holding the write lock, a search on a reader still answers (WAL) and `verify()` returns an error with `fault() == Busy` instead of `fts_ok == false`; after the lock is released it is healthy and the generation is unchanged; BUSY/LOCKED/INTERRUPT/CORRUPT_VTAB/ERROR are classified `Busy/Busy/Interrupted/Corrupt/Other` |
| `storage …deadline_interrupts_inside_sqlite_and_readers_do_not_serialize` (P2-2) | an endless recursive statement on reader A is interrupted by its 1.5 s deadline (`Interrupted`) while a search on reader B completes in < 1 s; an interrupted FTS lookup yields an empty page with `timed_out` and `complete == false`; an interrupted scan chunk ends the window as `timed_out` |
| `daemon fulltext::readers::…concurrent_requests_get_separate_readers_and_waits_are_bounded` (P2-2) | two concurrent leases get distinct connections at once; an exhausted pool returns `WireError::Busy` at the request deadline; a waiter proceeds when a reader is returned; readers are reused |
| `daemon_fulltext::attachment_added_during_a_running_extraction_is_extracted_after_it` (P2-3) | a PDF committed by another writer while an extraction job runs (no hook) is `complete` and searchable after that job ends, without restart; with the old runner rule the same test times out with the attachment `pending` (checked once by reverting the rule locally) |
| `core …transient_failures_are_retried_with_bounded_attempts`, `daemon_fulltext::transient_extraction_failures_are_retried_and_bounded`, `storage …deferred_attachment_stays_unfinished_with_attempt_count` (P2-4) | a time-budget/read failure defers the attachment (other attachments continue, pages before it are committed), the job is retried and completes; three consecutive transient failures → `failed`, with no resubmission loop; progress resets the count and changes the automatic job key |
| `core …snippets_find_phrases_across_line_breaks_and_folded_diacritics`, `storage …phrase_across_pdf_line_break_gets_snippet_and_highlight`, `core search::fold` unit test (P2-5) | `"solid electrolyte"` across `\r\n` and `muller` → `Müller` (precomposed and combining) get `matched_fields = [body]`, a centred snippet and a highlight over the original text; C1 controls keep offsets valid |
| `storage …unsafe_attachment_path_fails_only_that_attachment` (P3) | an attachment row with an unsafe stored path is marked `failed` and skipped; the other attachment is still a target |

## Measurements

Final run on merge head (base `271edae` + #18), all commands above exit 0:
fmt check; workspace clippy; runtime-feature clippy (`cm-daemon/pdfium-runtime-tests`,
`cm-pdf/runtime`); `cargo test --workspace --locked` **284 passed, 0 failed** (57 result
lines incl. doctests); architecture; migrations vs `origin/main` and vs
`origin/claude/issue-25-daemon-ipc` (4 checksums/IDs valid); Python 32 tests OK; PDFium
runtime tests (metadata_runtime, runtime, extract_runtime: 3 passed); `fulltext_runtime` 1
passed. An earlier pre-merge workspace run had one failure in the #25 timing test
`daemon_lifecycle::active_and_retry_waiting_jobs_block_idle_exit_until_done` while the
4-vCPU container was shared with other builds; it passed when rerun alone and in the final
run. It is recorded here rather than hidden.

### Synthetic corpus (`fts::measure`, debug build, in-memory DB, warm, nearest-rank of 31)

120 papers (Japanese/English titles and abstracts), 360 pages, 480 projection documents.
Recall is against a literal oracle (ASCII case-insensitive substring) over the same text
with `all_pages` scope and a window covering the corpus; precision is exact (no extra hits).

| Term | Scalars | Series | Expected | Found (recall) | Default bounded scope | p50 / p95 ms |
|---|---:|---|---:|---:|---:|---:|
| `電` | 1 | short fallback | 240 | 240 (1.0) | 60 (pages unsearched) | 4.93 / 5.58 |
| `電池` | 2 | short fallback | 90 | 90 (1.0) | 30 (pages unsearched) | 4.25 / 4.84 |
| `Li` | 2 | short fallback | 132 | 132 (1.0) | 72 (pages unsearched) | 4.63 / 5.05 |
| `固体電` | 3 | FTS | 120 | 120 (1.0) | 120 | 2.00 / 2.36 |
| `電解質` | 3 | FTS | 90 | 90 (1.0) | 90 | 1.72 / 2.17 |
| `lithium` | 7 | FTS | 90 | 90 (1.0) | 90 | 2.93 / 3.69 |
| `"current density"` | 15 | FTS | 60 | 60 (1.0) | 60 | 3.53 / 4.00 |

- Candidate window 100: 5 windows, disjoint hits, union = oracle for `電池`.
- Index size (DBSTAT allocated bytes): total 348,160 — projection 77,824 (+ indexes
  32,768 / 24,576 / 16,384), trigram `_data` 81,920, unicode61 `_data` 24,576, docsize
  12,288 each, `fulltext_pages` 45,056. Synthetic short texts; not a production estimate.
- Indexing cost: 360 page inserts (trigger → projection → both FTS) 90 ms total.

### Real PDFium over IPC (`fulltext_runtime`, debug)

4 PDFs (corpus 6 pages, scanned, encrypted, malformed) imported and extracted:
266 ms wall from the first import job to all extraction states settled. Warm IPC search
(31 runs): FTS `日本語` p50 1.60 / p95 1.99 ms; scoped short term `日本` p50 1.98 / p95 2.13 ms.

## Independent review fixes (draft PR #60)

| Finding | Change |
|---|---|
| P2-1 spurious rebuild | `cm_storage::SqlFault` classification; `SqliteSearch::fault`; `verify()` returns BUSY/LOCKED/interrupt as an error (no verdict) and only CORRUPT/"malformed" as `fts_ok=false`; projection diff in one statement (one snapshot); papersd verifies only after a `Corrupt` search failure and answers `WireError::Busy` for contention |
| P2-2 one serialized connection, unbounded FTS | `SearchReader` (dedicated connection, progress-handler deadline, one read snapshot per request) and a 4-reader pool in papersd; `timeout_ms` bounds the whole request inside SQLite; interrupted FTS → `timed_out` page, interrupted scan → partial window |
| P2-3 attachments added during a run | the runner calls `ensure_extraction` after every extraction job that was not cancelled; the idempotency key (now with transient attempts) prevents loops |
| P2-4 transient = permanent | `ExtractError::Transient` for PDF worker deadline/IO; per-attachment `attempts` (migration column), deferral, job-level retry, `failed` after 3 consecutive transient failures; queue full stays a job-level retry |
| P2-5 snippets miss phrases/diacritics | `search::fold`: case, Latin diacritics (table generated from Unicode canonical decompositions for U+00C0–U+024F and U+1E00–U+1EFF, plus combining marks) and whitespace runs folded with exact original offsets for `matched_fields`, snippet and highlights |
| P3 | 64 pages per extractor call (was 16; each call loads and hashes the whole PDF); an unsafe stored path fails only that attachment; rebuild batching and cursor stability documented as limits; per-attachment document reuse in the PDF worker not done |

Verification of these fixes (Linux container, debug): `cargo fmt --all -- --check` exit 0;
`cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0;
`cargo clippy -p cm-daemon --all-targets --features desktop-bootstrap --locked -- -D warnings`
exit 0; runtime-feature clippy (`cm-daemon/pdfium-runtime-tests,cm-pdf/runtime`) exit 0;
`cargo test --workspace --locked` **295 passed, 0 failed** (57 result lines);
architecture, migrations vs `origin/main` (4 checksums/IDs valid) and 32 Python tests OK;
PDFium runtime tests (metadata_runtime, runtime, extract_runtime: 3 passed) and
`fulltext_runtime` 1 passed (extraction 259 ms wall; IPC FTS p50 1.59 / p95 1.93 ms;
scoped short term p50 1.72 / p95 2.10 ms). One earlier workspace run failed the #25 test
`daemon_lifecycle::concurrent_starts_leave_exactly_one_writer`: a losing concurrent
`papersd` exited 1 with `library I/O: File exists (os error 17)` from directory creation
before the library lock (#25 start path, not changed here); it reproduced 2 times in 24
isolated reruns and passed in the final workspace run. Recorded, not fixed here.

## Not verified / limits

- macOS PDFium/.app bundling of the extractor (#34) and release-build timings.
- GUI search (#21), REST/CLI/MCP adapters (#27–#29) — only the IPC contract exists.
- 10,000 papers / 20,000 pages, RSS and §16 p95 targets (#30). The figures above are small
  debug-build observations and are not evidence for those targets.
- Automatic rebuild scheduling after a damaged index makes a *search fail* is implemented
  in papersd but not exercised end-to-end over IPC (a corrupted index that still answers
  queries is only found by `verify()`/`search.rebuild`). `verify()` and `rebuild()` are
  tested directly; the routing (only `Corrupt` verifies, `Busy` → `WireError::Busy`) is
  covered by the storage classification test, not by an IPC test (daemon tests cannot
  force SQLITE_BUSY on a WAL read).
- The rebuild is one transaction (not batched), and it renumbers documents: cursors from
  before a rebuild can skip/repeat hits (documented in the ADR and on `SearchCursor`).
- No width/NFC/kana normalization (decomposed dakuten, full-width Latin stay distinct, #53).
- Short-term literal comparison is ASCII case-insensitive only.
- OCR for image-only PDFs is out of scope (`no_text`).
- #33 previews: extraction commits (one per ≤64 pages/4 MiB) invalidate open import
  previews like any library write; not narrowed here.
- The 20-minute run budget and the 30-minute `max_run` were exercised only with a zero
  budget in a unit test, not with a real long-running document.
