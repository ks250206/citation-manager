# Real-Mac verification (#32 / PR #98 blockers)

Session of 2026-11-25. A Claude Code agent ran these checks on a real Mac, driving Finder and the
app through native input (computer use) together with the owner, who made the Gatekeeper
override themself. This covers the "real Mac" items that the hosted macOS VM could not show
([MVP_ACCEPTANCE](../MVP_ACCEPTANCE.md), blockers 1 and 4).

## Environment

| | |
|---|---|
| Machine | Mac16,6, Apple M4 Max, 14 cores, **36 GB** (not the 16 GB §16.1 reference machine) |
| OS | macOS 27.2 (26B123), Retina display, Japanese UI |
| Input sources | ABC, Kotoeri (Romaji), Google Japanese Input (used for the IME checks) |
| Source | `7e2878d` (tip of `claude/issue-32-acceptance`, includes #79 packaging and main up to #82) |
| Toolchain | rustc/cargo 1.99.1, Xcode Metal toolchain |
| Build | the `package-macos.yml` recipe run locally: `fetch.py` (PDFium chromium/8125), `codesign.py pdfium` → `CM_PDFIUM_SIGNED_SHA256=61046c42…ab79e`, release binaries, `build_app.py --signed-pdfium …`, `codesign.py app` (ad-hoc), `create_dmg.py` |
| Image | `Citation-Manager-0.1.0-arm64-adhoc.dmg`, 26,912,676 bytes, sha256 `f1d63044806aecdda2bfcd69cf08758ec46a785256a8fabe4729c8de9f3a3129`, `hdiutil verify` VALID |

Not available here: a Developer ID signature or notarization (on hold), a 16 GB machine, a
download through a real browser. To get the quarantine a browser download would add, the image
was copied to `~/Downloads` and given a `com.apple.quarantine` attribute
(`0083;…;Google Chrome;<uuid>`). It was not downloaded over the network.

## Results

| Area | Check | Result | Evidence |
|---|---|---|---|
| Install | Open the `.dmg` from Downloads → Finder window with the app and the `Applications` link; drag the app onto the link | **passed**: copied to `/Applications/Citation Manager.app` (68 MB); `codesign --verify --deep --strict` valid; the quarantine attribute propagated to the bundle and its executables | — |
| Gatekeeper | Double-click the quarantined ad-hoc build in Finder | **passed (expected block)**: the process is created suspended and the system dialog says the app "は開いていません … マルウェアが含まれていないことを検証できませんでした" with only **ゴミ箱に入れる** / **完了**. `spctl -a -t exec` → `rejected`; the `.dmg` → `rejected, source=no usable signature` | [screenshot](screenshots/gatekeeper-first-launch.png) |
| Gatekeeper override | Owner: System Settings → プライバシーとセキュリティ → **このまま開く** (Open Anyway), authenticated, then confirmed | **passed**: quarantine flags changed from `0183` to `01c3` (user-approved), the app started, and later launches open without a prompt. The Control-click → Open route was not tried | — |
| First start | Default library | **passed**: `~/Library/Application Support/Citation Manager/library` created 0700, `library.sqlite`, `backups/initial.sqlite`, `daemon/desktop.credential` and `papersd.lock` all 0600; `papersd` started from `Contents/MacOS` | — |
| Metal | Renderer of the GUI process | **passed**: the process maps `Metal.framework` and the Apple GPU driver (`AGXMetalG16X`), and a Metal shader cache exists under `…/C/dev.citation-manager.desktop/com.apple.metal/` | — |
| GUI | Library list, details, Japanese titles, relaunch on another library (`open -a … --args --library …`) | **passed** | — |
| PDF viewer | PDF tab of a CLI-added paper (`fixtures/pdf/corpus.pdf`): page 1/6 at 99 %, "Unicode 日本語 αβ café" rendered; Expand (reading mode) at 258 %; ‹ › paging to 20/20 on a 20-page PDF | **passed** | — |
| Text selection | Mouse drag over the first line, then the real ⌘C key | **passed**: clipboard `Unicode 日本語 αβ café` (19 characters, exact); status "Copied 19 characters from the PDF" | — |
| ⌘ shortcuts | ⌘K (palette, fuzzy "cop", ↩ runs **Copy citation** → clipboard `Corpus fixture 日本語 PDF.`), ⌘F (focus search), ⌘C (PDF), ⌘Q (GUI exits; `papersd` then idles out) | **passed** | — |
| Japanese IME | Google Japanese Input, real key events in the metadata search field: `nihongo` → marked text にほんご → Space converts to 日本語 → ↩ **only commits** (list unchanged, 3 papers) → second ↩ submits (1 paper) | **passed** | — |
| Japanese IME | Candidate window: `kanji` → Space ×2 → candidate list 1/40, ↓ moves to 3 幹事; Esc returns to かんじ, second Esc cancels the composition; the app's Esc/Back does not fire and the field keeps 日本語 | **passed** for key handling; **failed** for candidate placement (defect 1) | [screenshot](screenshots/ime-candidate-window.png) |
| Keychain | `papersd token issue --kind api` against a test library under the real login session | **passed**: 0.13 s (the hosted VM waited 20.2 s and fell back to the 0600 file); "Stored in: macOS Keychain"; generic password, service `citation-manager`, in `login.keychain-db`; SHA-256 of the shown secret = SHA-256 of the Keychain item; no fallback file; `token revoke` removes the Keychain item | — |
| Quit Apple Event | `tell application id "dev.citation-manager.desktop" to quit` (what logout and the Dock send) | **passed** in 5/5 launches | — |

### Measurements (36 GB M4 Max, not the reference machine)

Headless, `scripts/benchmarks/representative.py --out macos-real-m4max` (10,000 papers, 1,000
PDFs × 20 pages = 20,000 pages, the #30 method), raw JSON in [benchmark/](benchmark/):

| Metric | Measured | §16.2 target | Status |
|---|---|---|---|
| Warm start (p95 over 11) | 72.1 ms (107.6 ms) | about 1 s | met |
| Collection switch p95 | 2.0 ms | < 100 ms | met |
| Metadata search p95 | 41.9 ms | < 150 ms | met |
| Full-text search p95 | 16.7 ms (all terms, default scope 31.0 ms) | < 300 ms | met |
| papersd RSS: idle after warm start / peak indexing / peak queries | 13.5 / 46.7 / 22.5 MiB | — | — |

GUI, the installed app on that indexed 10,000-paper library. RSS is `ps` RSS of the GUI and of
the `papersd` it joined, sampled every 0.25 s
([gui-rss-samples.jsonl](benchmark/gui-rss-samples.jsonl), `t` in seconds since the sampler
started). The window was the default size on a Retina (2×) display:

| Phase | GUI | papersd | Total | §16.2 target |
|---|---|---|---|---|
| Idle, list of 10,000 loaded (t 30–52 s) | 111.8 MiB | 14.5 MiB | 126.2 MiB | GUI idle < 200 MB: **met** |
| One PDF open in the details pane, 100 % (t 53–61 s) | 289.9 MiB | 206.0 MiB | 495.9 MiB | PDF view < 350 MB: **not met** |
| Expand to reading mode (258 %), scroll, page 1 → 20 (peak) | 377.8 MiB | 452.5 MiB | 830.3 MiB peak | **not met** |
| Same, settled at page 20 | 377.8 MiB | 151.6 MiB | 529.4 MiB | **not met** |

Launch to first on-screen window (`open -a`, CGWindowList polled every 10 ms, `papersd` already
running, 5 runs): 438, 425, 395, 371, 345 ms.

The PDF-view numbers predate #85, which changes how page bitmaps cross the IPC boundary. They are
also not comparable with Linux's 409.7 MiB: that run used a 1400×900 Xvfb surface at 1×, while
this one used a Retina surface at 2× (4× the pixels per page) and the 258 % reading zoom. Re-measure
after #85 merges, with the same steps. GUI frame time while scrolling the 10,000-row list was
not measured: no frame-time instrument exists for the macOS GUI yet.

## Defects found

1. **The IME candidate window opens at the top-left corner of the screen, not at the text.**
   With Google Japanese Input in the search field, the candidate list showed at the screen's
   origin while the marked text was in the field ([screenshot](screenshots/ime-candidate-window.png)).
   Selecting candidates still worked. The likely cause is
   `TextInput::bounds_for_range` ([text_input.rs](../../../crates/desktop/src/native/text_input.rs)):
   it returns `None` whenever the cached `last_layout` text differs from the current content.
   macOS asks for the rectangle right after `setMarkedText`, before the next paint, so the
   layout is always one edit behind, the check fails, and AppKit falls back to the origin. A fix
   could return a rectangle from the stale layout, clamped to its length, or the caret position
   of the last paint, instead of `None`. Not fixed here, and not yet checked with Kotoeri.
2. **[INSTALL.md](../../INSTALL.md) §1 step 3 leads with a route that is unverified here.** It
   documents Control-click → Open → Open first and System Settings → Open Anyway as the
   alternative. On this macOS the Finder double-click dialog offers no "Open" button at all, only
   ゴミ箱に入れる / 完了. The route verified here is: try to open the app once, then System Settings →
   Privacy & Security → Open Anyway, authenticate, and confirm Open. Suggested wording: lead with that
   route, and keep Control-click → Open only as "on older macOS versions" unless someone verifies it
   on current macOS.
3. **PDF-view RSS is above target on macOS too** (495.9 MiB with one PDF open, 830.3 MiB peak
   while paging in reading mode). Tracked by #85.

## What this changes in the §24 table

| # | Criterion | Before | Real-Mac evidence added here | Still missing |
|---|---|---|---|---|
| 1 | Install, start, restart, data kept | partial | `.dmg` → Finder drag install, quarantined first launch blocked by Gatekeeper and started after the owner's approval, relaunch, ⌘Q | Developer ID + notarization; a real browser download; sleep/logout session; concurrent first start |
| 2 | 10,000 papers usable | partial | headless p95 all met on a real Mac; GUI idle 126 MiB total with 10,000 rows | the 16 GB machine; GUI frame time |
| 5 | PDF view, search, text selection | partial | Metal viewer, drag selection, ⌘C exact text, paging | in-PDF find and highlight creation on macOS were not exercised in this session |
| 11 | Palette and Vim keys | partial | ⌘K/⌘F/⌘C/⌘Q, Japanese IME composition and commit rules | Vim keys on macOS; IME candidate placement (defect 1) |
| 12/13 | REST/MCP tokens | passed | Keychain storage verified (#26 macOS item) | — |
| 17 | RSS / startup / search measured and published | partial | macOS numbers published here | the 16 GB machine; PDF view above target (#85); frame time |

## Not done in this session

Browser capture with the real extension on macOS, the NSSavePanel export, annotation creation
in the macOS GUI, sleep/logout with the app running, concurrent first start, Kotoeri (Apple's
IME), Vim keys, and the 16 GB machine.
