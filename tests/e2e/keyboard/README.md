# #16 keyboard / Command Palette checklist

Automated coverage (default lane): `crates/desktop/tests/palette.rs` (ranking, history, key
adapter, shortcut labels, citation text, settings compatibility), `commands_keymap.rs`
(registry, keymap validation, resolver) and `native::tests::every_key_binding_parses_on_this_platform`.

Native checks (run on a real desktop; record results in docs/validation):

1. ⌘K / Ctrl+K opens the palette from the table, a text field and Organize & export.
   Typing filters (fuzzy, word starts first); ↑/↓ move; ↩ runs; Esc or a click outside closes
   and returns focus to the table (Library) or the window.
2. Empty query lists recently used commands first; restart keeps that order.
3. Disabled entries show a reason (e.g. "Select a paper first", "#20") and do not run.
4. Vim keys on: j/k, gg/G, y (clipboard holds the citation), / focuses search, Esc restores.
   Vim keys off: those letters do nothing in the table; arrows/↩ still work.
5. Japanese IME in the search and metadata fields: composing, converting with ↩ and Esc never
   triggers commands; ↩ after conversion submits only when nothing is being composed.
6. keymap.json beside the settings file (0600): an invalid file is ignored as a whole with a
   status warning and per-binding diagnostics under Organize & export › Keyboard; a valid file
   replaces the defaults after Reload Keymap.
7. Mouse: clicking palette rows runs them; clicking table rows still selects.
8. A pending prefix never outlives an unrelated interaction: press `g`, then click another row
   (or ⌘F / Ctrl+F into search and Esc back) and press `g` within a second — the selection must
   not jump to the first paper.
9. #84 table selection and menus: Shift+click and Shift+↑/↓ select a range from the anchor,
   ⌘/Ctrl+click toggles a row, ⌘A / Ctrl+A selects the page; the details pane follows the last
   row. Right-click or Shift+F10 opens the row menu with shortcuts shown; ↑/↓, →/←, ↩ and Esc
   drive it and its submenus, and no workbench shortcut fires behind it. ＋ Add and ⚙ Settings
   open the same kind of menu; Esc returns focus to the table.
