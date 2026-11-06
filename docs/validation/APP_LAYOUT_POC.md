# #34 macOS app layout PoC — partial, browser gate blocked

2026-11-06/07 JST, implementation agent, macOS 27.2 (26B123), Apple Silicon,
Rust 1.99.1, base main `42345ccf7f80c11d3b8a89e8ad1db5e0cafa881b`.
**This is not completed #34 acceptance. Keep the PR draft and #23/#25 gates closed.**
[Layout/lifecycle decisions and limits](../ADR/app-layout.md) are the ADR.

| Required evidence | Observed result |
|---|---|
| Real #10 executable inside external .app | Passed: actual GPUI window, synthetic Unicode title saved through CUA |
| Actual app executable loads fixed #11 PDFium | Passed: original SHA-256, 8125 bindings, 360×440 raster, 50 characters including Japanese, fixture hash unchanged |
| Spaces/Unicode + move/rename + relaunch | Passed: old bundle absent, actual app resource/helper paths point at new Contents; GPUI window and PDF startup run repeated |
| Missing/altered runtime/resources | Passed: 6 actual executable failures, before GPUI/library output; no development env fallback |
| Absolute Chromium/Firefox manifests | Generated old/new temp registrations; removed them; this does not prove browser discovery |
| Chromium actual connectNative/lifetime | **Failed/unverified**: extra Seatbelt sandbox prevented browser subprocess sandbox initialization; no callback acknowledgement |
| Firefox actual connectNative/lifetime | **Failed/unverified**: official Firefox acquired, but extra sandbox prevented usable startup/Marionette; no callback acknowledgement |
| Direct-spawn fixture concurrency/EOF/death/idle/restart | Passed supplemental harness; explicitly `browser_tested=false` |
| Developer ID / hardened runtime / notarization | **Blocked**: 0 valid identities; strict/deep signatures fail; not attempted as shipping verification |

## Native evidence

The app is the real optional daemon composition executable, not a developer example.
`layout-smoke-io` resolves/checks bundled resources and invokes `cm-pdf-serial` before
starting the existing protocol-client GPUI UI. No source/PDF original is overwritten.

- [Before-move PDF/resource report](APP_LAYOUT_POC/native-before.json),
  [moved report](APP_LAYOUT_POC/native-moved.json), [native CUA observations](APP_LAYOUT_POC/native-cua.json).
- [Move and separate old/new registrations](APP_LAYOUT_POC/relocation.json).
- [Actual executable negative cases](APP_LAYOUT_POC/negative-cases.json).
- [Mach-O commands and signatures](APP_LAYOUT_POC/macho.json), [bundle hashes](APP_LAYOUT_POC/bundle.json).

CUA observed `0 papers loaded · offline`, then Cmd+N → paste synthetic
`Layout fixture 日本語` → Cmd+S → **Saved locally**. Read-only inspection of only
that synthetic DB confirmed the title and integrity_check=ok. After moving the
whole app to `/private/tmp/cm34 移設後 外部/Moved Citation 日本語.app`, CUA observed
the real window again and the app wrote a second PDF/resource report using the
new absolute paths. Cmd+Q ended both runs; final CUA app inventory had no matching
bundle ID. No browser preview was used for this native UI check. The raster is a
startup fixture artifact, not an integrated viewer screenshot or a full PDF viewer.

Initial findings were fixed and rerun: `/tmp` alias rejected by #8's library path
policy → canonicalize the explicit test output; macOS accepted sockets inherited
O_NONBLOCK → explicitly restore blocking reads, preserving connected-client lifetime;
Clippy required `as_chunks::<4>()` for raster iteration. No security/path validator
was weakened. A temporary full lock regeneration selected unrelated cached versions;
it was discarded, and Cargo regenerated only the two new daemon dependency edges
from the existing lock. All dependency package versions/checksums are preserved.

Original dylib hash remains
`8d1e46a6dbe32d7cb95279ab3def2e71e557a3d4cc49162453afa3e3b250dc5e`.
The actual app/helper executables are arm64; PDFium contains arm64+x86_64. The
install_name is still `./libpdfium.dylib`; explicit absolute dlopen worked without
editing it or an rpath. Complete dependencies/load commands are in the report.
Valid bundle loading/rendering succeeded; missing runtime and changed original
bytes failed closed. Strict signature verification failures are retained separately,
including the outer resource seal and dylib verification failure. No Developer ID,
TeamIdentifier, hardened runtime, notarization/stapling or x86_64 execution is claimed.

## Actual browser failures, isolation and closed gate

Sources and downloaded artifacts are pinned in [sources.json](APP_LAYOUT_POC/sources.json).
The machine's installed Chrome at `/Applications/Google Chrome.app` was left alone;
a separate official Chrome for Testing 157.0.8103.38 archive was used because it
supports a local fixture extension without changing the user's installed extensions.
Official Firefox 159.0.1 was downloaded into temp, its DMG SHA-256 checked against
Mozilla's release SHA256SUMS, copied into temp from a read-only mounted image, and
the image detached. No global installation was performed. No third-party extension
or web/network permission was installed; the fixture requests only nativeMessaging.

Pinned official source establishes that Chromium's host registrations follow
DIR_USER_DATA/NativeMessagingHosts, but Crashpad follows the default user-data
root independently of `--user-data-dir`. Firefox uses XREUserNativeManifests with
macOS FSFindFolder; `--profile` alone does not isolate it. We did not assign HOME,
modify default NativeMessagingHosts, edit actual profiles/preferences, or disable
extension signature verification. The intended Firefox harness uses Mozilla's
own test-only directory-provider pattern in its dedicated browser process, but
that stage was not reached and is **unverified**. It would not prove a production
installer's default-path registration.

We added an outer Seatbelt deny for writes to the actual user's Application Support
and Preferences while giving each trial its own profile/manifest/extension paths.
Both browser subprocess sandbox initializations failed with **Operation not
permitted**. Chromium's remote endpoint appeared but no extension/native-host
acknowledgement followed; Firefox did not reach usable Marionette. Logs:
[Chromium failure](APP_LAYOUT_POC/chromium-failed.log),
[Firefox failure](APP_LAYOUT_POC/firefox-failed.log), and the corresponding
[Chromium](APP_LAYOUT_POC/chromium-failed.json)/[Firefox](APP_LAYOUT_POC/firefox-failed.json) reports.

After the failed Chromium trial, an unexpected process from the downloaded CfT app
was observed without the explicit profile arguments. It and its downloaded-app
children were stopped by their exact test executable paths. The initial runner's
cleanup did not cover that relaunch; this is a **test isolation failure**, not a
passed clean browser run. The recorded default manifest snapshots match before/
after (published as counts/aggregate hashes to avoid exposing installed host names),
and Firefox profiles.ini/installs.ini remained absent. Those snapshots do **not**
prove absence of every possible default CfT profile/Crashpad/preference side effect.
We did not delete/restore any default-user file without a trustworthy before-image.
The user's installed browser sessions were not automated or deliberately changed.

Do not repeat the original interactive-user launch. Current `browser_probe.py`
refuses this OS account **before creating a profile or launching a process** and
requires a disposable macOS OS account named `cm34test-*` (or a VM with that account).
The additional sandbox remains enabled; browser sandbox/extension protections
are not bypassed. If it still prevents startup there, retain the failure and use a
properly isolated dedicated machine; do not remove protections to get a green test.
The separate account/VM is an external prerequisite not created by this work.

There is no verified actual Chromium or Firefox connectNative reply, host EOF/death,
browser quit, concurrent-client singleton, idle exit or lazy restart result.
**Neither browser gate passed; direct-spawn results cannot substitute.** Lifecycle
selection stays provisional, #23/#25 stay blocked, and #34 must not be closed by
merging this partial draft. The real host/daemon must repeat these tests with their
actual auth, single-writer/job and deployment paths after the gate is satisfied.

## Supplemental lifecycle fixture

[Direct harness report](APP_LAYOUT_POC/direct-harness.json) is explicitly classified
as `direct-spawn-only`, `browser_tested=false`. Two native-framed fixture clients
received one daemon PID, remained connected beyond the 2 s idle interval, survived
one host's SIGKILL through a replacement host attached to the same daemon, and
closed via stdin EOF. The daemon survived EOF briefly, exited on idle, and a later
request produced a different daemon PID. Invalid/truncated/oversized frames,
wrong nonce and command-shaped requests failed without exposing file/command APIs.
All fixture processes ended and the socket was removed. No launchd job was created.
This validates the fixture's process mechanics; it does not select a product startup
architecture or prove real browser cleanup behavior.

## Reproduction (native and supplemental checks)

Use a fresh explicit temp destination outside all checkouts. The test library is
always synthetic; do not pass a real library/profile/settings path. The commands
below create a new unsigned fixture, not an installed product:

```sh
cargo build -p cm-daemon --bin citation-manager --features layout-smoke --locked
rustc --edition 2024 -D warnings tests/fixtures/lifecycle-probe/probe.rs -o /private/tmp/cm34-probe-host
python3 scripts/pdfium/fetch.py /private/tmp/cm34-runtime-new
python3 scripts/package/layout-smoke/bundle.py \
  --out '/private/tmp/cm34 新規 外部' --runtime /private/tmp/cm34-runtime-new \
  --probe /private/tmp/cm34-probe-host
# Create a new private output dir and launch the actual app with arguments.
mkdir -m 700 '/private/tmp/cm34 新規 外部/native'
open -n '/private/tmp/cm34 新規 外部/Citation Layout 日本語.app' \
  --args --layout-smoke '/private/tmp/cm34 新規 外部/native'
# Use native CUA to observe the window and quit it before moving/testing resources.
python3 scripts/package/layout-smoke/check_failures.py \
  --bundle '/private/tmp/cm34 新規 外部/Citation Layout 日本語.app' \
  --out '/private/tmp/cm34 新規 外部/negative'
python3 scripts/package/layout-smoke/probe_harness.py \
  --bundle-meta '/private/tmp/cm34 新規 外部/bundle.json' \
  --out '/private/tmp/cm34 新規 外部/direct'
python3 scripts/package/layout-smoke/inspect_macho.py \
  --bundle '/private/tmp/cm34 新規 外部/Citation Layout 日本語.app' \
  --out '/private/tmp/cm34 新規 外部/macho.json'
```

For browser verification first provision the disposable OS account/VM externally;
the current account cannot satisfy the isolation preflight. Use pinned official
browsers under that account's temp directories and invoke `browser_probe.py` with
`chromium` or `firefox`, `--browser <absolute downloaded executable>`, `--bundle-meta`
and a new `--out`. Expect nonzero until actual callback-derived PID acknowledgements
and teardown assertions pass. Preserve failures as failures. No `--no-sandbox`,
unsigned-extension policy disablement, default-user registration or actual-library
fallback is an acceptable reproduction step.

## Checks and cleanup

Workspace fmt/locked Clippy/tests/build, native-feature Clippy/tests/build,
architecture, immutable migrations against the stated current main, Python tests,
standalone fixture compile/rustfmt and #11's existing fixed-PDFium runtime corpus
were run. [checks.json](APP_LAYOUT_POC/checks.json) records the final commands/results.
CI only compiles/tests the opt-in composition and standalone fixture; it does not
run GUI or browser acceptance. Remote CI and independent model review are separate
PR states owned by root; there is no self-review/merge in this implementation turn.

All active temporary host registrations were removed, browsers/test profiles and
extensions were cleaned after evidence collection, the downloaded browser image
was detached, fixture sockets/processes ended, and no launchd jobs were registered.
The static moved .app plus synthetic native artifacts remain in temp for inspection;
the private nonce directory is removed to disable stale helpers. The repo contains
only source and sanitized reports, no browser/PDFium binaries or real user data.
[cleanup.json](APP_LAYOUT_POC/cleanup.json) lists exact paths/results and the
unproven default-browser side-effect limitation. Rebuild a fresh bundle for reuse.

Full official Apache-2.0 wrapper license text and source/author attribution were
collected. Upstream explicit copyright notice confirmation remains pending along
with #31's complete shipping license audit; no invented legal claims were added.
