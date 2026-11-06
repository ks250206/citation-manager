# #52: pure command/keymap implementation evidence

2026-11-07, implementation agent in the T3-prepared isolated checkout
`/Users/ryojikanno/.t3/worktrees/citation-manager/t3-52b5e36a`, branch
`t3/desktop-command-keymap`. Source base / migration comparison base:
`9e7e66e3e284a047096eae0707b823233206fe8c`, verified against live main before implementation.
Live #52 and #16 and their comments were read; GitHub's parent endpoint confirms #52 is a real
child of #16. Existing #15 / PR #45 and #34 / PR #42 remain outside this change.

Environment: macOS 27.2 (26B123), arm64; rustc 1.99.1 (58cf8f9c3 2026-10-13),
cargo 1.99.1 (5f94df478 2026-08-27), Python 3.14.0, Xcode 26.6 (17F113).

## Implemented and scoped

See [schema, policy and integration ADR](../ADR/desktop-commands-keymap.md).
The only production changes are feature-owned pure `desktop::{commands,keymap}` modules,
additive lib.rs registrations and the already-pinned serde/serde_json dependency edges.
Known commands have descriptions/resource availability, unconnected handlers fail disabled,
and known deferred PDF/API descriptors carry owner/reason. Configuration validation is whole-map,
bounded and versioned, with explicit validated defaults. Resolver policy is deterministic and
headless; it includes context/mode separation, editing/IME priority, prefix cancellation, reset,
timeout and mismatch handling. No native/GUI/service/settings/protocol/daemon/storage behavior
was changed or invoked by this feature. No UI was launched or automated.

## Local validation

All required local checks passed. [Machine-readable commands/results](ISSUE_52/checks.json) and
[focused test output](ISSUE_52/10-focused.log) are committed alongside this record.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed, no warnings |
| `cargo test --workspace --locked` | passed, 205 tests including the 19 new headless tests and existing compile-fail doctest |
| `cargo test -p cm-desktop --test commands_keymap --locked` | passed, 19/19 |
| `cargo build --workspace --locked` | passed |
| `python3 scripts/check_architecture.py` | passed, 12 members and allowed boundaries |
| `python3 scripts/check_migrations.py --base 9e7e66e3e284a047096eae0707b823233206fe8c` | passed, two migrations/base preserved |
| `python3 -m unittest discover -s scripts -p 'test_*.py' -v` | passed, 22/22 including benchmark instrument tests |
| `PYTHONPATH=scripts python3 -m unittest discover -s scripts/benchmarks -p 'test_*.py' -v` | passed, 4/4 additional focused invocation |
| `git diff --check` | passed; staged diff also checked before commit |

An extra standalone benchmark discovery attempt omitted `PYTHONPATH=scripts` and failed to import
`benchmarks.run`. The corrected invocation passed; full scripts discovery had already passed all
22 tests including those four. Both attempt logs are preserved (terminal trailing blank lines are trimmed). No product change was made for
this invocation error. No local required test failure remains unresolved.

The dedicated tests exercise independent expected command IDs/results, not just parser roundtrips:

- Additive Unicode descriptor registration; duplicate rejection preserves the original descriptor.
- Enabled, unconnected, missing-library, missing-selection and busy availability; unknown IDs fail
  closed. Known PDF/API deferred commands remain disabled despite claimed host connections.
- Both Library/Search defaults for j/k/gg/G/Enter///y/Esc and Cmd+K in Normal/Insert/VimOff.
  Known disabled commands are valid JSON but produce disabled matches.
- Context-specific remaps and mode isolation; successful prefix completion, sibling sequences,
  Escape cancellation, explicit reset, mismatch/retry, exact timeout boundary and backwards time.
- Editing/IME passes every tested character, Enter/Escape, modifier shortcut, navigation/delete
  and Alt dead-key event and clears pending input. Even custom Insert/VimOff bindings cannot
  consume ordinary editing keys. Mode changes discard prefixes.
- Whole-document fallback for unknown commands, invalid keys, duplicate sequences and prefix
  conflicts; same-context/same-mode conflicts in either order; modifier-order alias collisions.
- Invalid UTF-8/JSON/schema/object duplicates/unknown fields/version/context/mode, Unicode keys,
  quoted/escaped structural characters, redacted diagnostics and explicit broken-default errors.
- Exact byte/binding/sequence boundaries, excess limits, nesting bounds and a deliberate empty
  user keymap. There is no partly applied valid prefix of a rejected document.

## Immutable inputs / dependency proof

All 636 Cargo package identities, versions, sources and checksums were compared with the exact
base. All package records are identical except cm-desktop's added dependency on existing serde.
serde_json moves from dev-only to normal dependency without changing its already-locked edge.
No new package/version or workspace dependency pin was added. Both immutable SQL files and their
manifest are byte-identical to base; SHA-256 evidence is in `ISSUE_52/invariants.json`.
The validator also compares with the explicit base SHA (rather than only current checksums).

Initial locked test correctly refused the missing new direct serde edge. An offline full lock
regeneration proposed unrelated transitive drift; that output was discarded. Cargo resolved the
minimal edge from the restored original lock using `cargo check -p cm-desktop --offline`, and all
required checks below use `--locked` on the preserved pins. No unrelated lock drift remains.
An EOF blank line in the desktop manifest was removed after diff-check identified it.

## Limits and handoff

No real focus, IME, input bubbling, palette rendering/fuzzy search/MRU, clipboard, mouse,
accessibility, filesystem persistence or actual command dispatch is proven by these tests.
Mode transitions are caller-supplied data; `/` requests focus but does not establish it. The
connecting #16 must use the editor's own Escape handling and reset on focus/mouse/map changes.
The native/PDF runtime/signing lanes were not rerun locally for this pure change; their existing
CI jobs remain separate from the local headless results. No native GUI process was started.

Independent Opus/medium read-only review and final CI/main integration are root responsibilities,
**pending at implementation handoff**, not passed by this agent. The implementation agent takes
one CI snapshot after opening the Ready PR, with no merge/watch/poll. PR closes #52 only;
#15/#16 remain open, #16 still depends on #15, #34 remains HELD and #23/#25 gates remain closed.
