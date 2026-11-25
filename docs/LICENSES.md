# Licenses, notices and signing status of the macOS distribution

What the app bundle redistributes, where its notices are, and what has (not) been verified
about signing. Installation: [INSTALL](INSTALL.md).

## What is bundled

| Component | Where in `Citation Manager.app/Contents` | License | Notice shipped |
|---|---|---|---|
| Citation Manager executables (`citation-manager`, `papersd`, `papers`, `papers-mcp`, `papers-native-host`) | `MacOS/` | MIT ([LICENSE](../LICENSE)) | `Resources/licenses/LICENSE` |
| Rust crates linked into those executables (≈420, incl. GPUI from Zed at a pinned revision, SQLite via rusqlite `bundled`, rustls, tokio, axum, rmcp, pdfium-render) | inside the executables | per crate; MIT/Apache-2.0 dominate, also BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, Unlicense, CC0-1.0, BSL-1.0, MPL-2.0 (`option-ext`) | `Resources/licenses/THIRD_PARTY_NOTICES.md` (+ `third-party-licenses.json`) |
| PDFium chromium/8125 (bblanchon/pdfium-binaries, universal) | `Frameworks/libpdfium.dylib` | BSD-3-Clause, with bundled third-party code (FreeType, ICU, libjpeg-turbo, OpenJPEG, libpng, zlib, Little CMS, HarfBuzz, abseil, highway, simdutf, fast_float, dragonbox, AGG, llvm-libc) | `Resources/licenses/pdfium/` (LICENSE + all 18 upstream notices, hashes pinned in [pdfium-licenses/manifest.json](ADR/pdfium-licenses/manifest.json)) |
| App icon | `Resources/AppIcon.icns` | project asset | — |

The notices are generated at packaging time, never edited by hand:

- `scripts/package/third_party_notices.py` walks `cargo metadata` for the macOS target with
  the features the release uses and takes the **normal-dependency closure** of the shipped
  crates (dev/build-only dependencies are not redistributed). For every crate it records the
  SPDX expression and source and includes every `LICENSE*`/`COPYING*`/`NOTICE*`/`COPYRIGHT*`/
  `AUTHORS*` file of the crate package verbatim (identical texts once). It fails the build when
  a crate offers only GPL/LGPL/AGPL alternatives or declares no license.
- About 40 crates (mostly `objc2-*`, `block2`, `accesskit*`, `lyon*`, `taffy`) ship no license
  file in their crate package. They are listed by name with their SPDX expression; the
  standard texts of those licenses (MIT, Apache-2.0, Zlib) appear in the document through other
  crates. Obtaining the copyright lines from their upstream repositories is an open item
  before a public release.
- PDFium's notices are copied from the checksum-verified archive by `build_app.py`
  (`scripts/pdfium/fetch.py` fails on any notice drift).
- `option-ext` is MPL-2.0 (file-level copyleft, used unmodified). MPL-2.0 §3.2 asks a binary
  distribution to tell recipients where the source is; the notices list its crates.io source
  and repository. Confirming that this is sufficient is part of the legal review below.
- The wrapper `pdfium-render` (MIT OR Apache-2.0) ships only the selection note reproduced in
  [pdfium-render-LICENSE](ADR/pdfium-render-LICENSE.md), without a copyright line; the
  standard MIT/Apache-2.0 texts are in the document through other crates. Its copyright line
  is one of the open items below.
- SQLite is compiled in from `libsqlite3-sys`'s bundled source (public domain; the crate
  itself is MIT).

Fonts used by test fixtures (Noto Sans JP, OFL) and PDF validation tools (pypdf, ReportLab,
fontTools, Pillow) are development-only and not bundled.

The project itself is MIT-licensed ([LICENSE](../LICENSE), decided by the owner; #80).
Open legal items (not decided by engineering): review of the generated notices by the rights
holder, and the missing copyright lines above.

## Signing and notarization

| Step | Status | Evidence |
|---|---|---|
| Layout, linkage, PDFium load from the bundle | verified on hosted macOS 15 arm64 | [APP_LAYOUT_POC](validation/APP_LAYOUT_POC.md) |
| Ad-hoc signature (`codesign -s -`) + `codesign --verify --deep --strict` | verified on hosted runner | same |
| Hardened runtime with ad-hoc signature (PDFium, daemon, GUI, Firefox host) | verified on hosted runner; no Team ID involved | [ISSUE_31](validation/ISSUE_31.md) |
| `.dmg` (UDZO, `hdiutil verify`), run from the mounted image | verified on hosted runner | same |
| Third-party notices bundled | generated and checked on every packaging run | same |
| **Developer ID signature** | **UNVERIFIED: no Developer ID** (0 identities, #7) | — |
| **Notarization and stapling** | **UNVERIFIED: no Developer ID / no notary credentials** | — |
| Gatekeeper acceptance of a downloaded build | **UNVERIFIED** (requires the two rows above) | — |

`scripts/package/macos/release_sign.py` implements the procedure (below) and reports
`UNVERIFIED` instead of failing or pretending when no identity or credentials are present.

### Procedure (for whoever holds the Developer ID)

Secrets are read only from the environment or the login/CI Keychain, never from files in the
repository or command-line arguments, and are never printed.

1. Identity: a "Developer ID Application" certificate in the Keychain.
   `security find-identity -v -p codesigning` must list it. CI: import a `.p12` into a temporary
   keychain from the secrets `CM_SIGN_P12_BASE64` / `CM_SIGN_P12_PASSWORD` (see
   `.github/workflows/package-macos.yml`), and delete the keychain afterwards.
2. Notary credentials, one of:
   `xcrun notarytool store-credentials <profile>` once on the signing Mac, then
   `CM_NOTARY_KEYCHAIN_PROFILE=<profile>`; or an App Store Connect API key:
   `CM_NOTARY_KEY_PATH` (the `.p8`, outside the repository), `CM_NOTARY_KEY_ID`, `CM_NOTARY_ISSUER`.
3. Run the stages in this order (the PDF worker pins the digest of the signed PDFium copy at
   compile time, so the dylib is signed before the binaries are built):

   ```sh
   export CM_SIGN_IDENTITY="Developer ID Application: <Name> (<TEAMID>)"
   export CM_NOTARY_KEYCHAIN_PROFILE=<profile>
   python3 scripts/package/macos/release_sign.py preflight          # UNVERIFIED -> exit 3
   python3 scripts/pdfium/fetch.py target/pdfium
   digest="$(python3 scripts/package/macos/release_sign.py pdfium \
     target/pdfium/lib/libpdfium.dylib target/pdfium-signed/libpdfium.dylib)"
   CM_PDFIUM_SIGNED_SHA256="$digest" cargo build --release --locked -p cm-daemon --bins --features desktop-bootstrap
   CM_PDFIUM_SIGNED_SHA256="$digest" cargo build --release --locked -p cm-cli -p cm-mcp -p cm-native-host
   python3 scripts/package/third_party_notices.py --out target/notices
   python3 scripts/package/macos/build_app.py --bin-dir target/release --pdfium-dir target/pdfium \
     --out target/release-app --signed-pdfium target/pdfium-signed/libpdfium.dylib \
     --signed-pdfium-sha256 "$digest" --notices-dir target/notices
   python3 scripts/package/macos/release_sign.py app --app "target/release-app/Citation Manager.app" \
     --dmg target/Citation-Manager.dmg --status target/release-signing.json
   ```

   `app` checks that the bundled dylib carries the same Team ID, signs the helpers and the app
   with `--options runtime --timestamp`, verifies (`codesign --verify --deep --strict`),
   notarizes the app (zip, `notarytool submit --wait`) and staples it, builds and signs the
   `.dmg`, notarizes and staples it, and validates (`stapler validate`, `spctl -a -t open
   --context context:primary-signature`). In CI only the `release-sign` job of
   `package-macos.yml` runs these stages, and only when it is dispatched by hand on a `v*` tag.
   It requires approval in the GitHub environment `release`. Set the environment up as follows:
   - Add the signing secrets as **environment** secrets of `release`, not as repository secrets:
     `CM_SIGN_IDENTITY`, `CM_SIGN_P12_BASE64` and `CM_SIGN_P12_PASSWORD`, and optionally
     `CM_NOTARY_KEY_P8`, `CM_NOTARY_KEY_ID` and `CM_NOTARY_ISSUER`.
   - Require a reviewer.
   - Limit its deployment refs to protected `v*` tags.

   That job restores no build cache and fails, instead of publishing, when the environment has
   no Developer ID or Apple does not accept notarization. Branch and PR runs never see the
   secrets; they record `UNVERIFIED: no Developer ID` in `release-signing.json`. This signed path has **never run**
   (no identity), so treat its first execution as a test.
4. Re-run the layout smoke on the stapled build on a clean Mac: install from the `.dmg`, start
   with quarantine, PDF text/render, browser capture, MCP, backup/restore.

### Known blockers for a signed release

1. **PDFium trust pin (resolved in #34, untested with a Developer ID).** Signing rewrites the
   dylib, so the release job signs a copy first and builds with `CM_PDFIUM_SIGNED_SHA256`
   ([app layout](ADR/app-layout.md)); verified with an ad-hoc signature on the hosted runner.
   A Developer ID signature (with a timestamp) is not byte-reproducible, so the digest must
   come from the exact copy that is bundled; `build_app.py` refuses a mismatch.
2. **Library validation.** Under the hardened runtime a dylib must be signed by Apple or the
   same Team ID. A dylib re-signed with the same Developer ID satisfies it; keeping the
   upstream (ad-hoc) dylib would require the `com.apple.security.cs.disable-library-validation`
   entitlement. Measured with ad-hoc signatures only (no Team ID on either side): the hardened
   process loaded the ad-hoc dylib copy without the entitlement; the Developer ID case is
   unverified.
3. No Developer ID certificate and no notary credentials exist yet (#7: 0 identities).
4. Store-published browser extension IDs (#24) are needed for an end-user browser setup.
