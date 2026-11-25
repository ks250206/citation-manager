# #24 Chrome/Firefox capture extension: verification evidence

Environment: Linux x86_64 container, Node 22.23, Xvfb `:99` (no window manager), debug Rust
builds, a throw-away HOME / config / library / runtime directory / browser profile per E2E
run. No user library or browser profile was touched. Binaries were built from this branch
after merging `origin/claude/issue-23-native-host` at bb3c31d.

## Automated

| Command | Result |
|---|---|
| `cd browser-extension && npm ci` | 20 packages from the committed lockfile (dev-only, pinned: typescript 5.9.3, esbuild 0.25.12, linkedom 0.18.12, @types/node 22.19.21) |
| `npm run build` | `dist/chrome` (MV3 service worker, dev `key`) and `dist/firefox` (MV3 `background.scripts`, gecko ID) from one source |
| `npm run lint` | `tsc --noEmit` (strict) passed; policy checks passed (exact permissions, no host permissions / content scripts / web-accessible resources / externally_connectable, no markup or dynamic-code sinks in `src/`) |
| `npm test` | 51 passed, 0 failed (`node --test`) |
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| `cargo test -p cm-native-host --locked` | 25 unit + 11 `host_process` passed |
| `python3 scripts/check_architecture.py` | passed (12 crates) |
| `python3 -m unittest discover -s scripts -p 'test_native_host.py'` / `'test_install_linux.py'` | 4 + 4 passed (extension-ids.json comment changed) |

Unit test coverage (`browser-extension/tests/unit`):

- Extractor regression on every `fixtures/html/*.html` from #22 (publisher, arXiv, PubMed,
  JSON-LD, Dublin Core, no-metadata, malicious): tags/JSON-LD forwarded, snapshot HTML free of
  `<meta>`/`<script>`/`<style>`, commented-out and script-written meta absent, command/path
  strings kept as data, each result accepted by the request validator. Bounds: a ~3.6 MB
  multi-byte page cut to ≤ 2 MiB without splitting a code point; oversized meta values and
  extra meta tags / JSON-LD blocks dropped whole; total budget shrinks the HTML; lone
  surrogates replaced; a PDF document becomes the explicit `pdf_url`.
- Boundaries: outgoing limits (HTML, title, URL, meta count/size, JSON-LD count/size, 4 MiB
  total, measured in UTF-8 bytes), non-http(s) page and PDF URLs, malformed destinations and
  decisions; popup-port sender checks (other extension, web page/content script, other
  extension page, framed popup, foreign origin); content script results (no activeTab URL,
  `file:`, sub-frame, navigated meanwhile, wrong types, foreign `pdf_url`); host reply
  envelopes (wrong id/version/type, non-object, malformed results, unsafe or too many PDF
  candidates, bad job state/IDs); requests that never reach the fake host (oversize, bad
  scheme, unknown op, Merge decision, out-of-range decision/PDF index, unknown preview);
  superseded and abandoned previews cancelled; native messaging failures mapped.
- Popup state machine and controller with a fake background: start/pairing/install/version
  errors, save → review defaults → commit → PDF polling limit and "Check again", PDF failure
  vs saved paper and retry, `pdf_error`, retryable commit retried on the same preview,
  conflict → re-preview without re-reading the page, repeated conflict with hidden matches →
  "already in your library", visible exact duplicate defaults to Skip, destination change
  re-previews, corrections become `citation_*` meta replacing the page tags, invalid year.
  Rendering (linkedom DOM): fields, PDF radios, decisions, hidden-duplicate notice, page and
  host strings inserted as text (no element injection), pairing / retry / PDF-retry actions.
- Manifests: MV3 both, exact permissions, development key derives Chrome ID
  `baapgkfjfjjagdkpkmocjhfopmaohcdc` (same key as the #23 smoke extension), gecko ID, release
  builds refuse empty store IDs.

## GUI-less end-to-end (real browsers, real host and daemon)

`browser-extension/tests/e2e/run.mjs` (see its header and the extension README):

```sh
cargo build -p cm-daemon --bin papersd -p cm-native-host --bins --examples
cd browser-extension && npm ci && npm run build
DISPLAY=:99 node tests/e2e/run.mjs --browser chromium --bin-dir <dir with the binaries>
DISPLAY=:99 node tests/e2e/run.mjs --browser firefox --bin-dir <dir> \
    --executable <firefox-153.8.0esr>/firefox
```

Browsers: Playwright's Chromium build 1243 (`/opt/pw-browsers/chromium-1243`, launched with
`--load-extension`/`--disable-extensions-except`, host manifest in
`<user-data-dir>/NativeMessagingHosts/`); Mozilla Firefox 153.8.0esr Linux tarball (SHA-256
checked against Mozilla's `SHA256SUMS`), headed on Xvfb, unsigned build sideloaded into the
temp profile, driven over WebDriver BiDi.

Both runs passed all 29 checks:

1. `papers-native-host register --pair --development-extensions` into the temp profile; grant
   `{DestinationRead, CapturePreview, CaptureCommit}`. The `papersd` started for pairing
   idled out before the browser was launched: **no daemon and no GUI running**.
2. Negative control before any user gesture: the popup document cannot see the fixture tab's
   URL, and **Save** fails with `no_access` (no activeTab grant, no host permissions).
3. The extension shortcut (Alt+Shift+S) is pressed on the article tab, which grants activeTab;
   the popup document then saves that tab (`?tab=<id>`).
4. Article (`tests/e2e/fixtures/article.html` on `http://127.0.0.1`, offline DOI lookup):
   preview shows title, "Ada Lovelace; Grace Hopper", 2026, DOI `10.9999/e2e24.capture`, the
   daemon's Highwire PDF candidate selected, decision Create. Commit → "Saved" with the paper
   ID. The PDF host does not resolve offline: the popup showed the job pending, then
   **failed** ("attempt limit 4 reached: network error") while the paper stayed saved; **Retry
   PDF download** re-queued it ("queued (attempt 0 of 4)").
5. Same article again: the preview shows the hidden-duplicate notice (matches are redacted
   for the capture grant). Create → daemon `conflict` → **Preview again** → `conflict` again →
   reported as "already in your library"; nothing created; Back → Cancel.
6. Page without metadata: preview flagged provisional; corrections (title, two authors,
   year 2025) re-previewed and applied by the daemon (no longer provisional); committed.
7. No `papers-native-host` process remained; the browser was closed. `papersd` was still
   running (it keeps the retried PDF job alive) — it was started by the host, not by a GUI.
8. "Launch the app": an owner session on the library (the `inspect_capture` example, the
   same authenticated IPC path the desktop app uses, starting `papersd` if needed) found both
   papers persisted: the article with its DOI and year, and the provisional page with the
   corrected title and year 2025.

How the gesture was produced: in Chromium with `xdotool key alt+shift+s` (real X input). The
toolbar popup itself is not exposed to Playwright, so the popup document was opened in a tab
after the grant. In Firefox, X key input does not reach the window under Xvfb without a
window manager (verified: even Ctrl+T had no effect), so the same shortcut is delivered as a
trusted key event inside the browser window with `nsITextInputProcessor` (privileged BiDi,
`--remote-allow-system-access`); Firefox's own `_execute_action` handler opened the popup
panel and granted activeTab (checked through the extension's tab manager while debugging and
by the tab URL becoming visible).

## Acceptance (Issue #24)

| Criterion | Status |
|---|---|
| 1. Only the saved page is read, limited permissions (activeTab), browser differences in manifest/build | Done: activeTab/scripting/nativeMessaging/storage only; negative control in both browsers; two manifests from one source |
| 2. DOI/authors/year/URL/PDF candidates, destination/tags and duplicate preview; provisional registration and manual correction | Done (unit + E2E). Corrections travel as a new preview (contract has no edited-draft field) |
| 3. Origin and input limits on content→background→host; save vs PDF failure vs retry distinguished | Done (unit boundary tests; E2E PDF failure + retry, conflict/duplicate) |
| 4. Chrome and Firefox: GUI/daemon stopped → save → visible after launching the app | Done for Playwright Chromium and Firefox ESR, with an owner IPC session standing in for the GUI |

## Not verified

- The desktop GUI itself after the save: the GPUI app was not built or started; persistence
  was checked through the same owner IPC path (`inspect_capture`).
- The real toolbar popup window in Chromium (Playwright cannot attach to it) and a real X
  key press in Firefox (see above); the popup document and the browsers' own shortcut →
  activeTab path were exercised instead.
- Branded Google Chrome, Chrome Web Store / AMO-signed builds and release IDs (placeholders),
  Firefox release channel, macOS and Windows.
- Online DOI complement (Crossref/DataCite) and a successful public PDF download: the
  sandbox has no outbound network for the daemon; E2E used `offline: true`.
- Authenticated pages, PDF tabs (`pdf_url` from a PDF document is unit-tested only), very
  large real publisher pages, and pages that block script injection (CSP/sandbox) in a real
  browser.
