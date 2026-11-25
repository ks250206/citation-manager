# #84 M6-1: library list and top bar — validation

What changed is described in [library-workbench](../ADR/library-workbench.md#84-library-list-and-top-bar-m6-1),
[library-organization-query](../ADR/library-organization-query.md) (author / journal sort keys,
`LibraryPage.with_pdf`) and [desktop-commands-keymap](../ADR/desktop-commands-keymap.md#84-list-commands-table-keys-and-popup-menus).

## Automated

| Area | Tests |
|---|---|
| Author / journal keyset paging (both directions, page sizes 1–8, missing keys last, editors skipped, literal and given names, cursor round-trip, cross-sort cursor rejected) and PDF marks per page | `cm-storage` `library_organization::author_and_journal_sorts_page_with_keyset_cursors_and_mark_pdfs` |
| Cursor key shape for the new sorts; old sorts unchanged | `cm-core` `library_query::author_and_journal_cursors_carry_an_optional_text_key` |
| The worker (same path as IPC) returns PDF marks under the new sorts | `cm-daemon` `library_workbench::pdf_offline_registration_…` |
| Column cells (first author + et al.), default and toggled columns, header sort cycle, labels | `cm-desktop` `library_list::{author_cell_…, cells_columns_and_header_sorting}` |
| Selection: plain / Cmd-Ctrl / Shift / Cmd-Ctrl+Shift clicks, Shift+↑↓ from the anchor, select all, right-click focus | `library_list::{plain_toggle_and_shift_clicks_…, shift_arrows_grow_and_shrink_…}` |
| Menu navigation (separators, disabled rows, submenus, pointer) | `library_list::menus_skip_inert_rows_…` |
| Bulk collection / tag / Trash requests, in order, stopping at the first failure | `library_list::{bulk_requests_…, bulk_runs_in_order_…}` |
| `with_pdf` is optional on the wire (old replies decode, empty marks omitted) | `library_list::page_pdf_marks_are_optional_on_the_wire` |
| Viewer keeps the page position across a scale change | `library_list::rescaling_keeps_the_same_page_position_in_view` |
| Pre-#84 settings files upgrade (widths kept, new default visibility); Title cannot be hidden | `palette::settings_accept_old_files_and_reject_invalid_history` |

No migration was added (sorting on the new keys uses the same per-page ordering as title / year).

## In the running app (Linux, Xvfb, real PDFium)

Built with `cargo build -p cm-daemon --bins --features desktop-bootstrap`, installed into a
private prefix with the pinned PDFium, private library and settings seeded with the `papers`
CLI: 36 BibTeX records (1–4 authors, 7 journals and some without one), two PDFs attached to one
paper and one to another, a page note copied to the second PDF as an unresolved note.
The same steps were run on the pre-change build for the "before" pictures. Driven with
xdotool; every result below was checked on a screenshot.

| Check | Result | Screenshot |
|---|---|---|
| Before: Title/Year/Type/DOI, Import BibTeX… / Import RIS… / Organize & export / API & integrations in the top bar | reference | [01](issue-84/01-before-library.png) |
| Default columns Author / Title / Year / Journal / PDF badge; a pre-#84 settings file loaded without a warning and was rewritten in the new layout with its widths | pass | [02](issue-84/02-after-library.png) |
| Header clicks: Author ▲, Title ▼ (second click), Journal ▲ with journal-less papers last; status and footer name the sort; the choice is persisted | pass | [03](issue-84/03-after-context-menu.png) |
| Click, Shift+click, Ctrl+click → 4 selected; right-click inside the selection keeps it and focuses that row; menu shows Open ↩, Copy 4 citations Ctrl+Shift+C, Add to collection ▸, Tags ▸, Move 4 papers to Trash Delete; keyboard ↓/→/↩ ran "Add #battery" on all 4 | pass | [03](issue-84/03-after-context-menu.png) |
| Shift+↓ then Shift+F10 opens the same menu under the focused row; Esc returns to the table | pass | [04](issue-84/04-after-keyboard-menu.png) |
| Add to collection for 4 papers ("Added to Reading list: 4 papers"); Ctrl+Shift+C put 3 citation lines on the clipboard; Delete moved 3 papers to Trash; Ctrl+A + Delete in Trash restored them | pass | (status line, not kept) |
| ＋ Add menu: New paper Ctrl+N, Add paper by DOI…, Add PDFs…, Import BibTeX…, Import RIS…; ↑/↓ skip the separator | pass | [05](issue-84/05-after-add-menu.png) |
| ⚙ Settings menu: Organize & export Ctrl+,, API & integrations, Table columns ▸ (toggling DOI kept the menu open), Vim keys ✓, Reading layout, Command Palette Ctrl+K | pass | [06](issue-84/06-after-settings-menu.png) |
| API & integrations opened from ⚙ and from the palette; both settings screens link to each other; Esc returns to the library | pass | [07](issue-84/07-after-api-screen.png) |
| Bug: in the default pane the second attachment was cut off under the viewer toolbar | reproduced before, fixed after | [08](issue-84/08-before-attachments-default.png) → [09](issue-84/09-after-attachments-default.png) |
| Bug: Expand, scroll to the end of page 4, Notes › Place in PDF… showed a blank page | reproduced before, fixed after (opens at the top of p. 4, "Place on p. 4") | [10](issue-84/10-before-place-expand.png) → [11](issue-84/11-after-place-expand.png) |
| Exit reading after that: the viewer stays on the top of p. 4 at the narrower width | pass | (not kept) |

## Review follow-up (PR #99)

Three findings from the review were fixed with tests on the pure logic. Each new test failed when
the function was given the pre-fix behaviour and passes now (`cm-desktop` `library_list`):

| Finding | Fix | Test |
|---|---|---|
| Re-selecting the open paper kept a stale editor after another client changed it; a later Save would send the old bibliography back | `library::newer_row`: a clean editor is refreshed from the newer page row, on re-selection and whenever a page loads (attachments, viewer and notes are kept). Unsaved local edits are never replaced | `the_page_version_of_a_changed_open_paper_is_detected` |
| A tag/collection bulk run and a Trash bulk run could overlap and share state, so one run's follow-up read replaced the other's ticket | `bulk::BulkSlot`: one bulk run at a time (a second one, or a single membership change, is refused with "Wait until … finishes"); each completion takes its own verb and refetch flag from its lane and ticket | `bulk_runs_are_serialized_and_completions_keep_their_own_state` |
| The Tags submenu was built before the right-clicked paper's memberships had loaded, offering existing tags as "add" | `bulk::tag_choice`: for one paper, tags are disabled ("loading…") until that paper's memberships arrive, and the open menu is rebuilt when they do | `tag_entries_wait_for_the_selected_papers_memberships` |

In the app: with one paper open, right-clicking another paper tagged #battery showed "✓ #battery";
choosing it reported "Removed #battery from 1 paper" and the menu then showed it unchecked. A
three-paper bulk add to a collection with Delete pressed straight after it left the three papers
in place (the Trash run was refused while the first run was active) and ended with that run's own
status, "Added to Reading list: 3 papers".

## Not covered

- Import BibTeX… / Import RIS… from the ＋ Add menu open the XDG portal file chooser, which is
  not available under this Xvfb session; the menu entries and palette commands were checked to
  be present and enabled, and the import itself is unchanged.
- macOS (⌘ shortcuts, menu rendering) was not run; key bindings are checked to parse by the
  default test lane only.
- Paging past 100 rows under the new sorts was verified by the storage tests, not in the app
  (the seeded library has one page).
