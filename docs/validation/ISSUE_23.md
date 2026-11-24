# #23 Native Messaging host: verification evidence

Environment: Linux x86_64 (Ubuntu 24.04 container), Xvfb `:99`, debug builds, temp HOME /
library / runtime directory per run. No user library or browser profile was touched.

## Automated (cargo)

| Command | Result |
|---|---|
| `cargo test -p cm-native-host` | unit: 25 passed (framing/native endian/size limits, argv caller parsing, allowlist, message contract and error codes, manifests, config permissions, idle/oversized/response-limit handling) |
| `cargo test -p cm-native-host --test host_process` | 11 passed (6 initial + 5 review regressions: grants never orphaned by `pair`/`unregister` failures and retried from `revoke_pending`, `--keep-grant` keeps the credential without daemon contact, corrupt config fails clearly, non-UTF-8 host path rejected, no empty-allowlist manifest and `refresh` does not recreate deleted profiles; Firefox launches through an unregistered manifest path are refused). Initial 6: the real executable driven like a browser against a protocol-level fake daemon (Chrome and Firefox argv, rejected callers exit 3 with no stdout, oversized/invalid frames, not paired / unauthorized / daemon unavailable, lazy start of the sibling `papersd` with the host exiting first, `register` → move → `status` stale → `refresh` → `unregister`) |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | passed (installer layout and registration command, dev extension ID derived from the committed key) |

## Real browsers against the real #25 `papersd`

`scripts/native-host/browser_smoke.py` (binaries copied into a temp prefix first):

```sh
cargo build -p cm-daemon --bin papersd && cargo build -p cm-native-host --bins --examples
DISPLAY=:99 python3 scripts/native-host/browser_smoke.py --bin-dir target/debug \
  --browser chromium --executable /opt/pw-browsers/chromium-1243/chrome-linux64/chrome
DISPLAY=:99 python3 scripts/native-host/browser_smoke.py --bin-dir target/debug \
  --browser firefox --executable <firefox-153.8.0esr>/firefox
```

Both runs (Playwright's Chromium build 1243, launched directly with `--load-extension`; Mozilla
Firefox 153.8.0esr tarball, headless, unsigned test add-on sideloaded into a temp profile)
passed every check:

- `register --pair` wrote the manifest with `path` = the installed absolute host in the temp
  prefix and the pinned development ID; `status` healthy; the grant holds only
  `DestinationRead, CapturePreview, CaptureCommit`.
- The `papersd` started by pairing idled out before the browser was launched (no daemon running).
- Extension flow: `hello` (paired, protocol 1) and `capture.preview` via `sendNativeMessage`
  (offline HTML fixture, Highwire metadata, DOI, PDF candidate); a `Merge` commit was
  `denied` by the daemon grant; `destinations` + `capture.commit` (Create, PDF index 0) via a
  `connectNative` port; `v: 99` → `unsupported_version`; `grants.issue` → `unknown_type`.
- No host process remained after the requests; the browser was then terminated.
- After the browser quit (GUI never started), `papersd` was still running for the queued
  `capture.pdf_fetch` job. An owner-credential inspection found the paper persisted (title, DOI,
  year) and the job terminal: `failed`, 4 attempts, "attempt limit 4 reached: network error"
  (the fixture PDF host is unreachable offline; the paper was not rolled back).
- Chromium's captured host stderr contained only diagnostics such as
  `papers-native-host: capture.preview: ok (1130 bytes in)`; no page title, URL or DOI.
  Firefox sends host stderr to its browser console, which the headless run did not capture.

## Not verified

- macOS (manifest paths, `.app` placement and launch, #34 refresh hook on app start, signing #31).
- Windows (not supported: no registry registration).
- Branded Google Chrome, Chrome Web Store / AMO-signed extensions, Firefox release channel.
- A successful public PDF download through the host flow (no outbound network for the daemon
  here); #22 covers the fetch itself with a loopback fixture.
- The real #24 extension UI.
