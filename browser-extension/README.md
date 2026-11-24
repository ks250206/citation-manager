# Citation Manager browser extension (#24)

Chrome/Chromium and Firefox WebExtension (Manifest V3 for both) that saves the paper on the
current page to the local library. It talks only to `papers-native-host` over Native
Messaging (#23, [ADR](../docs/ADR/native-messaging.md)); the host starts `papersd` on demand,
so saving works with the desktop app closed. Evidence: [ISSUE_24](../docs/validation/ISSUE_24.md).

## What it does

1. The user clicks the toolbar button (or presses **Alt+Shift+S**) on an article page and then
   **Save this page**. Only then is the extractor injected into that tab
   (`activeTab` + `scripting`; there are no host permissions or content scripts).
2. The extractor copies, as inert data: the page URL and title, `<meta name|property>` tags,
   `application/ld+json` blocks, and an HTML snapshot built in a document without a browsing
   context (no page script is called; `<script>`, `<style>`, `<meta>` and embedded objects
   are removed). Everything is bounded to the #22 limits on UTF-8 boundaries (HTML ≤ 2 MiB,
   ≤ 1024 meta tags, ≤ 32 JSON-LD blocks, 4 MiB total); oversized meta/JSON-LD values are
   dropped whole, never cut.
3. `capture.preview`: the popup shows the resolved title/authors/year/DOI/journal/URL, the
   daemon's PDF candidates (radio list, or no PDF), the destination collection and tags
   (from `destinations`), duplicate warnings and a per-item **Save as new / Skip** decision.
   Pages without metadata are flagged as provisional; the **Correct the metadata** form
   re-previews with the user's values (see below).
4. `capture.commit` (Create/Skip + PDF index), then the result: saved, and separately the PDF
   job state (polled with `capture.pdf_job`; **Check again**; **Retry PDF download** with
   `capture.retry_pdf` after a failure). Closing the popup cancels an uncommitted preview.

Errors: not paired / unauthorized → pairing instructions; host not installed → install
instructions; `daemon_unavailable` / `busy` → **Retry** (same request); `conflict` →
**Preview again**. The daemon answers an exact duplicate (same DOI/identifier) with
`conflict` as well and hides the match from the capture grant; a second conflict after
re-previewing is therefore reported as "already in your library" (merge in the app).

### Manual correction

The #22/#23 contract has no field for an edited draft: commit only accepts Create/Skip over
the daemon's own preview. Corrections (title, authors, year, DOI, journal) are sent as a new
`capture.preview` in which the user's values are `citation_*` meta tags placed before the
page's own tags, replacing the page's tags of the same field. The daemon validates and
normalizes them like page data. Other fields are edited in the app.

## Boundaries (DESIGN §12.3)

| Boundary | Check |
|---|---|
| page → content script | runs only after the user's action, top frame only, values copied as strings, bounded |
| content → background | the script's return value: top frame, http(s) tab URL visible through activeTab, same page URL, shapes, then the #22 limits |
| popup → background | only a `capture-popup` port from this extension's `popup.html` (`sender.id`, URL, frame 0, origin); no `runtime.onMessage`, no external messaging, no web-accessible resources |
| background → host | closed set of types, capture re-validated against the limits, decision/PDF indices checked against this session's own previews, 8 MiB frame check |
| host → background | v1 envelope (`v`, echoed `id`, `type`), result shapes and bounds; anything else is `bad_reply` |
| popup DOM | page and host strings only via `textContent`/attributes (`npm run lint` rejects markup sinks) |

## Build

Node 22+. Dependencies are dev-only and pinned (`typescript`, `esbuild`, `linkedom` for DOM
unit tests, `@types/node`); `package-lock.json` is committed.

```sh
cd browser-extension
npm ci
npm run build        # dist/chrome and dist/firefox (development IDs)
npm run lint         # tsc --noEmit + policy checks (permissions, no markup sinks)
npm test             # node:test unit tests (extractor fixtures, boundaries, popup)
```

`npm run build:release` uses the store IDs from `native-manifests/extension-ids.json`
(`release`). They are empty placeholders until the extension is published: fill
`release.chrome` with the Chrome Web Store ID and `release.firefox` with the AMO add-on ID;
the release build fails while they are empty. The installer passes them to
`papers-native-host register --chrome-extension-id/--firefox-extension-id`.

## Load unpacked (development)

The development build has fixed IDs that the native host allows with
`--development-extensions`: Chrome `baapgkfjfjjagdkpkmocjhfopmaohcdc` (from the public `key`
in the generated manifest) and Firefox `citation-manager-dev@citation-manager.invalid`.

- Chrome/Chromium: `chrome://extensions` → Developer mode → **Load unpacked** →
  `browser-extension/dist/chrome`.
- Firefox: `about:debugging#/runtime/this-firefox` → **Load Temporary Add-on** →
  `browser-extension/dist/firefox/manifest.json` (unsigned add-ons are temporary in release
  Firefox; ESR/Developer Edition can install them permanently with
  `xpinstall.signatures.required = false`).

## Register and pair the native host

Once per user and library (Linux shown; see the ADR for macOS paths):

```sh
papers-native-host register --library /abs/path/to/library \
    --browser chromium --browser firefox --development-extensions --pair
papers-native-host status
```

`--pair` asks the daemon for a capture-only grant (`CapturePreview + CaptureCommit +
DestinationRead`). The popup's first call, `hello`, never starts the daemon; if it reports
`paired: false` the popup shows these instructions. `scripts/install_linux.py` installs the
binaries and runs the registration.

## End-to-end test (Linux, X display)

```sh
cargo build -p cm-daemon --bin papersd -p cm-native-host --bins --examples
cd browser-extension && npm ci && npm run build
DISPLAY=:99 node tests/e2e/run.mjs --browser chromium --bin-dir ../target/debug
DISPLAY=:99 node tests/e2e/run.mjs --browser firefox --bin-dir ../target/debug \
    --executable /path/to/firefox-esr/firefox
```

Throw-away HOME/library/profile; pairs, waits until `papersd` has stopped, serves
`tests/e2e/fixtures/*.html` on 127.0.0.1, presses the shortcut (the user gesture that grants
activeTab), drives the popup (save, PDF failure and retry, duplicate, provisional save with
corrections), quits the browser and opens the library as its owner to check the papers.
Chromium needs Playwright's Chromium (not a dependency here; resolved from
`/opt/node-tools`); Firefox is driven over WebDriver BiDi with Node's built-in WebSocket.

## Files

- `src/content/` extractor, `src/background/` port, boundaries, host client and session,
  `src/popup/` state machine, controller and view, `src/shared/` limits, types, validation,
  request builder; `src/manifest.ts` generates both manifests.
- `public/` popup HTML/CSS and icons. `native-manifests/` extension IDs and the #23 smoke
  extension (not this extension).
