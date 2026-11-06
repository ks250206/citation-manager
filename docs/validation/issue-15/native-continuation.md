# Native continuation — 2026-11-07

**PR45 remains Draft and #15 remains open.** This continuation adds native evidence
for collection changes, size limits, dirty guards, unrelated attachment delivery and
repeated preview release/Resume. D&D, offline DOI, simultaneous cancellation and failed
page-query retry still do not have successful native proof.

## Exact executable and private scope

- Starting remote/source head: `9ea22dd2d343cfa921ba12377cd24ba62500adbf`.
- Main verified before and after GUI work: `9e7e66e3e284a047096eae0707b823233206fe8c`.
- T3-prepared checkout: `t3-3485176f`; normal fast-forward of the existing PR branch.
- Fresh private root: `/private/var/folders/y1/84n75pz56qg1b36t882r8n900000gn/T/cm-pr45-native-348-v4ut62vv`.
- Real arm64 Mach-O app executable SHA-256:
  `0eaaec984e238371415d264a244529ce357bae175119313335f036826c70c682`.
- Bundled `Contents/Frameworks/libpdfium.dylib` SHA-256:
  `8d1e46a6dbe32d7cb95279ab3def2e71e557a3d4cc49162453afa3e3b250dc5e`.
- Built here with `cargo build --locked -p cm-daemon --features desktop-bootstrap --bin citation-manager`.
  No product code changed. Full tracked source hashes were recorded before launch and
  verified unchanged after observation, before writing this documentation.
- All 51 retained screenshots (numbers 01–52 except 27) and action ranges map to that
  executable in [the artifact manifest](native-continuation-artifacts.json). The private
  manifest retains the complete tracked source snapshot. Screenshot 27 does not exist:
  that coordinate probe returned a CUA error before an image was produced.
- All UI input and screenshots used documented `mcp__cua_repl` native APIs. Actual
  JPEG bytes from `getScreenshot()` were saved with `node:fs/promises.writeFile`.
  Pickers used current AX state; GPUI controls used fresh screenshots. No DB preseed,
  mock drop, injected product response, HTML GUI substitute or alternate input API.

The library started empty. Two manual records and one bibliography row were explicitly
reviewed and committed through the UI. The earlier ten-batch 1000-record proof in
[native-awake.md](native-awake.md) remains earlier-binary evidence; it was not repeated
or relabelled as this executable's observation. The initial missing private settings
parent produced a visible defaults warning; this continuation makes no new settings
persistence claim.

## Observed native results

| Path | Actual result | Screenshot numbers |
|---|---|---|
| Collection rename/move/delete | Created Parent Alpha and Child Beta. Renamed child to Moved Beta, moved to Root, then back under Parent Alpha. Deleted the disposable parent; child remained and became Root. Saved paper remained. | 05–11, 51 |
| Oversize PDF paths | Native picker selected 33 MiB and 257 MiB synthetic files. The first displayed `PDF metadata unresolved: LimitExceeded` and editable filename fallback. The second displayed `Excluded: exceeds file size limit`, without registration controls. Neither was committed. Screenshot 14 is still pending; 15–16 are settled results. | 14–16 |
| Dirty selection / Trash guards | Edited the saved anchor title to a sentinel, selected the other record, then clicked Move paper to Trash. Both retained the anchor, sentinel and persistent unsaved banner, with the explicit save/discard instruction. | 19–20 |
| Attach/Get with dirty existing form | Explicit AttachExisting saved info.pdf to the anchor. The report and subsequent Get retained the sentinel and unsaved banner. | 31–32 |
| Attach/Get with unrelated non-PDF preview | Inspected scanned.pdf for the anchor, then opened an unrelated manual preview. Returned to the retained attachment intent and explicitly attached. The report/Get preserved the manual candidate, its uncommitted state and dirty form. | 33–35 |
| Repeated slot release and Resume | Three-row bibliography, batch size 1; only Saffron row 1 was approved/committed. Six manual-preview/explicit-Resume cycles returned to Tundra row 2 at 1/3 processed, without slot exhaustion. Selected Create before the final pause; fresh Resume/preview reset it to Choose a decision and disabled Commit. Stopped file without committing rows 2–3. | 36–50 |

[Read-only corroboration](native-continuation-db-readonly.json), collected after normal
quit using SQLite URI `mode=ro`, shows exactly three active records: GUI Delta Anchor,
Quartz Mountain Reference and Saffron horizon microscopy. There is one Root collection,
Moved Beta. Both attachments belong to the anchor; each source digest equals its managed
blob digest. The sentinel, uncommitted manual candidate and later bibliography rows were
not saved. This corroborates actions; it does not substitute for their GUI evidence.

## Remaining native gates and reproduction constraints

- **D&D for both intents:** Finder opened the private fixture folder, then macOS's
  native right/left arrangement paired Finder and the test app. Documented `drag()`
  calls toward Attach and Register zones produced no visible report change. An alternate
  coordinate probe returned `Computer Use server error -10005: windowNotFoundAtPosition((2175.0, 134.0))`.
  Source selection/coordinate mapping and cross-window drag delivery were not established
  reliably. This is neither successful ExternalPaths evidence nor an established product
  defect. No fabricated event or picker result is counted as a drop. Screenshots21–30
  retain the attempts. A working supported cross-window gesture or human native check
  is still required for each intent. Finder's previous window size was restored.
- **Offline DOI fallback:** Read the two earlier failed process-local sandbox setup
  records; both failed CUA availability and exited143. Did not repeat those launches.
  Considered an unreachable per-process proxy but rejected it before launch: the actual
  DOI client explicitly calls `no_proxy()` (`crates/metadata/src/doi/mod.rs`). No new
  network-restricted app was launched. A safely isolated network-failure setup that
  remains observable through CUA is still needed. No global network/WiFi/security/profile
  setting changed, and no offline success is claimed.
- **Simultaneously pending DOI/PDF cancellation in both directions:** Not observed.
  Current native controls reject DOI lookup while PDF is busy (`locked()`), and entering
  PDF/inspection invalidates the pending DOI generation. Those production guards do not
  provide simultaneous native evidence. Existing separate-token state/worker tests stay
  separate; root must resolve/exercise the required native scenario without a public
  fault/test API or removing the production guards merely to manufacture coverage.
- **Failed forward/back query and same-direction retry:** No real failed page query
  was exercised. No DB mutation, product fault switch or fake GUI response was introduced.
  Keep this gate open until a safe controlled real query failure can be reproduced.

No changes to commands/keymap/search/daemon, dependency pins, SQL or migration manifest.
All 636 locked dependency identities/checksums and both SQL migrations/manifest match main.
#34 stays **HELD**, #23/#25 stay closed. No browser/VM/account/native-host registration,
signing, review agents, watch or merge was performed. Root retains independent final review.
The owned normal app exited0 after actual Cmd+Q; CUA inventory then contained no owned
Citation Manager app. No prior QA library, settings or evidence was edited.

The fresh native executable build, formatting, architecture validator, migration
preservation against refreshed `origin/main`, all 23 Python validators, artifact/link
integrity and `git diff --check` passed. [Commands and log hashes](native-continuation-checks.json)
identify this continuation's checks. The earlier full Rust/native/runtime test results
remain in the awake report; no Rust tests were rerun for this documentation-only change.
The final pushed head's CI and independent review remain root's responsibility.

Selected original CUA images:

- [Parent deleted, child retained](native-continuation-screenshots/11-parent-deleted-child-retained.jpg)
- [Both size-limit outcomes](native-continuation-screenshots/15-oversize-results.jpg)
- [Dirty Trash guard](native-continuation-screenshots/20-dirty-trash-guard.jpg)
- [Dirty form after Attach/Get](native-continuation-screenshots/32-get-preserved-dirty-fields.jpg)
- [Unrelated preview after Attach/Get](native-continuation-screenshots/35-manual-preview-survived-report-get.jpg)
- [Create before pause](native-continuation-screenshots/48-row-two-create-before-pause.jpg)
- [Fresh decision after sixth Resume](native-continuation-screenshots/50-resume-six-fresh-decision-required.jpg)
