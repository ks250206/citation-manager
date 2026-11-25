# #34 `.app` layout, PDFium load and lazy start: evidence

Decision record: [app-layout ADR](../ADR/app-layout.md). Everything below ran on
**GitHub-hosted runners** (`macos-15-arm64` image, macOS 15.8.2, Apple Silicon VM, Xcode 16.4,
Rust 1.99.1) through [`.github/workflows/package-macos.yml`](../../.github/workflows/package-macos.yml).
It is **not real-hardware evidence**: the real-Mac rows at the end stay open and gate #34 as
the #7 verification table requires. No Developer ID was used; ad-hoc signatures are not
distribution signatures.

## Runs

| Run | Commit | Result | What it showed |
|---|---|---|---|
| [45590229355](https://github.com/ks250206/citation-manager/actions/runs/45590229355) | `fab5c03` | failed | first build; static/CLI/PDFium/GUI passed; bundle signing refused the dylib's upstream linker signature; idle-exit check broken by its own polling; Firefox did not find the manifest under a temp `HOME`; browser smoke socket path too long |
| [45593512429](https://github.com/ks250206/citation-manager/actions/runs/45593512429) | `027a558` | failed | all smoke + ad-hoc signed bundle passed after the fixes below; branded Chrome 158 never ran the unpacked probe extension |
| [45596826266](https://github.com/ks250206/citation-manager/actions/runs/45596826266) | `e442fe1` | **passed** | every step green (details below; artifact `package-macos-evidence`: `summary.json`/`smoke.log` per phase, `page.png`, `gui.png`, browser reports, the unsigned `.app` zip) |

## Results (run 45596826266 unless noted)

Bundle: 67 MB, built by `scripts/package/macos/build_app.py`, copied with `ditto` to
`$RUNNER_TEMP/Install Root 検証/Citation Manager.app` (outside the checkout, spaces + non-ASCII).
Checks are in `scripts/package/layout-smoke/smoke.py`; each line is a `[PASS]` in `smoke.log`.

| Check | Result |
|---|---|
| Layout | `MacOS/{citation-manager,papersd,papers,papers-mcp,papers-native-host}`, `Frameworks/libpdfium.dylib`, `Resources/licenses/pdfium/` (LICENSE + 18 notices), Info.plist (`CFBundleExecutable=citation-manager`, `LSMinimumSystemVersion=13.0`), AppIcon |
| `lipo -info` | 5 executables: arm64 (thin); dylib: x86_64 + arm64 |
| `otool -L` | executables link only `/System/Library/...` frameworks, `/usr/lib/libSystem`, `libiconv` (GUI also AppKit/Metal/CoreText etc.); **no** `@rpath`/`@loader_path`, no PDFium link. Dylib: `./libpdfium.dylib` (its own id) + AppKit/CoreGraphics/CoreFoundation/Foundation/libSystem |
| `otool -l` | no `LC_RPATH` in any executable; executables `minos 11.0`; dylib `LC_ID_DYLIB ./libpdfium.dylib`, `minos 13.0` (both arches) |
| PDFium digest | unsigned bundle `8d1e46a6…dc5e` = upstream pin; signed bundle `033353a8…5e5f` = build-time pinned signed copy |
| `codesign` (unsigned) | executables `flags=0x20002(adhoc,linker-signed)`; bundle verify fails "code has no resources but signature indicates they must be present" (expected for an unsigned bundle); `spctl` rejects |
| `codesign` (ad-hoc signed) | `codesign --verify --deep --strict` passes (`flags=0x2(adhoc)`, TeamIdentifier not set); `spctl --assess` "rejected" (expected: no Developer ID, not notarized) |
| CLI via PATH symlink | `bin on PATH/papers -> …app/Contents/MacOS/papers`; `server status` → not running; `add file corpus.pdf --yes` lazily started the bundle's `papersd` (0.3–0.5 s); `ps` shows `…/Citation Manager.app/Contents/MacOS/papersd --library …` |
| PDFium text (bundled) | extraction `complete`, 6 pages, 5 with text, contains `Unicode 日本語 αβ café` — both the unsigned and the ad-hoc signed bundle |
| PDFium render (bundled) | REST opt-in (`papersd api set --enable`, scoped token) → `GET /attachments/{id}/pages/0?zoom=1`: 360×440 RGBA, 1702 ink pixels; saved as `page.png` (text and red marker visible) — both bundles |
| Idle exit | after the API was disabled and the last client left: `papersd` exited 8.7–8.8 s later (idle timeout 8 s) |
| GUI | bundle **moved** to `Moved Apps 移動先/`, started with `open -n` (LaunchServices, no arguments) and a temp `HOME`: window "Citation Manager — private local library" listed by System Events, screenshot `gui.png`, default library created under `<HOME>/Library/Application Support/Citation Manager/library`, its `papersd` started from the moved bundle |
| Re-registration on move | host registered at the old path before the move; launching the moved app rewrote the Chromium manifest `path` to the new location within the check's first poll |
| Daemon after GUI quit | GUI terminated; its `papersd` exited 30.4 s later (default 30 s idle) |
| Chrome (Chrome for Testing 158.0.8127.88) | probe extension: `hello` paired, `capture.preview`, `destinations`, `capture.commit` over a port, unsupported version / unknown type rejected, `Merge` denied by the grant; host exited after the requests; after the browser quit `papersd` was still running and drove the `capture.pdf_fetch` job to a terminal state (failed: the fixture PDF host is offline by design); paper persisted; host stderr had no page content |
| Firefox (release 160.0.1, geckodriver 0.37.1, temporary add-on) | same flow and checks as Chrome, all passed |

Lazy start from the browser host: before each browser launch the probe waited until the
`papersd` started for pairing had idled out (30.1–30.3 s), so the host started it on demand.

## Failures found and fixed (feedback to #11 / #23 / #25)

1. **A signed bundle cannot keep the upstream dylib bytes.** `codesign` refuses to seal the
   bundle while `Frameworks/libpdfium.dylib` only has its upstream linker signature ("code
   object is not signed at all"), and signing it changes its SHA-256, which #11's loader
   rejected. Fixed: the packaging job signs a copy first and builds with
   `CM_PDFIUM_SIGNED_SHA256`; `cm_pdf::worker::trusted_library_digest` accepts the upstream
   digest or that compile-time pin (compile-time format check, unit test). The ad-hoc signed
   bundle then loads, renders and extracts with the signed copy (runs 45593512429, 45596826266).
   [pdfium-runtime ADR](../ADR/pdfium-runtime.md) handoff updated.
2. **`current_exe()` is not canonical on macOS.** It returns the executed path, so `papers`
   linked onto PATH looked for `papersd` next to the link. `papers`, `papers-mcp`, papersd's PDF
   runtime and the GUI now canonicalize; subprocess test `papers_run_through_a_symlink_starts_its_own_papersd`
   (meaningful on the macOS CI lane) and the PATH-symlink check above.
3. **The app never refreshed browser manifests** (#23 left it to #34). The bundle launch now
   runs `papers-native-host refresh`; the host refuses App Translocation and read-only
   locations so a run from the disk image cannot hijack the registration (unit tests).
4. **A Finder launch has no arguments.** Inside a bundle the GUI now uses the per-user default
   library (unit tests in `cm_daemon::app_bundle`; GUI row above).
5. Test-harness only: the macOS per-user `TMPDIR` (`/var/folders/…`) plus a temp tree is too
   long for the 104-byte socket path (`socket path too long`); the probes use `/tmp`. A
   `server status` probe is itself a connection and restarts the idle timer, so idle exit is
   observed through the process, not by polling status. Firefox resolves its per-user
   Native Messaging directory from the real account, not `$HOME`, so its probe manifest is
   written to the runner user's `~/Library/Application Support/Mozilla/NativeMessagingHosts`
   and removed afterwards. Branded Chrome 158 ignores `--load-extension` (also with
   `--disable-features=DisableLoadExtensionCommandLineSwitch`): the probe could not run in it
   (run 45596826266, recorded as the first attempt) and used Chrome for Testing.

## Isolation and cleanup

Every phase used its own temporary `HOME`, `TMPDIR` (`/tmp/cm…`), library and runtime
directory; Chrome a temp `--user-data-dir` (manifest in `<profile>/NativeMessagingHosts`);
Firefox a geckodriver temp profile. Exception: the Firefox manifest in the runner user's real
`~/Library/Application Support/Mozilla/NativeMessagingHosts/` (absent before, deleted after the
probe; the step lists the directory before and after). Development extension IDs were used
only in these throw-away registrations. All daemons idled out or were terminated; the runner
VM is discarded.

## Verified only on the hosted runner / not verified

| Item | Status |
|---|---|
| Real Mac (Apple Silicon) install from Finder, first launch, relaunch | **not verified** (needs a hardware run per #7; hosted VM only) |
| Intel Mac / universal Rust binaries | **not verified**; only arm64 executables are built |
| Gatekeeper with quarantine (downloaded build), App Translocation behaviour | **not verified** (code refuses translocated paths; unit-tested only) |
| Branded Google Chrome with an installed (store) extension | **not verified** (branded Chrome ignores unpacked extensions from the command line; no store ID yet, #24) |
| Firefox with a signed AMO add-on, Firefox profile/HOME interplay on a real account | **not verified** (temporary add-on via geckodriver only) |
| Concurrent first start from GUI + browser + CLI on macOS | **not verified** on macOS (single-writer lock covered on Linux, #25) |
| Daemon lifetime while a job runs, across sleep/logout | partially: PDF job finished after host and browser quit; sleep/logout **not verified** |
| TCC prompts for libraries in Documents/Desktop/Downloads | **not verified** |
| Keychain secret store from a GUI-started daemon | **not verified** (the CLI run with a temp `HOME` used the 0600 file fallback) |
| `papers server stop` on macOS | refused (exit 7) until peer PIDs are verified there (cli ADR); unchanged |
| Developer ID signing, hardened runtime with a real Team ID, notarization | **not verified** (#31; 0 identities) |
