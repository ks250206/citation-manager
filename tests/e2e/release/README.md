# Release E2E (#32)

`run.py` installs a release build into a throw-away home and drives it through the MVP flow of
DESIGN §20 M5 / §24 in one go. It records named checks in `<out>/report.json` (plus
`logs/` with every command's output and `screenshots/` of the GUI) and exits non-zero if any
check fails. A step whose prerequisites are missing is reported as `skipped` with the reason;
it is never counted as passed. Results and where they were run:
[MVP_ACCEPTANCE](../../../docs/validation/MVP_ACCEPTANCE.md).

| Step | What it does | Checks (examples) |
|---|---|---|
| `install` | Linux: `scripts/install_linux.py` into `<work>/prefix` (pinned PDFium from `--pdfium-dir`, browser manifests for the dev extension when `--browser` is given, pairing). macOS: copies the `.app` (e.g. from the mounted `.dmg`) into `<work>/Applications` like a drag-install | all five executables installed, PDFium checksum = `runtime-lock.json`, notices, desktop entry with an explicit library |
| `privacy_defaults` | first `papers server start` on the new library | lazy start from the install, library 0700 / DB 0600 / owner credential 0600, `initial.sqlite` backup, HTTP API `enabled=false`, no TCP listener, default MCP grant read-only (12 tools, 4 read-only; a write tool is refused), no telemetry crate in `Cargo.lock` (heuristic) |
| `seed_with_cli` | `papers add file` (corpus PDF), `papers import` BibTeX and RIS | stored PDF byte-identical, extraction finished (`papers fulltext`) |
| `browser_save` (`--browser chromium`) | stops nothing by hand: waits until no `papersd` runs and checks no GUI runs, then runs `browser-extension/tests/e2e/run.mjs --installed` against the **installed** host, library and the profile the installer registered | the 26 extension checks (gesture → activeTab, preview, commit, PDF job failure/retry, duplicate, provisional + correction), then the papers are read back with the installed `papers` |
| `host_save` (no browser) | the same Native Messaging requests the extension sends (`hello`, `capture.preview`, `capture.commit`), framed to the installed host started the way a browser starts it | paired, saved, host exits when the port closes, paper persisted |
| `gui_restart` (`--gui`, Linux/X) | starts the installed GUI, searches with Ctrl+F, selects the hit, runs "Copy citation" from Ctrl+K, quits with Ctrl+Q, starts it again and repeats with another paper | window shown, daemon started by / connected to the GUI, clipboard names the paper saved from the browser (round 1) and the CLI paper (round 2), clean quit, daemon idles out |
| `search_everywhere` | enables REST (`papersd api set --enable`, tokens from `papersd token issue`), MCP over stdio (`papers-mcp serve`, `papers mcp`) | the same paper IDs from CLI, REST and MCP for a full-text, a browser-title and a Japanese query; page-jump target `paper://…?page=1`; 401 without token; foreign Host refused |
| `annotate_and_export` | REST note (read-only token refused first), MCP highlight (refused with the read-only grant, accepted after `papers-mcp grant --write`), owner confirms the AI annotations and saves the annotated copy (`release_e2e_owner`, the IPC requests behind the GUI's Confirm and Save PDF…), `papers export` BibTeX/RIS, REST disabled again | report complete with ≥1 highlight and ≥1 note, original SHA-256 unchanged, position error < 0.5 pt, pypdf (not PDFium) sees `/Highlight` and `/Text`, listener closed after disable |
| `portability` | re-imports both exports into fresh libraries; stops the daemon, copies the library (`cp -a`/`ditto`, INSTALL §6), restores the copy elsewhere | same titles after BibTeX and RIS round trip; restored copy serves the same papers, PDF, full text and annotations |
| `offline_doi` (`--offline`) | `papers add doi` with no network | explicit failure, library unchanged, and the namespace counters register the attempt (positive control) |
| `teardown` | revokes the MCP grant, unregisters the host, stops the daemon | no daemon or host left |

`--offline` (Linux) re-runs the script inside a new network namespace that has only `lo`. Every
step then records the namespace's `Ip:OutNoRoutes`/`Ip6OutNoRoutes` counters: an attempt to reach
any other host (DNS included) increments them. Each product step checks a delta of 0; the
`offline_doi` step must see a non-zero delta. The browser step's delta is recorded but not
checked: Chromium itself tries to reach Google services; the extension has no host
permissions and `connect-src 'none'` (checked by the extension's own lint/tests).

## Running it (Linux)

```sh
cargo build --release --locked -p cm-daemon --bins --features desktop-bootstrap
cargo build --release --locked -p cm-cli -p cm-mcp -p cm-native-host
cargo build --release --locked -p cm-native-host --example release_e2e_owner
python3 scripts/pdfium/fetch.py --platform linux-x64 target/pdfium-linux   # once, network
(cd browser-extension && npm ci && npm run build)                             # for --browser
Xvfb :99 -screen 0 1400x900x24 -nolisten tcp &                                # for --gui/--browser
DISPLAY=:99 python3 tests/e2e/release/run.py --bin-dir target/release \
    --pdfium-dir target/pdfium-linux --browser chromium --gui --offline --out target/release-e2e
```

The browser step needs Node 22 and Playwright with its Chromium (as for the #24 E2E); pass
`--browser chromium=/path/to/chrome` for another build. The GUI step uses `xdotool`, `xclip` and
ImageMagick `import` and assumes the 1400x900 screen above (no window manager). `--offline`
needs `unshare` (root, or unprivileged user namespaces). `--keep` keeps the work directory.

## macOS

`.github/workflows/package-macos.yml` runs the non-browser steps against the app copied out of
the ad-hoc signed `.dmg` (browser probes and the GUI launch after a move are separate steps of
that workflow). Locally:

```sh
python3 tests/e2e/release/run.py --app "/Volumes/Citation Manager/Citation Manager.app" \
    --owner-client target/release/examples/release_e2e_owner --out /tmp/release-e2e
```

## What it does not cover

Real Chrome/Firefox store builds and IDs, Firefox (no binary here; `run.mjs --browser firefox`
exists), the native toolbar popup (Playwright opens the popup document in a tab after the real
shortcut), the GUI's save dialog (the export uses the same IPC request), clicking Confirm in the
GUI, a live DOI lookup, Developer ID signing/notarization/Gatekeeper, a real Mac, and the
16 GB Apple Silicon performance targets (#30).
