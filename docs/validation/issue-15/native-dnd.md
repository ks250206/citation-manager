# Native Finder D&D acceptance follow-up (2026-11-07)

Both explicit PDF drop intents on the PDF files / folder screen were exercised with
real Finder gestures by the user.
The gestures used the two zones on the **PDF files / folder screen**. The duplicate
Attach/NEW zones in the Library details/PDF pane were only displayed, not drop-tested.
Root CUA observed the actual app results and performed the subsequent explicit review,
save, quit/relaunch, metadata correction and attachment-host checks. This is assisted
native evidence, not an automated cross-window gesture claim. PR45 remains Draft and
#15 open pending final acceptance assessment; no other gate is waived.

## Exact runtime and operators

All screenshots01–13 use the same actual Mach-O executable SHA256
`4e7bcf996fc2089cb0454bba0da6731c1ff30391bf9f484694c3e5b0e377e423`, built at clean
source `12a20771e19d98982e45300d9a6762bf8c6e20e6`, and fixed PDFium
`8d1e46a6dbe32d7cb95279ab3def2e71e557a3d4cc49162453afa3e3b250dc5e`.
Runtime/Cargo sources are byte-identical at documentation head `8aa87e6`; no code change
or rebuild was made between the two info.pdf gestures or the scanned.pdf gesture.
[Artifact hashes, operator split and private paths](native-dnd-artifacts.json) retain
exact provenance. The dedicated launcher supplied explicit private library/settings paths.
This same private QA library began the D&D phase with one previously reviewed synthetic
paper and one metadata.pdf attachment, as recorded in the ownership follow-up. It was
not preseeded through SQL. No real user library was used.

## Existing-paper Attach intent

The user first reported dropping info.pdf into the blue Attach zone. CUA instead
observed a RegisterNew inspection, provisional metadata fields and zero attachments
saved ([01](native-dnd-screenshots/01-human-info-drop-received-as-new.jpg)). A read-only
DB check still found the original one paper/one attachment. The release position was
not recorded, so this first attempt is neither Attach success nor an established
routing defect. No Create or Commit was performed for this first result.

Asked to repeat at the blue zone's centre, the user confirmed that gesture. CUA then
observed the actual `info.pdf` report with the original paper ID fixed as attachment
destination and the “Attach ready inspected files (bibliography unchanged)” action
([02](native-dnd-screenshots/02-human-info-repeat-result.jpg)). Root clicked that action.
The app reported one attachment saved and zero failures
([03](native-dnd-screenshots/03-dropped-info-attachment-saved.jpg)). Separate read-only
verification found one paper/two attachments; the original title and DOI stayed
`XMP suggested synthetic title` / `10.1234/embedded`, and both managed blob hashes
matched their original input bytes. The second gesture succeeded with unchanged code.

## NEW-paper intent and explicit approval

The user then reported dropping scanned.pdf into the green RegisterNew zone. CUA
observed its actual 769-byte report, editable provisional title `scanned`, and unresolved
metadata warnings ([04](native-dnd-screenshots/04-human-scanned-new-drop.jpg)). Before
Preview/Create/Commit, the DB still held one paper/two attachments. Dropping and
inspecting had not created a paper or attachment.

Root opened Preview registration as NEW. The common import review showed one candidate,
Choose a decision, and disabled Commit
([05](native-dnd-screenshots/05-scanned-preview-awaiting-decision.jpg)). Root explicitly
selected Create ([06](native-dnd-screenshots/06-scanned-create-explicitly-chosen.jpg)),
then Commit. The real worker reported one attachment saved, zero failures, and a new
retained paper ID ([07](native-dnd-screenshots/07-scanned-registration-committed.jpg)).
Library then displayed the original paper and `scanned`
([08](native-dnd-screenshots/08-library-two-papers.jpg)). Post-quit read-only validation
found exactly two papers and three attachments, with each source/blob digest and paper
association correct. Neither an info.pdf paper nor a lookup-DOI paper was created.

## Restart and later metadata correction

Actual Cmd+Q ended the first owned app session with exit0. Root relaunched the identical
executable with the same private library/settings. Both rows remained
([09](native-dnd-screenshots/09-relaunch-two-papers.jpg)). Root selected `scanned`, edited
its title to `scanned — corrected after relaunch`, then saved. Screenshot10 shows the
unsaved edit after the first click missed the Save button because the dirty banner had
shifted the layout; it is **not** save proof. The next explicit Save produced “Saved
locally” and the corrected table/form title
([11](native-dnd-screenshots/11-corrected-title-saved.jpg)).

The PDF host showed scanned.pdf as Present and selected
([12](native-dnd-screenshots/12-reopened-scanned-attachment-present.jpg)). Selecting the
original paper showed both metadata.pdf and the dropped info.pdf as Present
([13](native-dnd-screenshots/13-reopened-original-two-attachments.jpg)). Actual Cmd+Q
ended the second owned session with exit0; the subsequent AX query timed out because
the window was gone, and CUA inventory then contained no owned app.

[Read-only DB reports](native-dnd-db-readonly.json) retain the separate phases, correct
paper IDs, all source/blob digests and final metadata. After the later correction the
DB still has exactly two papers/three attachments, integrity `ok`, and no foreign-key
violations. All three original PDFs remain unchanged. These are DB/file observations
separate from the native screenshots and the user's gesture confirmations.

## Scope and remaining checks

This follow-up closes the missing **D&D proof for both intents**, including filename
registration → restart → later metadata edit and attachment selection. It does not
claim the CUA drag API itself delivered the cross-window gestures, nor that the first
ambiguous attempt succeeded. There was no parser/dispatcher/runtime change and no new
product defect established by that first attempt.

No workspace/native/Python/PDFium suite or 1000-record import was rerun for this
**documentation/evidence-only** update. The prior 227/43/28 local checks and all three
CI successes apply to their recorded source/head; new-head CI and independent evidence
review remain separate. Offline DOI fallback and real failed-page retry remain
unverified additional robustness checks. The local PDF no-network argument still is
code plus observed filename fallback, not an actual network-disabled GUI experiment.
Native timing proof of an already-queued DOI completion is not claimed; deterministic
state tests and the reviewed ownership guards remain separate evidence. #34 stays HELD,
#23/#25 gates remain closed (issues open). No browser/VM/native-host/signing work occurred.

## D&D evidence review clarification

Independent review of `c9c4859` found no P0–P2. The first ambiguous attempt's retained
DB observation is now committed with retrospective method provenance explicitly marked.
The pre-commit observation remains labelled as counts-only (no missing IDs invented).
Application exit codes belong to the artifact session receipt, not SQLite results.
Duplicate Details/PDF-pane drop zones remain untested; the literal Attach/NEW intent
requirements were exercised on the dedicated PDF files / folder surface.
