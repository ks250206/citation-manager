# #31 macOS app/DMG, PDFium bundling, signing procedure: evidence

Builds on #34 ([app-layout ADR](../ADR/app-layout.md), [APP_LAYOUT_POC](APP_LAYOUT_POC.md)); the
layout is not re-decided here. Guides: [INSTALL](../INSTALL.md), [RECOVERY](../RECOVERY.md),
[LICENSES](../LICENSES.md). All runs are on **GitHub-hosted** `macos-15-arm64` runners
(macOS 15.8.2, Xcode 16.4); nothing here is real-hardware evidence, and **no Developer ID
signature or notarization was performed** (no certificate exists).

## Run

[45600278348](https://github.com/ks250206/citation-manager/actions/runs/45600278348) on `6fd9d07`:
all steps passed. Artifact `package-macos-evidence` holds per-phase `summary.json`/`smoke.log`,
`page.png`/`gui.png`, browser reports, `third-party-licenses.json`, `release-signing.json`, the
`.dmg` and the unsigned `.app` zip.

| Step | Result |
|---|---|
| Third-party notices | 423 crates (normal-dependency closure of `citation-manager`, `papersd`, `papers`, `papers-mcp`, `papers-native-host` for `aarch64-apple-darwin` with `desktop-bootstrap`), 255 distinct license texts, 43 crates without a license file (listed), no copyleft-only/undeclared license; bundled as `Resources/licenses/THIRD_PARTY_NOTICES.md` + JSON next to `licenses/pdfium/` (LICENSE + 18 notices). `crate_notices_bundled` is a required smoke check for every bundle |
| Unsigned bundle | same results as #34 (50 checks), now with the notices |
| Ad-hoc signed bundle | `codesign --verify --deep --strict` passes; signed PDFium copy (build-time pinned digest) loads, renders and extracts; lazy start and idle exit (42 checks) |
| `.dmg` | `Citation-Manager-0.1.0-arm64-adhoc.dmg`, 27 MB, `Format: UDZO`, `hdiutil verify` checksum VALID; contains the app and an `Applications` → `/Applications` link |
| Run from the mounted image (read-only) | `codesign --verify --deep --strict` of the app on the image passes; `papers-native-host register` **refuses**: "… is on a read-only volume (e.g. the mounted disk image); move Citation Manager.app to /Applications …" (exit 1); the CLI via PATH symlink lazily starts the image's `papersd`, PDFium text/render work, idle exit 8.7 s (44 checks) |
| Hardened runtime (ad-hoc) | all executables `flags=0x10002(adhoc,runtime)`, `Runtime Version=15.5.0`; dylib = the ad-hoc signed copy (no Team ID); `codesign --verify --deep --strict` passes; PDFium text and render from `papersd`, lazy start, idle exit 8.9 s; bundle moved, GUI started through LaunchServices with no arguments, window listed, manifest re-pointed, daemon exited 30.4 s after the GUI quit (50 checks); Firefox probe against the hardened bundle passed (hello paired, preview, commit, paper persisted, PDF job terminal) |
| Chrome / Firefox probes (unsigned moved bundle) | passed as in #34 (Chrome for Testing after branded Chrome ignored the unpacked extension; Firefox via the real per-user manifest directory) |
| Developer ID signing / notarization | `release_sign.py preflight`: **UNVERIFIED: no Developer ID** (`developer_id_identity`, `pdfium_signature`, `developer_id_signature`), notarization **UNVERIFIED: no notary credentials**; nothing signed; step summary says so |

## Hardened runtime and library validation (measured, ad-hoc only)

Under `--options runtime`, an ad-hoc signed process loaded the ad-hoc signed PDFium copy
without the `com.apple.security.cs.disable-library-validation` entitlement (the entitlement
file in `scripts/package/macos/entitlements/` was not needed and is not used). Ad-hoc code has
no Team ID, so this does **not** prove the Developer ID case: there, library validation requires
the dylib to carry the same Team ID, which `release_sign.py app` checks before signing
(the `pdfium` stage signs the copy with the same identity). Not verified: Developer ID +
hardened runtime + notarized + quarantined launch.

## Secrets handling

`release_sign.py` reads the identity name, keychain path and notary credentials only from
environment variables (from CI secrets or the signer's shell) or the Keychain (`notarytool`
profile); it never prints them and puts only names/paths on command lines. The workflow decodes
the `.p12` and `.p8` with `printf` (a shell builtin) into files under `$RUNNER_TEMP` with
`umask 077`, imports into a temporary keychain with a random password, and deletes keychain and
files in an `if: always()` step. A unit test checks that the workflow never echoes the secret
variables, never uses `set -x`, and keeps the UNVERIFIED path. No secret exists in the
repository; none was configured for these runs.

## Acceptance criteria

| #31 criterion | Status |
|---|---|
| .app/.dmg with binaries, resources, PDFium; ABI/rpath/launch/lazy start verified | **PARTIAL**: done on hosted runners (above and #34); a clean real Mac and Intel/universal binaries are not verified |
| third-party notices + PDFium license bundled; sign → notarize → staple procedure and evidence | **PARTIAL**: notices bundled and checked; procedure scripted and documented ([LICENSES](../LICENSES.md#procedure-for-whoever-holds-the-developer-id)); its evidence is UNVERIFIED (no Developer ID); the signed path has never run |
| Install/update/uninstall: extension + host, CLI PATH, MCP config, API opt-in, backup/restore | **DONE (docs)**: [INSTALL](../INSTALL.md), [RECOVERY](../RECOVERY.md); the CLI symlink, host registration/refresh, REST opt-in and run-from-image refusal are exercised by the smoke; MCP client config and backup/restore commands are documented but not run in CI; extension install depends on store IDs (#24) |
| Without certificates, signing/notarization reported unverified, never success; secrets via safe inputs | **DONE**: `UNVERIFIED: no Developer ID` in `release-signing.json` and the step summary |
| Inherit #34 layout ADR; track Developer ID/notarization blockers; re-verify PDFium/daemon/host under signed/hardened runtime | **PARTIAL**: layout unchanged; PDFium pin for signed bundles solved; hardened runtime re-verified with ad-hoc signatures only; Developer ID blockers listed in [LICENSES](../LICENSES.md#known-blockers-for-a-signed-release) |

## Not verified

Clean real Mac install from the `.dmg` (Finder drag, quarantine, Gatekeeper, App Translocation),
Developer ID signing, notarization, stapling, `spctl` acceptance, an Intel Mac, MCP client
connection from a desktop MCP client, backup/restore by the documented commands on macOS,
Keychain token storage from the GUI.
