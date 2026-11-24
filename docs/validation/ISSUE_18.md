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
| `core tests/search_extraction.rs` | 64-page commit batching, resume from `next_page`, encrypted/hash-mismatch/no-progress failures do not stop the queue, cancel keeps committed batch, busy is retryable, page limit → `partial`, snippet bounds/highlights, request limits |
| `pdf tests/extract_runtime.rs` (real PDFium) | 6-page corpus text incl. `日本語 αβ café`, labels `iv`/`S12`, image-only page empty, page ranges, per-page cut to 5 scalars with `truncated`, scanned PDF, encrypted (error and with password), malformed, admission limits |
| `daemon tests daemon_fulltext` (IPC, deterministic extractor) | automatic extraction after `attachment.import`; encrypted/image-only states while the job succeeds; page hits with label and highlighted snippet; scoped 2-scalar Japanese; page read bounds; DSL error over IPC; soft delete/restore; explicit `search.rebuild` and forced extraction jobs; cancel mid-document, committed pages searchable, `pending_extraction` coverage, retry resumes at the committed page and completes 320 pages; native-host cannot search, MCP read-only can search/status but not submit jobs, no extraction kind without an extractor |
| `daemon tests fulltext_runtime` (real PDFium over IPC) | corpus/scanned/encrypted/malformed imported → extraction job → states `complete/no_text/encrypted/malformed`; `"coordinate proof"` hits pages 0–3 with labels; `日本語` via trigram; `αβ` as a unicode61 token; `日本` unscoped → pages unsearched, scoped → 4 page hits |
| `storage fts::measure` | synthetic JP/EN corpus (below) |

## Measurements

RESULTS_PLACEHOLDER

## Not verified / limits

- macOS PDFium/.app bundling of the extractor (#34) and release-build timings.
- GUI search (#21), REST/CLI/MCP adapters (#27–#29) — only the IPC contract exists.
- 10,000 papers / 20,000 pages, RSS and §16 p95 targets (#30). The figures above are small
  debug-build observations and are not evidence for those targets.
- Automatic rebuild scheduling after a damaged index makes a *search fail* is implemented
  in papersd but not exercised end-to-end over IPC (a corrupted index that still answers
  queries is only found by `verify()`/`search.rebuild`). `verify()` and `rebuild()` are
  tested directly.
- No width/NFC/kana normalization (decomposed dakuten, full-width Latin stay distinct, #53).
- Short-term literal comparison is ASCII case-insensitive only.
- OCR for image-only PDFs is out of scope (`no_text`).
- #33 previews: extraction commits (one per ≤64 pages/4 MiB) invalidate open import
  previews like any library write; not narrowed here.
