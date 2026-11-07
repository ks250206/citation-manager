# DOI/PDF request ownership follow-up (2026-11-07)

PR45 remains Draft; #15 remains open. This follow-up fixes the reachable P2 race found
in the independent review of `ca29e58`. It does not waive D&D, offline DOI or real failed-page retry.

## Problem and fix

A retained PDF preview could coexist with a pending DOI lookup after Library → Lookup
DOI → Import review. Committing the PDF then allowed a late DOI reply to replace the
form and release the preview while CommitNew was using it. The earlier continuation
only exercised other navigation paths; its guard description was incomplete.

The actual native request dispatcher now invalidates the DOI ticket and cancels only
the DOI token before every Editor, Import or PDF ownership request. The unchanged
receive ticket check rejects even a response queued before cancellation. Import-review
navigation does the same, preserving the retained preview and decisions. DOI reply
and manual-preview handlers also reject replacement while an owned operation is busy.
Read-only Query/Organization/Membership/Attachments/settings/export requests retain a
live lookup. DOI and PDF tokens remain separate. No test/fault API or network policy
change was added.

## Executable and scope

Every screenshot01–18 in this run uses source `12a20771e19d98982e45300d9a6762bf8c6e20e6`
(clean snapshot after the fix and normal main `1304028` integration), executable SHA256
`4e7bcf996fc2089cb0454bba0da6731c1ff30391bf9f484694c3e5b0e377e423`, and pinned
PDFium `8d1e46a6dbe32d7cb95279ab3def2e71e557a3d4cc49162453afa3e3b250dc5e`.
[Source/input/binary/screenshot hashes and private paths](doi-guard-artifacts.json)
retain the full mapping. No Rust source changed during or after this run; subsequent
changes are documentation/evidence only. A fresh empty private library/settings and
synthetic PDFs were used. No real user library or prior QA data was opened for writing.

## Actual native observations

1. Inspect the private metadata.pdf, preview NEW registration, explicitly choose Create
   ([07](doi-guard-screenshots/07-pdf-create-chosen.jpg)). Keep that PDF preview while
   visiting Library. Enter public DOI `10.1038/nphys1170` and start the real provider
   lookup; actual loading and active Cancel are visible
   ([11](doi-guard-screenshots/11-doi-in-flight.jpg)).
2. Click Import review while lookup is pending. The app reports “DOI lookup cancelled;
   retained import review is unchanged”. The same synthetic title/embedded DOI and
   Create decision remain, with Commit enabled
   ([12](doi-guard-screenshots/12-review-navigation-cancelled-doi.jpg)).
3. Explicitly Commit that retained PDF preview. The real worker reports one attachment
   saved, zero failures ([13](doi-guard-screenshots/13-retained-pdf-commit.jpg)).
   This confirms the guarded navigation/commit path. It does not prove that a late
   provider callback actually arrived; that queued-reply case is deterministic state
   test evidence below, not a native observation or reproduction of the original bug.
4. Inspect a folder of 1000 unique synthetic PDF copies (trailing comment only), without
   approving any new registration. While PDF is working, visit Library: Lookup DOI and
   Cancel lookup are visibly disabled
   ([15](doi-guard-screenshots/15-library-during-pdf-inspection.jpg)).
   Inspection subsequently completed 1000 assessments and zero saves. Before the attempted
   DOI click, completion had already switched back to PDF; screenshot16 therefore does
   **not** prove that click was rejected. The later Cancel click was also after completion
   (screenshot17); no cancellation is claimed for this folder run.
5. The final library still shows only the original one synthetic paper
   ([18](doi-guard-screenshots/18-library-final-one-paper.jpg)). Actual Cmd+Q ended the
   owned process with exit0. The post-quit AX request timed out; CUA app inventory then
   showed no owned app. A separate read-only SQLite connection found exactly one paper,
   one attachment, integrity `ok`, no foreign-key violations, and no saved lookup DOI.
   Source, DB digest and blob bytes match
   ([read-only report](doi-guard-db-readonly.json)). The 1000 inspections wrote no papers
   or attachments; this is not a 1000-record import rerun.

## Automated validation (separate from GUI)

[Commands and output hashes](doi-guard-checks.json): locked workspace tests **227**,
native desktop tests **43**, Python **28**, fmt, workspace and native Clippy with warnings
denied, workspace/native builds, architecture and immutable migration checks all passed.
The three new request-policy tests exercise the production dispatch helper and ticket
filter predicate, using real independent cancellation tokens: Editor/Import/Pdf rejects
an already-queued DOI reply without cancelling PDF; unrelated reads retain it; review
navigation invalidates the old ticket but permits a later fresh lookup. Existing
separate-token and retained-review/worker tests also remain in the full suite.

All 636 dependency identities/checksums and the migration tree are preserved against
main `1304028`; #52 commands/keymap and #53 experiment sources remain unchanged. No
fixed-PDFium runtime test was rerun in this follow-up; the actual GUI did inspect and
save a PDF using the fixed library. Historical runtime suite results remain historical.
Final pushed-head CI and independent review are separate and pending at evidence creation.

## Remaining acceptance boundaries

The simultaneous-cancel robustness scenario is now handled through ownership invalidation
and guarded native navigation; a freely overlapping pair of DOI/PDF operations was not
manufactured. The old scenario was reachable and must not be labeled unreachable by design.
Native timing proof of a queued stale completion remains absent, while deterministic
queued-ticket and independent-token tests cover rejection/isolation.

Both external cross-window D&D intents remain unverified and are literal #15 acceptance
requirements. Offline **PDF** filename/manual registration is a local-only workflow:
cm-pdf and the PDF worker have no HTTP client, and observed malformed/encrypted/oversize
fallbacks retain editable filenames. This code/observed-fallback argument is not an
actual network-disabled GUI test. Offline **DOI** fallback and a real failed-page retry
remain unverified additional robustness checks in the native checklist. No global
network setting or synthetic production fault switch was introduced. #34 remains HELD;
#23/#25 gates remain closed (the issues remain open).
