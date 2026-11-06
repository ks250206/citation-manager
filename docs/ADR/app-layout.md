# #34: executable-relative .app layout and lifecycle gate

Status: **bundle layout implemented and exercised; browser lifecycle gate BLOCKED**.
This is an unsigned M0 test artifact, not a shipping bundle. #23/#25 must not start
on the assumption that this issue's browser acceptance has passed. Measurements,
failed attempts, cleanup and reproducible commands are in [APP_LAYOUT_POC](../validation/APP_LAYOUT_POC.md).

## Measured layout

```text
Citation Layout 日本語.app/Contents/
  Info.plist
  MacOS/citation-manager            # real #10 GPUI executable, opt-in layout-smoke
  MacOS/cm34-probe-host             # test-only standalone fixture
  MacOS/cm34-probe-daemon           # same fixture, role selected by fixed executable name
  Frameworks/libpdfium.dylib        # exact original #11 universal 8125 bytes
  Resources/layout-fixture.pdf      # synthetic #11 Unicode corpus
  Resources/lifecycle-config.txt    # disposable nonce/private test path, never real library
  Resources/layout-integrity.json
  Resources/host-manifests/{chromium,firefox}.json.in
  Resources/licenses/               # PDFium + all archive notices, GPUI, corpus, wrapper
```

`cm-daemon/layout-smoke` is macOS-only through the existing optional
`desktop-bootstrap` composition edge, off by default. No 13th workspace crate,
framework dependency in a contract crate, or desktop-to-storage/PDF edge is added.
`citation-manager --library /absolute/dedicated/path` keeps #10's behavior.
`--layout-smoke /absolute/empty/private/output` is available only in this test feature.
Its `layout-smoke-io` thread performs bundle checks, starts #11's `cm-pdf-serial`
worker, writes a synthetic raster/report, then supplies a fresh synthetic library
path to the unchanged `LibraryClient`-based views. This is not a PDF viewer.

`current_exe()` determines Contents. #11's unchanged `bundled_library()` resolves
`Contents/Frameworks/libpdfium.dylib`; its original SHA-256 gate and required-symbol
binding remain unchanged. Resources/helpers are siblings under the same Contents.
The resource inventory requires the fixture, helper binaries, license/notice files
and manifest templates, verifies their hashes, rejects traversal and symlinks, and
fails before GUI/library creation on missing/altered resources. It is a consistency
check in a trusted disposable unsigned bundle, **not signed integrity**: an attacker
who can replace both inventory and content can replace that evidence. #31 owns
release trust, post-sign hashes, identity, entitlements and sealed resources.
Hash-read/dlopen TOCTOU is inherited from #11, not solved here.

There is no CWD/PATH/system-library/checkout discovery, implicit development env
fallback, installed Python dependency, runtime repo asset access, or default user
library. The native app's output path is canonicalized before passing it to #8:
macOS `/tmp` is a symlink and #8 correctly rejects non-canonical library paths.
The fixture nonce path is explicit test configuration packaged into Resources, not
an automatic resource search path. Removing that disposable directory disables
the leftover probe helpers. The product must not ship this feature/configuration.

## ABI, dependency and signing feedback to #11/#31

Actual app executable loaded the original universal dylib at its absolute bundle
path and rendered/extracted Japanese from the bundled fixture, before and after
moving/renaming the whole .app outside the checkout under spaces and Unicode.
No install_name_tool, thinning, signature patch or re-signing was performed.
`install_name` remains `./libpdfium.dylib`; absolute dlopen succeeds without rpath
lookup. Both dylib slices depend only on Apple frameworks/libSystem. The measured
app/helpers are arm64; x86_64 execution and a universal app build are unverified.
Mach-O load commands, dependencies, architectures and signature failures are
preserved in the evidence. No runtime loader failure was seen with the valid
bundle. Missing dylib and altered original bytes fail closed; load/link/signature
success are separate claims.

Developer ID identity count was rechecked: **0**. The linker/upstream ad-hoc Mach-O
signatures have no TeamIdentifier, and strict/deep app verification fails. This is
not hardened runtime, Developer ID signing, notarization, stapling or Gatekeeper
acceptance. #31 must sign nested code then the outer app, validate all architectures,
and resolve #11's fixed-original-hash versus post-sign-digest policy explicitly.
A signed dylib must never be silently patched to satisfy the old hash check.

The full official Apache-2.0 text is now packaged for `pdfium-render` 0.9.5, selecting
one of its offered licenses. The official published crate identifies Alastair Carey
as author but ships only a license selection statement, no separate NOTICE or
explicit wrapper copyright declaration located in that package. Author metadata
is preserved without inventing a copyright year/claim. Upstream rights-holder
notice confirmation and the complete product dependency/license audit remain #31.
All fixed PDFium archive notices are copied and verified against #11's manifest.

## Absolute host registrations and moving/updating

`register_fixture.py` serializes each browser's allowlist with an absolute bundled
host executable path. It only writes a new registration in an explicit temp
subdirectory and never overwrites a registration or installs into Application
Support. Moving the app makes the old host path stale. The measured relocation
record contains separate old/new Chromium and Firefox manifests; every temporary
registration was removed afterward. **Generation is not actual browser discovery.**

Downstream #23/#31 must track the installed bundle URL and owned registration paths.
Before moving/updating: stop active hosts/daemon, validate the replacement bundle,
regenerate the absolute host paths, atomically replace only this app's previously
owned registrations, then reconnect and check protocol/version. Never leave a
manifest pointing into a deleted app, rewrite unrelated hosts, or solve moves with
PATH/CWD lookup. In-place helper updates also require a daemon restart so the old
native library/process cannot continue serving the new version. Installer prompts,
rollback, credential migration and production registration are not implemented.

Pinned Chromium 157.0.8103.38 `chrome_paths.cc` resolves user NativeMessagingHosts
under DIR_USER_DATA: `--user-data-dir` isolates that registration lookup. It does
**not** isolate Crashpad's default-user-data path. Pinned Firefox 159.0.1
`NativeManifests.sys.mjs` uses XREUserNativeManifests/XRESysNativeManifests. On macOS
`nsXREDirProvider.cpp` derives the user root with FSFindFolder, independently of
`--profile`; neither a profile flag nor assigning HOME establishes isolation.
Mozilla's own test directory-provider pattern is implemented in the optional
browser harness, but was not reached/validated on this machine. It changes only
the dedicated browser process's test lookup; it would not prove product install
path selection. No default registration, signature-check preference or extension
security policy was changed by our scripts.

## Lifecycle candidate and closed gate

The standalone Rust fixture accepts only bounded native-endian JSON nonce pings
and acknowledgements of returned PIDs. There are no file paths, shell commands,
HTTP/REST endpoints, real user data, host grants or CRUD operations. It uses a
private nonce directory/socket, an OS-released file lock, and a fixed sibling
helper executable with empty environment, `/` CWD, null stdio and a new process
group. No launchd job is installed. Connected fixture clients prevent the 2 s
idle exit; EOF drops the connection, idle exit unlinks the socket, and a later
request restarts it. Concurrency, host death, EOF, idle exit/restart and malformed
frames passed the **direct-spawn supplemental harness**. macOS accepted sockets
inherit nonblocking mode; the fixture explicitly restores blocking mode, as the
active-past-idle regression demonstrated.

**Direct spawn is a candidate, not the final browser-validated selection.** Both
actual browser launch trials failed with the extra isolation Seatbelt sandbox;
the browsers could not initialize their own subprocess sandboxes. Disabling their
sandbox/extension protections is not an acceptable workaround. CfT also produced
an unexpected downloaded-browser process without the explicit profile arguments;
it was stopped, and absence of all possible default-profile side effects cannot
be asserted. The runner now refuses the interactive user's OS account and requires
a disposable macOS account named `cm34test-*` (or a VM with such an account).
Do not rerun the original trial on the user's account.

Neither browser produced a verified connectNative reply/ack, so browser EOF/death/
quit, one-instance behavior and lazy relaunch are **unverified for both**. #23/#25
gates remain closed; #34 acceptance is incomplete and its PR must stay draft.
A separate OS test account/VM and successful actual Chromium+Firefox evidence are
required before selecting direct spawn versus launchd. #25 still owns production
single-writer/startup races, jobs, auth/grants and failure recovery; #23 owns the
real host/capture flow. Those must revalidate the same browser and move conditions.
