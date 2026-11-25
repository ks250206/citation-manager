# #87 M6-4: design tokens, Light / Dark / System theme, density and icons — validation

## What changed

- `crates/desktop/src/ui/theme` (GPUI-free): colour tokens for the Light and Dark schemes
  (`Palette`, named by role: `canvas`, `surface`, `text`, `muted`, `accent_soft`, `danger`, …),
  spacing and font-size tokens per density (`Metrics`: row height, control and item padding,
  bar and field height, caption / small / body / title sizes, icon size), radius (`RADIUS`)
  and shadow (`shadow(scheme, Raised | Floating)`) tokens, WCAG contrast math and the list of
  every foreground/background pairing the native views draw (`PAIRINGS`, 43 entries).
- Light is the pre-#87 look: every former constant and hex literal in `native/` maps to a token
  with the same value. Two deliberate differences: the text-field border went from `#c9d1dc`
  (1.5:1) to `#828d9d` (≥ 3:1 against the field and the canvas, WCAG 1.4.11), and the
  placeholder text is a little darker (40 % instead of 20 % black).
- `native/` reads tokens through `tok()` / `dims()`. The scheme and density are resolved once
  per frame from the settings and the window appearance (`System` follows it, and the view
  re-renders when the OS appearance changes). No colour literal remains in `native/` except
  user-chosen annotation colours, which are content.
- Settings gain `theme` (`Light` / `Dark` / `System`, default `System`) and `density`
  (`Compact` / `Comfortable`, default `Comfortable`). They are part of the existing UI settings
  document (`cm_protocol::settings::Settings`, saved by the worker like `details_tab`), with
  serde defaults so files written before #87 load unchanged. Switch them in
  Settings › Organize & export › Appearance or with the palette commands **Switch Theme**
  (`appearance.cycle_theme`: Light → Dark → System) and **Toggle Density**
  (`appearance.toggle_density`). "Reset layout to defaults" keeps the theme and density.
- Icons: the emoji and symbol glyphs used as icons (📚 🗑 📁 📄 📝 ⚙ ＋ ↻ ⚠ ⏳ ✎ ● ✕ ↶ ↷ ⟳ − ☐ ☑ ✓ ▸ ▾ ▲ ▼ ⇤ ⇥ ←)
  are replaced with Lucide SVGs (lucide-static 0.460.0, ISC). Only the 28 icons in
  `crates/desktop/src/ui/icons.rs` are vendored, in `crates/desktop/assets/icons/lucide/` with
  the upstream `LICENSE`; they are compiled in and served to GPUI's `svg()` through an
  `AssetSource`, tinted with the token of the surrounding text. Keyboard glyphs in shortcut
  labels (⌘ ⇧ ↩ ↑ ↓) are text, not icons, and stay.
- Licenses: `docs/LICENSES.md` lists the icon set; `scripts/package/third_party_notices.py`
  has a `VENDORED` list and writes a "Vendored assets" section with the Lucide license into the
  `THIRD_PARTY_NOTICES.md` that `build_app.py` bundles, failing when a listed license file is
  missing.

No daemon, IPC or storage design change; no migration.

## Automated

| Area | Test |
|---|---|
| Contrast math against WCAG reference values (21:1, 1:1, `#767676` ≈ 4.54:1) | `cm-desktop` `theme_tokens::contrast_math_matches_wcag_reference_values` |
| Every used pairing meets AA in Light and Dark: 4.5:1 for text, 3:1 for UI indicators (focus ring, text-field border) | `theme_tokens::every_used_pairing_meets_wcag_aa_in_both_schemes` |
| Light keeps the pre-#87 values; Dark is dark | `theme_tokens::light_keeps_the_pre_87_look_and_dark_differs` |
| `System` follows the window appearance; theme / density parsing; palette cycle | `theme_tokens::system_mode_follows_the_window_appearance` |
| Comfortable = pre-#87 spacing, Compact is denser and still legible; radius and shadow ordering | `theme_tokens::compact_is_denser_than_comfortable_and_comfortable_is_the_old_spacing` |
| Old settings files load as System + Comfortable; chosen values round-trip; unknown names rejected | `theme_tokens::theme_and_density_are_saved_with_the_other_ui_settings` |
| The vendored directory equals `Icon::ALL` (nothing unused is vendored), every icon carries the Lucide license header and is served by the asset lookup | `theme_tokens::every_icon_is_vendored_once_and_nothing_unused_is_vendored` |
| `docs/LICENSES.md` names the icon set and its license file | `theme_tokens::third_party_license_record_names_the_icon_set` |
| Generated notices contain the Lucide ISC text; a missing vendored license fails | `scripts/test_package_release.py` |
| Header sort direction as an icon; PDF cell text | `library_list::cells_columns_and_header_sorting` |

Disabled-control text is not in `PAIRINGS`: WCAG 1.4.3 exempts inactive components.
Translucent overlays drawn over PDF pages (find hits, selection, page badge) are not measured,
because their backdrop is the document.

Packaging: `third_party_notices.py --out DIR` was run for the macOS target on Linux: 423 crates
and the new "Vendored assets (1)" section with the Lucide license were written. The
`.app` itself was not built (no macOS here); `build_app.py` copies that generated file
unchanged and its tests pass.

## In the running app (Linux, Xvfb, real PDFium)

Debug build of `cm-daemon --bins --features desktop-bootstrap`, run from a private prefix with
the pinned PDFium against the 36-paper library seeded for #84 (two PDFs on one paper, one note).
Theme and density were set either in the settings file before start, or by clicking in the app.
Driven with xdotool; every result was checked on the screenshot.

| Check | Result | Screenshot |
|---|---|---|
| Before (#84 build), Light | reference | [00](ISSUE_87/00-before-light.png) |
| Light + Comfortable: same colours, spacing and layout as before; icons instead of emoji in the top bar, sidebar, tags and status bar | pass | [01](ISSUE_87/01-light-comfortable.png) |
| Dark + Comfortable: whole window dark, text readable, selected nav item / tab / primary buttons visible | pass | [02](ISSUE_87/02-dark-comfortable.png) |
| Light + Compact: 26 rows visible instead of 19, smaller controls and text | pass | [03](ISSUE_87/03-light-compact.png) |
| Dark + Compact | pass | [04](ISSUE_87/04-dark-compact.png) |
| PDF pane in Dark: attachment icons, viewer toolbar (rotate, zoom −/+, undo/redo, ＋ Note) and the sticky-note marker on the page are SVG icons; the page stays white | pass | [05](ISSUE_87/05-dark-compact-pdf.png) |
| Settings › Appearance shows Theme Light/Dark/System and Density Compact/Comfortable; column toggles are checkbox icons | pass | [06](ISSUE_87/06-dark-compact-settings.png) |
| Clicking Light switched the whole window at once; the settings file then held `"theme":"Light"` | pass | [07](ISSUE_87/07-clicked-light-compact.png) |
| Palette "Switch Theme" (after clicking Comfortable) | pass | [08](ISSUE_87/08-palette-switch-theme.png) → [09](ISSUE_87/09-after-palette-dark.png) |
| Restart: Dark + Comfortable were loaded from the saved settings | pass | [10](ISSUE_87/10-restart-keeps-dark.png) |
| Switch Theme again → System: drawn Light under Xvfb (the X server reports no dark appearance) | pass (System → Light only) | [11](ISSUE_87/11-system-under-xvfb.png) |

## Not covered

- **System → Dark** was not seen in the app: Xvfb has no desktop appearance setting, so GPUI
  reports Light. The mapping is unit-tested (`resolve`), and the view re-renders on GPUI's
  appearance-change callback, but neither a live OS switch nor macOS dark mode was run.
- **macOS** was not run (icons, colours and the bundled notices on a real `.app`).
- Screens not opened under Xvfb in Dark: Import review merge panel, API & integrations, PDF
  search hit highlighting, the native file dialogs. They use the same tokens, and their
  text/background pairings are in the contrast test.
- Contrast of the translucent overlays over PDF pages (see above) and of user-chosen annotation
  colours is not checked.
