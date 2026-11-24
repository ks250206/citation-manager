# #15 GUI usability pass — Linux preview evidence

**Scope of this evidence:** these screenshots come from a *local, uncommitted* Linux build
(Xvfb + Mesa lavapipe, GPUI `x11` backend) of the same `cm-desktop` native views, driven with
`xdotool` against a synthetic scratch library (40 papers, 3 collections, 3 tags, 1 trashed).
They show layout and interaction wiring only. They are **not** macOS acceptance: fonts
(DejaVu, not SF), Metal rendering, IME, native file pickers and drag-and-drop were not exercised.
The macOS checklist in `tests/e2e/library/README.md` still applies.

Screenshots 01–04 were taken from a throwaway copy with the macOS gates relaxed, before
Linux became a supported native lane. Screenshot 05 comes from the committed Linux lane:
`cargo build --release` of `citation-manager`, installed with `scripts/install_linux.py`
(pinned `libpdfium.so` in `<prefix>/lib/citation-manager/`), driven with Ctrl shortcuts.

| Screenshot | What it shows |
| --- | --- |
| `01-library-collection-selected.jpg` | Sidebar active state on a collection, full-width selected row, sort-able headers |
| `02-keyboard-edit-dirty.jpg` | Click row → ↓↓↓ → ↩ focuses Title → typing; dirty notice in the fixed status bar |
| `03-new-paper-preview.jpg` | ＋ New paper → Title → Preview opens Import review (tab shows “•”) |
| `04-organize-export.jpg` | Organize & export cards with explicit parent/format/scope choices |
| `05-linux-installed-ctrl-f-search.jpg` | Installed Linux release build: Ctrl+F → type → ↩ filters; hints show Ctrl+ |

Found and fixed while running the preview (would have shipped otherwise):

- GPUI panics with `hover style already set` when a style refinement is applied twice;
  buttons now choose primary/secondary styling once.
- A top “unsaved edits” banner shifted every pane down 40 px on the first keystroke, so the
  pointer landed on a different button (Preview → Expand). The notice now lives in the
  fixed-height status bar.

- On Linux, `KeyBinding::new("secondary-…")` panics at start-up (it only accepts explicit
  modifiers). Bindings now use a compile-time `cmd-`/`ctrl-` prefix, and a unit test builds
  every binding so the default lane catches this.
- Switching screens while the search field had focus unmounted it; afterwards Esc and
  shortcuts did nothing. Focus now returns to the table (Library) or the view root.

Type checks for the real target: `cargo clippy -p cm-desktop --all-targets --features native-gpui
--target aarch64-apple-darwin --locked -- -D warnings` passed on Linux with a placeholder
`shaders.metallib` (Metal shader compilation needs Xcode). This is a compile check, not a
macOS build/run.
