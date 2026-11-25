# MVP acceptance (#32): DESIGN §24 traceability

Status on 2026-11-25, branch `claude/issue-32-acceptance` (stacked on PR #79 → #78; main at
`3811f1d` with #30 merged in). **The MVP is not complete.** Seven of the 17 §24 criteria are
passed with evidence; the other ten are partial, mostly because the macOS release itself (real
Mac, Developer ID signing, store-published extensions, the 16 GB reference machine) has never
been exercised. Nothing below is marked passed without a test, script, workflow run or
recorded native session in this repository; "Linux" means the Linux container used by the
implementing agents (Xvfb for GUI runs), "macOS VM" a GitHub-hosted `macos-15-arm64` runner,
"real Mac" an Apple Silicon machine (native CUA sessions in #10/#15, headless benchmark in #47).
Linux is itself a P2 target (DESIGN §2.3); it is used here as the automatable stand-in for the
platform-neutral parts, never as evidence for macOS-specific behaviour.

Result values: **passed** (the whole criterion has concrete evidence), **partial** (named parts
are verified, others are not), **not verified** (no evidence), **failed** (evidence shows the
criterion is not met). Features outside the MVP or in P1/P2 (DESIGN §2.2/§2.3: sync,
Word/Google Docs, CSL, OCR, Zotero migration and annotation import, Safari, Linux/Windows
distribution, …) are not completion criteria and are not counted below.

## Release E2E (this issue)

[`tests/e2e/release/run.py`](../../tests/e2e/release/README.md) installs a release build into a
throw-away home and runs the §20 M5 exit flow in one process tree:
new install → browser save with GUI and daemon stopped → GUI start / search / quit / restart →
search via CLI, REST and MCP → MCP/REST annotations → annotated PDF export → BibTeX/RIS
round trip, backup by copy and restore → teardown, plus privacy defaults and (Linux) a
loopback-only network namespace that counts every attempt to reach another host.

| Run | Environment | Result | Evidence |
|---|---|---|---|
| Linux, `--browser chromium --gui --offline` | Linux container, Ubuntu 24.04, release build of this branch, pinned PDFium 8125, Playwright Chromium 1243 on Xvfb (no window manager), network namespace with only `lo` | **passed**, 89 checks, 0 failed, no step skipped | [report](release-e2e/linux/report.json), [logs](release-e2e/linux/logs/), [screenshots](release-e2e/linux/screenshots/) |
| macOS, non-browser steps (`package-macos.yml`) | macOS VM (`macos-15-arm64`, Darwin 24.6.0), app copied out of the mounted ad-hoc signed `.dmg` | **passed**, 66 checks, 0 failed; `gui_restart` and `offline_doi` skipped (Linux-only) | run [45613061723](https://github.com/ks250206/citation-manager/actions/runs/45613061723) on `cc2f58b`, [report](release-e2e/macos/report.json), [logs](release-e2e/macos/logs/) |

What the Linux run shows (each line is one or more named checks in the report):

- **Install**: `install_linux.py` puts all five executables, the checksum-pinned `libpdfium.so`,
  its notices and a desktop entry with an explicit library into a fresh prefix, registers the
  dev extension for the Chromium profile and pairs the host.
- **Privacy defaults** on the new library: library 0700, DB and owner credential 0600, automatic
  `backups/initial.sqlite`, HTTP API `enabled=false` and no TCP listener, the default MCP grant
  is read-only (12 tools, 4 `readOnlyHint`; `create_collection` refused), no telemetry crate in
  `Cargo.lock` (heuristic). In the loopback-only namespace every product step made **zero**
  attempts to reach another host (`Ip:OutNoRoutes` delta 0); the deliberate DOI lookup made 24
  (positive control) and failed explicitly (`unresolved`, exit 4) without changing the library.
- **Browser save, GUI closed**: no GUI process, the daemon had idled out; the #24 extension E2E
  (`run.mjs --installed`) used the installer's registration and pairing: 26/26 checks (activeTab
  only after the shortcut, preview, commit, PDF job failure + retry, hidden duplicate, provisional
  save with corrections). The installed `papers` then read both papers back.
- **GUI restart**: the installed GUI started (window in 0.5 s), started/joined `papersd`; Ctrl+F
  search → first row → Ctrl+K "Copy citation" put the browser-saved paper on the clipboard; Ctrl+Q
  quit cleanly; second start found the CLI paper the same way and opened its PDF tab
  ([screenshot](release-e2e/linux/screenshots/gui-2-pdf-tab.png)); the daemon idled out after.
  The palette history was persisted (0600) — in `<library>/ui-settings.json`, not in the GUI's
  `--settings` file, because the browser step's PDF job had started `papersd` first (defect
  below).
- **Search through CLI, REST and MCP** returned the same paper IDs for a full-text phrase, a
  browser-saved title and `日本語`; full-text hits carry `page_number` 1 and
  `paper://…?page=1`; REST refused a missing token (401) and a foreign `Host`.
- **Annotations and export**: REST note (read-only token → 403 first), MCP highlight refused with
  the default grant and accepted after `papers-mcp grant --write`; the owner confirmed both AI
  annotations and wrote the annotated copy through the GUI's IPC request: complete, 1 highlight +
  1 note, max position error 0.0 pt, original SHA-256 unchanged, and pypdf (not PDFium) sees
  `/Highlight` and `/Text` on page 1. REST was disabled again and its listener closed.
- **Portability**: BibTeX and RIS exports re-imported into fresh libraries with identical titles;
  the stopped library copied (`cp -a`) and restored elsewhere served the same papers, PDF, full
  text and annotations.

Not covered by the release E2E: Firefox (no binary in this container; #24 ran Firefox ESR 153),
the toolbar popup window (the real shortcut grants activeTab, the popup document then runs in a
tab), the GUI's native save dialog and its Confirm button (the export and confirmation use the
same IPC requests), a live DOI lookup, signing/Gatekeeper, a real Mac.

### macOS run

[Run 45613061723](https://github.com/ks250206/citation-manager/actions/runs/45613061723) of
`package-macos.yml` on `cc2f58b` succeeded end to end (bundle smoke, ad-hoc signed bundle,
`.dmg` run read-only, hardened runtime, Chrome for Testing and release Firefox probes, signing
preflight `UNVERIFIED: no Developer ID`). Its new step mounted the `.dmg`, copied the app into
an `Applications` folder of a temporary home and ran the release E2E without a browser and
without the GUI: install (bundled PDFium and licenses), privacy defaults, CLI seeding with
PDFium extraction from the bundle, a host-level capture (`hello`/`capture.preview`/
`capture.commit` written to the bundle's host with no GUI and no daemon running), the same
search results from CLI, REST and MCP, MCP/REST annotations, the owner's annotated export
through the bundle's `papersd` (complete, 1 highlight + 1 note, original unchanged), BibTeX/RIS
round trip and backup by `ditto`/restore. pypdf is not installed on the runner, so the
independent re-read was skipped there (it ran on Linux).

Observed on the VM, not counted as failures: `papers server stop` is refused on macOS
("needs socket peer credentials", exit 7), so the run waited for the idle exit; each
`papersd token issue` took 20.2 s and stored the secret in the 0600 fallback file — the Keychain
was not usable for the temporary `HOME` of a non-interactive runner. Keychain storage on a real
Mac remains unverified (#26).

## §24 criteria

| # | §24 criterion (quoted) | Owning issues / PRs | Verification | Environment | Result | Evidence / what is missing |
|---|---|---|---|---|---|---|
| 1 | 「macOS でインストール・起動・再起動でき、文献データが保持される。」 | #10, #34 (PR #78), #31 (PR #79), #32 | `smoke.py` (layout, `open -n` after a move, relaunch), `.dmg` mount, release E2E `install`/`gui_restart`; #10 native save → ⌘Q → relaunch | macOS VM (`.app`, `.dmg`); real Mac (#10 dev build, not a bundle); Linux (release E2E) | **partial** | [APP_LAYOUT_POC](APP_LAYOUT_POC.md), [ISSUE_31](ISSUE_31.md), [ISSUE_10](ISSUE_10.md), release E2E. Missing: install from the `.dmg` by Finder on a real Mac, first launch with quarantine/Gatekeeper, a Developer ID signed and notarized build |
| 2 | 「文献 10,000 件の一覧を実用的な応答速度で操作できる。」 | #47, #30 (PR #77), #15 | `scripts/benchmarks/run.py` (#47), `representative.py` + `benchmark.yml` (#30): warm start, collection switch, metadata/full-text p95; #15 native 1000-row run | real Mac M4 Max 36 GB (#47 headless); Linux; macOS VM 7 GB; Linux Xvfb GUI RSS | **partial** | [BACKEND_BENCHMARK](BACKEND_BENCHMARK.md), [ISSUE_30](ISSUE_30.md): headless p95 within §16.2 on all. Missing: GUI list scrolling at 10,000 (frame time never measured), the 16 GB Apple Silicon reference machine |
| 3 | 「コレクション・タグ・複合フィルタを設定できる。」 | #12 (PR #40), #15 (PR #45) | `crates/storage/tests/library_organization.rs` and core/domain tests; #15 native CUA session | Linux/macOS automated; **real Mac** native GUI | **passed** | [ISSUE_12](ISSUE_12.md); [native-awake](issue-15/native-awake.md): parent/child collections, two tags, AND→1 / OR→2, combined collection/tag/keyword/author/year/type filter, rename/move/delete ([continuation](issue-15/native-continuation.md)) |
| 4 | 「DOI のみから文献を追加でき、重複登録を防止できる。」 | #14 (PR #44), #33 (PR #41), #15, #28, #29 | reqwest transport against loopback Crossref/DataCite fixtures, resolve → common preview → duplicate decision; CLI `add doi` with a scripted daemon; MCP `add_paper_by_doi` tests; release E2E offline DOI (explicit failure, nothing saved) | Linux/macOS automated; real Mac GUI DOI lookup (#15 awake run, cancel/ownership) | **partial** | [ISSUE_14](ISSUE_14.md), [ISSUE_33](ISSUE_33.md), [doi-pdf-ownership](issue-15/doi-pdf-ownership.md). Missing: a live Crossref/DataCite lookup end to end (no public network in any test lane), the offline DOI fallback in the GUI (#15 gate) |
| 5 | 「PDF を閲覧・検索・テキスト選択できる。」 | #11 (PR #37), #17 (PR #59), #18, #21 (PR #64), #34 | PDFium corpus runtime tests (`cm-pdf --features runtime`), `layouts_runtime` (#30), Linux GUI runs (#17 view/select/copy, #21 find/jump), bundle smoke (PDFium render/text from `papersd` in the `.app`), release E2E PDF tab | Linux Xvfb GUI; macOS VM (bundled PDFium via `papersd`/REST, not the GUI viewer); real Mac (#11 PDFKit cross-check, no viewer) | **partial** | [issue-17](issue-17/README.md), [PDFIUM_POC](PDFIUM_POC.md), PR #64, [APP_LAYOUT_POC](APP_LAYOUT_POC.md). Missing: the GUI viewer and text selection on macOS (Metal, ⌘C, trackpad) |
| 6 | 「ハイライト・コメントが保存され、注釈付き PDF を書き出せる。」 | #19 (PR #61/#62), #20 (PR #65), #29, #27 | `crates/{core,daemon,desktop}/tests/annotations.rs`; `pdf_export_runtime` + `validate_export.py` (pypdf + poppler, 4 rotations, CI `linux-native`); #19 Linux GUI highlight → restart; release E2E MCP/REST annotations → owner export | Linux (GUI and CI); macOS VM (release E2E export through the bundle's `papersd`, [macOS run](#macos-run)) | **partial** | [issue-19](issue-19/README.md), [issue-20](issue-20/README.md), [ISSUE_30 validate_export](ISSUE_30/linux/validate_export.json), release E2E. Missing: choosing a file in a real save dialog (no XDG portal here; NSSavePanel never driven), the GUI on macOS |
| 7 | 「全文検索結果から正しい PDF ページへジャンプできる。」 | #18, #21 (PR #64) | `cm-storage` FTS tests, `fulltext_runtime`, `crates/desktop/tests/search_ui.rs`; Linux GUI: hit → PDF tab at p. 1, `n` → rotated p. 2; release E2E page targets via CLI/REST/MCP | Linux Xvfb GUI; Linux/macOS VM headless | **partial** | PR #64, [ISSUE_18](ISSUE_18.md), [ISSUE_30](ISSUE_30.md) (recall 1.0 at 20,000 pages). Missing: the jump in the macOS GUI |
| 8 | 「BibTeX と RIS のインポート・エクスポートを実行できる。」 | #13 (PR #43), #33, #15, #28 | `bibliography_behavior`, exchange round-trip tests; #15 native 1000-row import → classify → re-export → reparse; release E2E CLI import/export and re-import in fresh libraries | Linux/macOS automated; **real Mac** native GUI; Linux and macOS VM release E2E | **passed** | [ISSUE_13](ISSUE_13.md), [native-bulk](issue-15/native-bulk.md) (1000/1/1 exact records), release E2E `portability.*_round_trip_titles` |
| 9 | 「Chrome/Firefox から論文ページを保存できる。」 | #22 (PR #63), #23 (PR #66), #24 (PR #67), #34, #31 | `run.mjs` with the real extension (Chromium, Firefox ESR 153); `browser_smoke.py` probe extension on macOS (Chrome for Testing, release Firefox); release E2E browser step | Linux (real extension, both browsers); macOS VM (probe extension, not the real popup) | **partial** | [ISSUE_24](ISSUE_24.md), [ISSUE_23](ISSUE_23.md), [ISSUE_31](ISSUE_31.md), release E2E. Missing: store-published extension IDs (`extension-ids.json` `release` is empty, so a release build of the extension refuses to build and users can only sideload the development ID), branded Chrome, the real extension on macOS, an AMO-signed add-on |
| 10 | 「GUI を閉じた状態でもブラウザから保存できる。」 | #23, #24, #25 | the same E2Es check that no GUI and no daemon run before the save, the host starts `papersd`, and the paper is persisted after the browser quits | Linux (Chromium, Firefox ESR, real extension); macOS VM (probe extension, host + `papersd` from the bundle) | **passed** | [ISSUE_24](ISSUE_24.md) (both browsers), release E2E `browser.gui_not_running` / `browser.daemon_stopped_before_save` / `browser.extension_e2e_passed`, [APP_LAYOUT_POC](APP_LAYOUT_POC.md) Chrome/Firefox rows |
| 11 | 「Command Palette と Vim キーバインドで主要操作が完了する。」 | #52 (PR #55), #16 (PR #57), #21 | `commands_keymap.rs`, `palette.rs`, keymap parse tests; Linux GUI: Ctrl+K, Vim j/k/gg/G/y, custom/invalid keymap.json; release E2E Ctrl+K "Copy citation" | Linux Xvfb GUI | **partial** | [ISSUE_52](ISSUE_52.md), PR #57, [keyboard checklist](../../tests/e2e/keyboard/README.md). Missing: macOS ⌘ shortcuts and Japanese IME composition natively; #15 notes "no command/keymap native handlers were connected" in its macOS run, which predates #16 |
| 12 | 「ローカル REST API が認証付きで読み書きできる。」 | #26 (PR #69), #27 (PR #71) | `daemon_rest` (every endpoint's 401/403/revoked/Host/Origin), `daemon_api`, OpenAPI contract test; release E2E REST read/write/scope/Host/disable | Linux automated; Linux release E2E; macOS VM (bundle smoke: REST render; release E2E) | **passed** | [ISSUE_26](ISSUE_26.md), [ISSUE_27](ISSUE_27.md), [openapi.json](../openapi.json), release E2E `rest.*` |
| 13 | 「MCP から文献検索・本文取得・書き込みが権限に応じ実行できる。」 | #29 (PR #70), #26 | `cm-mcp` unit + stdio subprocess tests (read-only default, write grant, stdout purity); release E2E with a raw JSON-RPC client: search/full text with sources, write refused then allowed after the owner's grant | Linux automated; Linux release E2E; macOS VM release E2E | **passed** | [mcp-server ADR](../ADR/mcp-server.md), PR #70, release E2E `privacy.mcp_*`, `mcp.*`. Not verified: a third-party desktop MCP client application |
| 14 | 「CLI から文献の追加・検索・エクスポートを行える。」 | #28 (PR #68) | `crates/cli/tests` subprocess tests against real `papersd`; release E2E `add file`, `import`, `search`, `fulltext`, `export`, `server stop` | Linux automated; Linux and macOS VM release E2E; macOS VM smoke (CLI via PATH symlink) | **passed** | [ISSUE_28](ISSUE_28.md), [APP_LAYOUT_POC](APP_LAYOUT_POC.md), release E2E `cli.*`. `add doi` with a live provider: see #4 |
| 15 | 「バックアップ・リカバリ手段が明示されている。」 | #9 (PR #38), #50 (PR #51), #30, #31 (PR #79) | storage `recovery` tests (WAL backup → restore), `crash_recovery` SIGKILL, `daemon_recovery` (#30); documented procedures; release E2E backup by copy → restore elsewhere | Linux/macOS automated; Linux and macOS VM release E2E (`cp -a` / `ditto`) | **passed** | [INSTALL §6](../INSTALL.md#6-backup-and-restore), [RECOVERY](../RECOVERY.md), [ISSUE_9](ISSUE_9.md), [CRASH_RECOVERY](CRASH_RECOVERY.md), [ISSUE_30](ISSUE_30.md). The guides arrive with PR #79. The verified portable bundle backup of the storage layer has no app/CLI command yet (copy of a stopped library is the documented route) |
| 16 | 「PDFium 同梱・署名・配布ライセンスを確認済みである。」 | #11, #34, #31, PR #80 | fetch with pinned checksums; bundle layout/`otool`; notices generator (423 crates) + PDFium notices bundled; `release_sign.py preflight` | macOS VM | **partial** | [PDFIUM_POC](PDFIUM_POC.md), [ISSUE_31](ISSUE_31.md), [LICENSES](../LICENSES.md). Bundling and third-party notices: verified. Missing: Developer ID signature and notarization (`UNVERIFIED: no Developer ID`), and the project's own license (MIT chosen in PR #80, not merged; `LICENSES.md` still says "not yet chosen") |
| 17 | 「RSS / 起動時間 / 検索速度を計測し、測定結果を公開できる。」 | #47, #30 | `representative.py --gui`, `benchmark.yml`; raw JSON committed | Linux; macOS VM 7 GB; real Mac M4 Max (#47 headless only) | **partial** | [ISSUE_30](ISSUE_30.md) and [ISSUE_30/](ISSUE_30/): measured and published, but not on the 16 GB Apple Silicon reference machine, macOS GUI RSS and frame drops unmeasured, and the measured PDF-view RSS (409.7 MiB, Linux) is above the §16.2 target of 350 MB |

**Count: 7 passed (#3, #8, #10, #12, #13, #14, #15), 10 partial (#1, #2, #4, #5, #6, #7, #9, #11,
#16, #17), 0 not verified, 0 failed.** The release E2E adds evidence to #1, #4–#15 but changes
no row to passed on its own: the missing parts are outside what a Linux container or a hosted VM
can show.

## Milestones and child issues

DESIGN §20 exits. "Exit met" means the milestone's Exit sentence has evidence; the parent stays
open while a child is open or a gate below remains.

| Milestone / issue | State | Exit / acceptance | Blockers |
|---|---|---|---|
| **#1 M0** 技術検証と永続化 | open | Exit met: sample paper saved → restart → viewed (#10 real Mac; release E2E Linux) | #34 open |
| #7 workspace/CI/contracts | closed | met ([TEST_MATRIX](TEST_MATRIX.md)) | — |
| #8 SQLite/CRUD | closed | met ([ISSUE_8](ISSUE_8.md)) | — |
| #9 attachments/backup | closed | met ([ISSUE_9](ISSUE_9.md)) | — |
| #10 GPUI vertical slice | closed | met on a real Mac ([ISSUE_10](ISSUE_10.md)); Japanese IME candidate selection not verified | — |
| #11 PDFium PoC | closed | met ([PDFIUM_POC](PDFIUM_POC.md)) | — |
| #34 `.app` layout | open (PR #78) | hosted-runner evidence complete ([APP_LAYOUT_POC](APP_LAYOUT_POC.md)) | real Mac: Finder install, quarantine, concurrent first start, sleep/logout, TCC |
| #52 command registry, #53 FTS tokenizer | closed | met ([ISSUE_52](ISSUE_52.md), [ISSUE_53](ISSUE_53.md)) | — |
| **#2 M1** 文献管理・DOI・BibTeX/RIS | open | Exit met: 1000 BibTeX/RIS records imported, classified, searched and re-exported without corruption on a real Mac ([native-bulk](issue-15/native-bulk.md)) | #15 gates below; live DOI |
| #12 collections/tags/filters | closed | met | — |
| #13 BibTeX/RIS | closed | met | — |
| #14 DOI resolver | closed | met with loopback providers; live providers never contacted | live Crossref/DataCite |
| #15 3-pane workbench | closed | README: **native CUA acceptance not complete** — offline DOI fallback, a real failed-page retry and native queued-late-arrival unverified; duplicate Details/PDF drop zones untested | those gates |
| #16 palette/Vim | closed | met on Linux GUI; macOS ⌘/IME not verified | real Mac |
| #33 ImportService | closed | met ([ISSUE_33](ISSUE_33.md)) | — |
| #46 search DSL | closed | met ([ISSUE_46](ISSUE_46.md)) | — |
| **#3 M2** PDF・全文検索・注釈 | open | Exit met on Linux: multi-line and rotated annotations saved, restarted (#19 GUI), exported and re-read (#20 runtime + pypdf/poppler, 4 rotations) | macOS GUI not exercised; save dialog |
| #17 viewer, #18 extraction/FTS, #19 annotations, #20 export, #21 search UI | closed | met on Linux (GUI under Xvfb, PDFium 8125) | macOS GUI; #20 real save dialog |
| **#4 M3** Browser Capture | open | Exit met: GUI closed → browser save (Linux Chromium + Firefox ESR with the real extension; macOS VM probes) | store extension IDs; real Mac |
| #22 capture service, #23 native host, #24 extension | closed | met ([ISSUE_22](ISSUE_22.md), [ISSUE_23](ISSUE_23.md), [ISSUE_24](ISSUE_24.md)) | store IDs (#24), branded Chrome |
| **#5 M4** daemon・REST・MCP・CLI | open | Exit met: with no GUI running an MCP client searches and reads full text; after the owner's write grant it adds annotations (release E2E on Linux and the macOS VM) | — |
| #25 papersd/IPC, #26 tokens/grants, #27 REST, #28 CLI, #29 MCP | closed | met ([ISSUE_25](ISSUE_25.md) … PR #70); macOS Keychain storage not verified (#26) | Keychain on a real Mac |
| **#6 M5** 安定化と配布 | open | Exit **partly** met: new install → browser save → PDF annotation → MCP is reproducible by `tests/e2e/release` on Linux (passed) and, without a browser, on the macOS VM; not on a real Mac, not with a signed build | all blockers below |
| #30 recovery/security/10k | open (PR #77 merged) | recovery, hostile-input and layout regressions met; benchmark partial ([ISSUE_30](ISSUE_30.md)) | 16 GB reference machine; PDF-view RSS above target; frame time unmeasured |
| #31 `.app`/`.dmg`/signing | open (PR #79) | partial ([ISSUE_31](ISSUE_31.md)) | Developer ID, notarization, real Mac |
| #32 MVP acceptance | open (this branch) | traceability table and release E2E delivered; MVP completion **not** declared | everything listed here; independent different-model review and CI on the PR |
| #47 headless benchmark, #50 SIGKILL recovery | closed | met ([BACKEND_BENCHMARK](BACKEND_BENCHMARK.md), [CRASH_RECOVERY](CRASH_RECOVERY.md)) | — |

## Blockers to declaring the MVP complete

1. **Real-Mac verification** (owner or a native-CUA agent, per the [#7 table](TEST_MATRIX.md)):
   install from the `.dmg` by Finder, first launch with quarantine/Gatekeeper, relaunch, the GUI on
   Metal (viewer, selection, annotations, ⌘ shortcuts, Japanese IME, NSSavePanel export), browser
   capture with the real extension, Keychain token storage, sleep/logout, concurrent first start.
2. **Developer ID signing and notarization** (#31): no certificate or notary credentials exist;
   `release_sign.py` reports `UNVERIFIED: no Developer ID`; the signed path has never run.
3. **Store-published extension IDs** (#24): Chrome Web Store / AMO listings; until then only the
   development IDs exist and `npm run build:release` refuses to build.
4. **16 GB Apple Silicon measurement** (#30, DESIGN §16.1), GUI frame time, and the PDF-view RSS
   that is above target on Linux (409.7 MiB vs. 350 MB).
5. **Project license** (PR #80, MIT chosen but not merged; `docs/LICENSES.md` and the bundled
   notices still say "not yet chosen").
6. **#15 native gates** listed in its README: offline DOI fallback, failed-page retry, native
   queued-late-arrival.
7. **Live DOI providers**: no lane has ever resolved a DOI against Crossref/DataCite.
8. **Merges**: PR #78 (#34) and PR #79 (#31) are not merged; this branch stacks on them.
9. **Process**: CI on the #32 PR and the independent different-model review required by
   CONTRIBUTING have not run for this branch.

## Product defects found by the release E2E

Fixed here (with tests):

- `scripts/install_linux.py` did not install `papers` and `papers-mcp`; both start the sibling
  `papersd`, so an installed prefix had no CLI or MCP server (the README told users to run them
  from the build tree). Now installed beside `papersd` (`test_install_linux.py`). It can also
  keep the verified PDFium archive (`--pdfium-dir`), so a reinstall needs no network.
- The paper table footer and the offline status said "1 papers"; now "1 paper"
  (`Library::page_summary`, `papers_count`, `workbench_state.rs`).

Recorded, not fixed (larger or design questions):

- No app or CLI command for the storage layer's verified portable backup/restore; the documented
  route is copying a stopped library (INSTALL §6), which needs the daemon to be idle first.
- `papers server stop` is not available on macOS (RECOVERY; exit 7 in the macOS run), so scripts
  there wait for the idle exit.
- When the Keychain cannot be used (macOS VM, temporary `HOME`), `papersd token issue` blocks for
  about 20 s before falling back to the 0600 file; worth a shorter timeout or an explicit
  "no keychain" path, to be checked on a real Mac.
- **The GUI's `--settings` is ignored when `papersd` is already running.** The settings file is a
  `papersd` argument that only the GUI passes when it starts the daemon itself
  (`crates/daemon/src/bin/citation-manager.rs` → `DaemonConfig::settings`, default
  `<library>/ui-settings.json` in `desktop_worker.rs`). If the CLI, the browser host or an MCP
  client started the daemon first, the GUI silently loads and saves `<library>/ui-settings.json`
  instead of its `--settings` file, so preferences seem to reset or diverge depending on which
  client came first. Reproduced on Linux (release build, Xvfb): GUI first → palette "Toggle Vim
  Keys" → written to `--settings`; `papers server start` first → same action → written to
  `<library>/ui-settings.json`, `--settings` untouched. Affects the Linux desktop entry (always
  passes `--settings`) and any explicit `--settings`; a Finder launch on macOS uses the library
  default either way. A fix needs the settings location to travel with the session (or be a
  per-library setting) and touches the IPC contract and the security ADR (a daemon writing to a
  client-named path), so it is left for a separate issue.
- The GUI writes its settings only when something changes (e.g. the palette history), so a plain
  start/quit cannot show settings persistence.
