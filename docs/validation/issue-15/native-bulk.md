# Latest-runtime native 1000-record workflow

Root operated the actual macOS GPUI application through CUA in a new private library,
starting with no database. The executable is the same final runtime used for the
[ownership guard](doi-pdf-ownership.md) and [assisted Finder drops](native-dnd.md):
source `12a20771e19d98982e45300d9a6762bf8c6e20e6`, executable SHA-256
`4e7bcf996fc2089cb0454bba0da6731c1ff30391bf9f484694c3e5b0e377e423`.
All 139 tracked runtime/Cargo files still match that source. Subsequent changes are
documentation/evidence only. This phase does not reuse the earlier 1000-paper library.

[Artifact manifest](native-bulk-artifacts.json) records the private paths, exact source
hashes, 72 raw screenshot hashes, observation log, exports and session outcome. Twelve
screenshots are published unchanged. Fixture input is the committed synthetic
`fixtures/bibtex/thousand.bib`, SHA-256
`d767de29467d7ebb76ba49284b721afbee31f46ed08827de03952325a115fc91`.
No direct SQL seed or headless import populated this library.

## Native actions and results

1. The native picker opened the fixture: [1000 accepted candidates, zero file
   diagnostics](native-bulk-screenshots/02-thousand-file-selected.jpg).
2. Root requested, inspected, chose Create and committed ten separate previews of 100
   rows each. No subsequent batch started automatically. Every preview started without
   decisions; Commit was disabled until the explicit choice. The final preview shows
   [rows 901–1000 awaiting decisions](native-bulk-screenshots/54-batch-10-ready-check.jpg),
   then [Create selected](native-bulk-screenshots/55-batch-10-create.jpg), then
   [1000 committed / zero skipped, no pending preview](native-bulk-screenshots/57-thousand-confirmed.jpg).
3. While a preview was still working, Library navigation and table scrolling responded:
   [batch 2, Library switch with Preparing status](native-bulk-screenshots/08-library-responsive-during-batch-02-preview.jpg)
   and [batch 8, scrolled saved rows with Preparing status](native-bulk-screenshots/39-library-scroll-during-batch-08-preview.jpg).
   The table shows a 50-row page, rather than rendering all 1000 records at once.
   This demonstrates observed interaction during work on this debug build; no release
   latency, frame rate, percentile or 16-GB/RSS performance is inferred.
4. Root created collection `Latest bulk reviewed` and tag `latest-bulk`, searched the
   last imported DOI, then assigned both memberships. The
   [metadata query returns exactly one row](native-bulk-screenshots/61-metadata-query.jpg),
   [both memberships are checked](native-bulk-screenshots/65-memberships-assigned.jpg),
   and [collection plus tag filtering returns that same row](native-bulk-screenshots/66-collection-and-tag-filter.jpg).
5. Native Save pickers, initially at HOME, saved new files in the private QA directory.
   All-active scope exported [1000 RIS records](native-bulk-screenshots/68-all-export-result.jpg)
   despite the filtered one-row table. Selected scope exported
   [one record](native-bulk-screenshots/70-selected-export-complete.jpg); current direct
   collection exported [one record](native-bulk-screenshots/72-collection-export-complete.jpg).
6. Actual Cmd+Q ended the owned executable with exit code 0. CUA's app inventory no
   longer contained the QA app. No test app remains running.

The private sequence retains ready/choice/commit/confirmation frames for every batch.
Some filenames describe attempted actions, not results: screenshots 45–47 were captured
while batch 9 was still preparing; the controls were disabled and those clicks changed
nothing. Actual batch-9 readiness/choice/commit are 48/49/50, followed by 51 confirming
900 saved. Screenshot 24 was already past preparation, so it is not concurrent-scroll
proof. Only the explicitly cited 08 and 39 support the in-flight interaction claim.

## Read-only corroboration and reparse

After normal app exit, a separate connection using URI `mode=ro` and
`PRAGMA query_only=ON` found 1000 active papers, exactly the 1000 fixture DOIs, no
attachments, one collection membership and one tag membership, both for
`10.9999/fixture.999`. SQLite integrity is `ok` and foreign-key violations are empty.
[DB report](native-bulk-db-readonly.json) keeps this separate from app exit evidence.

The production BibTeX/RIS parsers and shared normalizer then read the actual GUI output
files from an archive copy of the committed source. The retained
[verification source](native-bulk-readback.rs.txt) and [log](native-bulk-readback.log)
show 1000/1/1 unique records, no file or row-error diagnostics, and exact equality of
normalized title, bibliography and external IDs against the input keyed by DOI.
The output uses the documented private cm-v1 extension; this is app readback, not an
independent BibTeX/RIS implementation or TeX/Biber interoperability test. Organization,
attachments and internal IDs are deliberately outside bibliography export.

## Acceptance scope

This closes the previously missing **latest-runtime** native
1000 import → classify → metadata search → re-export path and supplies observed
interaction during bulk work. Earlier native results and their binary limitations
remain historical; this run does not repeat their unrelated PDF/settings scenarios.

For the literal offline PDF filename-registration requirement, root accepts the
code-based argument documented in the ownership report: PDF inspection, filename
fallback, review/commit and later metadata editing have no HTTP operation or required
DOI lookup. The scanned/malformed/encrypted fallback and post-restart edit were observed
locally. This supports operation without network access; it is **not** a network-disabled
GUI experiment and does not establish offline DOI fallback.

Additional robustness checklist items remain unverified: offline DOI fallback, a real
failed-page retry, freely overlapping DOI/PDF cancellation and a queued late DOI reply
arriving natively. Deterministic tests and observed ownership guards remain separate
evidence. The duplicate Details/PDF-pane drop areas have not received native drops;
both explicit intents were tested on the dedicated PDF files / folder screen.
No system network configuration or #34 isolation boundary was changed.

PR45 stays Draft and #15 open pending independent acceptance review and current-head
CI. #34 stays HELD; #23/#25 gates remain closed while their issues remain open. No
runtime code, pins, migrations or feature boundaries changed in this follow-up.
