# #34: `Citation Manager.app` layout, resource resolution and process lifetime

Status: **adopted** (finalizes the #7 draft). Verified on GitHub-hosted macOS 15 arm64 runners
([APP_LAYOUT_POC](../validation/APP_LAYOUT_POC.md)); the real-hardware rows of that document
are still open. Signing, notarization and the `.dmg` are #31 and must not re-decide anything
below. Built by [`scripts/package/macos/build_app.py`](../../scripts/package/macos/build_app.py).

## Layout

```text
Citation Manager.app/Contents/
  Info.plist                     CFBundleExecutable=citation-manager, CFBundleIdentifier=dev.citation-manager.desktop
  PkgInfo                        LSMinimumSystemVersion=13.0 (the PDFium dylib's minos)
  MacOS/citation-manager         GPUI app (cm-daemon bin, desktop-bootstrap feature)
  MacOS/papersd                  per-library daemon (#25), started on demand by every client
  MacOS/papers                   CLI (#28), also `papers mcp`
  MacOS/papers-mcp               MCP stdio server and its grant bootstrap (#29)
  MacOS/papers-native-host       browser Native Messaging host (#23)
  Frameworks/libpdfium.dylib     chromium/8125 universal, upstream bytes, SHA-256 pinned (#11)
  Resources/AppIcon.icns         rendered from assets/icon.svg at build time
  Resources/licenses/pdfium/     PDFium LICENSE + all 18 upstream third-party notices
  Resources/native-messaging/extension-ids.json
  Resources/pdfium-runtime.json  build / archive / library digests of the bundled PDFium
```

- **Helpers live in `Contents/MacOS/`, next to the main executable.** Every client derives
  `papersd` from its own location; keeping them siblings makes the GUI, CLI, MCP server and
  browser host share one rule and one daemon binary. Apple's bundle guidance allows helper
  tools in `Contents/MacOS`; a `Contents/Helpers` split would only add a second rule.
- **PDFium is not linked.** No Mach-O in the bundle has a load command for PDFium, an
  `LC_RPATH` or an `@rpath`/`@loader_path`/`@executable_path` dependency; `otool -L` lists only
  `/usr/lib` and `/System/Library` libraries (checked by the smoke). The PDF worker `dlopen`s
  the absolute `Contents/Frameworks/libpdfium.dylib` after checking its SHA-256, so the
  dylib's own install name (`./libpdfium.dylib`, upstream) is irrelevant and is not rewritten
  (rewriting it would change the pinned hash). A future switch to linking would need
  `@rpath` + `LC_RPATH @executable_path/../Frameworks` and a new hash pin.
- Architecture: the hosted lane builds **arm64** binaries (the runner's native target). The
  PDFium dylib is universal (arm64 + x86_64). An Intel or universal build of the Rust
  binaries is a #31 release decision (`lipo` of two `--target` builds); nothing in the layout
  depends on it.

## Fixed resource resolution (no cwd, PATH, env or checkout)

| Resource | Rule | Code |
|---|---|---|
| own executable | `current_exe()` **canonicalized** (macOS returns the executed path, e.g. a PATH symlink) | `app_bundle::own_executable`, `papers`, `papers-mcp`, papersd's PDF runtime |
| `papersd` | `<dir of own executable>/papersd`, absolute, never a PATH search | GUI, `papers`, `papers-mcp`, native host |
| PDFium | `<exe>/../../Frameworks/libpdfium.dylib` when the executable is in `X.app/Contents/MacOS`; otherwise refused (`LibraryPath`) | `cm_pdf::worker::bundled_library` |
| PDFium trust | SHA-256 of the file equals the pinned original | `cm_pdf::worker::PDFIUM_SHA256` |
| default library (bundle launch only) | `~/Library/Application Support/Citation Manager/library` | `app_bundle::default_library` |
| browser host config | `~/Library/Application Support/Citation Manager/native-host/` | native host `config` |
| daemon socket | `$TMPDIR/citation-manager-<uid>/` (then `/tmp/…`), see #25 | `DaemonPaths` |

`CM_PDFIUM_LIBRARY` is read only by tests, examples and the PoC runner, never by a shipped
binary. A development build in `target/` keeps working through the same sibling rule (papersd
next to it) but has no PDFium unless a test passes one explicitly; the browser host refuses to
publish a `target/` executable.

## Launch

- **Finder / Dock / `open`**: LaunchServices passes no arguments. Inside a bundle, and only
  there, `citation-manager` then opens the per-user default library above (parents created
  0700; the library directory itself is created and checked by `papersd`). Outside a bundle
  `--library` stays mandatory (#10). An explicit `--library`/`--settings` always wins.
  A legacy `-psn_…` argument is ignored.
- Data never lives inside the bundle, so replacing or moving the `.app` never touches a
  library. The default location also avoids TCC-protected folders (Documents, Desktop,
  Downloads): `papersd` may be started by a browser or terminal, and macOS would attribute a
  privacy prompt to that responsible process. A library placed in such a folder works but
  may prompt (not verified on hardware).
- **No launchd agent.** Every client starts `papersd` directly (`posix_spawn` via
  `Command`, own process group, stdio closed) and the daemon outlives its starter: the
  hosted run shows it surviving the native host, Chrome and Firefox quitting, and the GUI
  quitting, then idling out. launchd would add an installation step, a plist with an absolute
  path that breaks on move, and a second lifetime owner; it is not needed for any requirement.
- Lifetime (#25, unchanged): the single-writer lock and socket are per library; the daemon
  stays while a client is connected, a job runs or the REST listener is enabled, and exits
  after the idle timeout (30 s default) otherwise.

## Browser host registration across moves and updates

`papers-native-host register` writes manifests whose `path` is the host's canonical absolute
location. Every bundle launch of `citation-manager` runs the sibling
`papers-native-host refresh` in the background (stdout closed, stderr inherited, reaped on a
thread): a no-op when never registered or already current, otherwise it rewrites the recorded
manifests to the new location. So after dragging the app elsewhere, or after an update that
replaces it at a new path, opening the app once repairs browser capture. Until the app is
opened again after a move, captures fail with the browser's "host not found".

Two locations are never published (register and refresh fail with an explanation):

- an **App Translocation** path (Gatekeeper runs a quarantined app that was not moved from
  a random read-only mount; the path disappears), and
- any **read-only volume**, e.g. running the app straight from the mounted `.dmg`.

With two copies of the app, the most recently launched copy owns the registration.
`papers-native-host unregister` before deleting the app removes the manifests (see #31's
INSTALL guide).

## CLI on PATH

`papers` / `papers-mcp` may be symlinked into a PATH directory
(`ln -s "/Applications/Citation Manager.app/Contents/MacOS/papers" ~/.local/bin/papers`).
They resolve the link before looking for `papersd`, so the bundle's daemon and PDFium are used.
A copy (not a link) outside the bundle has no sibling `papersd` and fails with exit 6.

## Signing order and hardened runtime (handoff to #31)

`scripts/package/macos/codesign.py` signs inside-out: the helper executables in
`Contents/MacOS`, then the bundle (main executable, Info.plist and Resources sealed). The
PDFium dylib keeps its upstream bytes and upstream ad-hoc signature, because the loader only
accepts the pinned original hash. Consequences recorded for #31:

1. Notarization requires every Mach-O, including the dylib, to carry a Developer ID signature
   with a secure timestamp. Re-signing the dylib changes its bytes, so the current
   original-hash gate rejects it. #31 must replace the gate for signed builds (e.g. pin the
   post-sign digest produced by the release job, or verify the code-signing identity) before a
   notarized bundle can load PDFium.
2. Hardened runtime enables library validation: a dylib must be signed by Apple or the same
   Team ID as the process. With the dylib re-signed by the same Developer ID this holds
   without the `com.apple.security.cs.disable-library-validation` entitlement; keeping the
   upstream signature would need that entitlement, which notarization accepts but weakens the
   process. The hosted run measures the ad-hoc behaviour (see the validation document).
3. Developer ID identities on the initial environment: 0 (#7). Nothing here is a
   distribution signature; an ad-hoc signature only makes `codesign --verify` meaningful.

## Development and history

- #10's throw-away smoke `.app` (`scripts/prepare_native_smoke.py`) is unrelated to this
  product layout. #25 added the sibling lazy start, #23 the host's refresh, #11 the dylib
  hash and path rule; this ADR adopts them for the bundle.
- The #7 draft's open items are resolved as above: Frameworks placement, no install_name
  change, absolute dlopen, sibling start without launchd, refresh on every app launch.
  Remaining real-hardware checks are listed in the validation document.
