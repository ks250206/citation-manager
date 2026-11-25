# #52: headless desktop commands and JSON keymap

Status: accepted for the additive child #52 of #16. Base: `9e7e66e3e284a047096eae0707b823233206fe8c`.
This implements the pure portion of DESIGN §11.2–3; it does not change existing M1 handlers.
#16 still depends on #15 and remains open. #34 is HELD; #23/#25 gates remain closed.

## Ownership and contracts

`desktop::commands` owns descriptions and UI availability; `desktop::keymap` owns schema,
validation and deterministic resolution. Neither opens files, dispatches commands, registers GPUI
handlers, persists settings, nor calls a service. Only additive module registration and the existing
pinned serde/serde_json dependency edges are shared. Native views, service_client, settings I/O,
protocol, daemon, storage and SQL are untouched.

`CommandRegistry::register(Command)` permits feature-owned descriptors with dotted ASCII IDs,
Unicode titles/descriptions, and a small resource requirement. Duplicate/invalid IDs return
`RegistryError` and leave existing entries intact. The registry is deliberately not a complete
product-wide command enumeration. Future features add descriptors in their own modules; changing
shared meanings requires the existing CONTRIBUTING contract process.

`CommandContext` supplies connected handler IDs, library readiness, selection and busy state.
Its default has no connected handlers. Ui commands need a handler; library commands also require
a ready/non-busy library; selection commands also require selection. This is presentation availability,
**never** a grant, permission check, import approval or service authorization. Merely recognizing an
ID does not connect a handler. Host-reported connection does not enable a Deferred descriptor.
A match returns its current availability and is still only a request. #16 must recheck at dispatch
and use the existing reviewed import workflow; later service authorization belongs to #25/#26.

Known M1 descriptors (library/import dispatch ownership stays with #16):

| IDs | Resource requirement / meaning |
|---|---|
| `library.select_next`, `library.select_previous`, `library.select_first`, `library.select_last` | Ready library, navigate visible library/search rows |
| `paper.open`, `citation.copy` | Selection; open paper details, copy its citation |
| `paper.new`, `paper.save` | Ready library; manual draft/create/save |
| `paper.add_by_doi`, `import.bibliography` | Ready library; request DOI or BibTeX/RIS preview and explicit review |
| `search.focus`, `focus.restore`, `palette.open` | Connected UI handler; library search field, navigation focus, palette |

All are disconnected until the host explicitly connects them. Even palette.open/citation.copy do
not claim a palette/clipboard implementation exists here. `paper.open` means paper details; it does
not claim a PDF viewer is available. The small `commands::deferred` provider contains only known
examples, with an issue and disabled reason:

| ID | Connecting owner | Reason |
|---|---|---|
| `pdf.save` | #20 | PDF export is not connected |
| `pdf.highlight_selection`, `pdf.show_annotations` | #21 | PDF selection/annotations are not connected |
| `search.full_text` | #18 | Full-text search is not connected |
| `api.open_settings` | #26 | API settings are not connected |

The connecting feature removes/replaces its placeholder through the normal module integration;
registration cannot overwrite another descriptor. PDF navigation/search/annotations are #21,
PDF export #20 and API settings #26. No speculative PDF/tool ID list or auth semantics are frozen.

## Version 1 schema

The complete document replaces the keymap; it is not a patch or a merge with defaults:

```json
{
  "version": 1,
  "bindings": [
    {"context": "library", "mode": "normal", "keys": ["g", "g"], "command": "library.select_first"},
    {"context": "search", "mode": "normal", "keys": ["/"], "command": "search.focus"},
    {"context": "library", "mode": "vim_off", "keys": ["Cmd+k"], "command": "palette.open"}
  ]
}
```

- All fields are required. Unknown/duplicate JSON object fields, invalid UTF-8, malformed/trailing
  JSON, unknown context/mode and non-integer versions reject the document.
- `context`: exactly `library` or `search`. No implicit global-context precedence.
- `mode`: exactly `normal`, `insert` or `vim_off`. Host selects one effective mode; Vim OFF is
  explicit, not an alias for Normal. Context and mode scopes are disjoint.
- `keys`: 1–4 abstract event tokens, not concatenated text (`["g","g"]`, not `["gg"]`).
  A token is at most 32 UTF-8 bytes and ASCII. Letters are case-sensitive (`g`/`G`).
  Printable ASCII characters, `Space`, `Plus`, `Enter`, `Esc`, `Tab`, `Backspace`, `Delete`,
  `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown` are supported.
  Modifiers `Cmd`, `Ctrl`, `Alt`, `Shift` use `+`; duplicates reject. Order normalizes, so
  `Cmd+Ctrl+k` conflicts with `Ctrl+Cmd+k`. Modified letters must be lowercase with explicit
  Shift (`Cmd+Shift+k`); bare shifted characters are literal (`G`, not `Shift+g`).
  Explicit Shift with printable non-letters (including named `Plus`) rejects; use the
  resulting literal instead (`Cmd+?`, not `Cmd+Shift+/`). This avoids layout-dependent
  aliases hiding collisions. The plus character itself is spelled `Plus` because `+`
  separates modifiers (`Cmd+Plus`); the native adapter must use this spelling.
  `⌘K` is display notation, represented by `Cmd+k` in JSON. Unicode text is not a shortcut token.
- `command` must exactly match a registered descriptor; a known disabled command is valid.
- At most 64 KiB input, 256 bindings, nesting depth 8 (root counts as 1). The byte cap runs before
  parsing; an escape-aware structural scan enforces depth before serde recursion/allocation.
  Array and string allocation is bounded by total input bytes; binding/sequence limits then apply.
- Duplicate sequences, including different commands bound to the same sequence, reject. Either
  ordering of a strict prefix (`g` and `gg`) in the same context **and mode** rejects. Siblings
  (`gg` and `gt`) and the same key in disjoint scopes are valid. No ambiguous delayed dispatch.
- `UnreachableBinding` rejects a non-Cmd/Ctrl key anywhere in an Insert/VimOff sequence,
  and unmodified `Esc` after a sequence's first key in any mode. The resolver necessarily
  passes through or cancels these events, so accepting such bindings would be misleading.
  Modified `Ctrl+Esc` and Normal sequences beginning with `Esc` remain valid.
- `Diagnostic` has a code, schema path, optional related binding index and JSON line/column.
  Paths never include user contents; at most 64 diagnostics are returned. Validation reports a
  bounded set, not every possible error. Syntax/schema errors stop before semantic checks.

`Keymap::parse` returns only a completely validated immutable keymap or diagnostics. There is no
partly applied map. `load_or_default` returns `LoadedKeymap {source: User|Defaults, diagnostics, keymap}`.
Every rejected user document gets the same validated built-in defaults and explicit diagnostics.
If the registry cannot validate those defaults, `DefaultKeymapError` includes both failures; it does
not panic, invent missing commands or silently erase defaults. A valid empty binding list deliberately
unbinds everything and remains `User`. No setting file path, save format/migration, or filesystem
persistence policy is implemented.

The checked-in `keymap/default.json` is version 1: in Library/Search Normal, `j`, `k`, `gg`, `G`,
`Enter`, `/`, `y`, `Esc` request next/previous/first/last/open/search/copy/restore-focus. `Cmd+k`
requests the palette in all three modes, subject to editing/IME priority below. This file uses the
same parser/validation as user input.

## Input policy and finite resolution

`InputState` is supplied per event, with context, effective mode, editing flag and IME composing
flag. One `KeyResolver` belongs to one focus owner and owns its immutable keymap. No keymap
operation mutates host focus or mode. In particular, resolving `/` merely requests search focus;
#16 changes to Insert after it successfully focuses the field. The editor's own Escape handling
ends editing/changes to Normal, resets the resolver and restores focus; this layer does not steal
that editor event. Mode transitions are data supplied on the next event, not simulated focus proof.

1. Editing **or** IME composing: return PassThrough for every key, including Enter, Escape and
   Cmd/Ctrl shortcuts, and clear pending state. This conservative policy lets native editing/IME
   handlers decide. A disabled command cannot bypass this rule.
2. Insert/VimOff: also pass through every non-Cmd/Ctrl event, including navigation/deletion keys,
   Alt dead-key input and unmodified characters. Even valid custom bindings cannot override this.
   Cmd/Ctrl shortcuts may resolve when neither editing nor composing. Vim OFF never enables the
   Normal `j/k/gg/G` layer. Actual text controls must report editing even if Vim is OFF.
3. A changed InputState, explicit reset, 1,000 ms inactivity or backwards monotonic time drops the
   prefix. Each event has caller-supplied monotonic milliseconds; no threads/timers are spawned.
   An expired prefix is logically dropped before the next event; the host resets/hides any pending
   indicator on its own timer. At most four keys can be pending/matched.
4. In navigation Normal, Escape with a prefix returns Cancelled and clears it. Without a prefix,
   it follows the map (`focus.restore` in defaults). A mismatch drops the prefix and retries only
   the current event as a fresh sequence; an unknown event passes through. Earlier prefix events
   are not replayed. Completion returns Command with fresh availability, then clears state.

#16 must translate real platform/layout events to canonical tokens; for an unsupported event or
Unicode text event it must pass through and reset. It must reset on **every** focus loss/change,
mouse navigation, successful focus/mode transition, map replacement, and cancellation, even if no
key event occurred during that transition. Recreate the resolver to replace its map. Calling code
must not consume PassThrough or execute disabled Command results. Auto-repeat/layout behavior,
accessibility and multi-window routing require native acceptance by that connecting feature.

## Integration still owned by #16

After #15 acceptance and normal main integration, connect library/import and navigation handlers
through their host; add palette UI/fuzzy matching/MRU/shortcut display and settings I/O separately.
Revalidate persisted bytes with this parser, expose diagnostics/fallback to the user, report true
editing/composition/focus state, and preserve native editing and mouse behavior. The #52 tests
prove only the supplied-data policy and finite resolver, not real GPUI focus, event bubbling,
keyboard layouts, IME composition, macOS behavior or clipboard behavior. No UI was launched for
this child. See [#52 evidence](../validation/ISSUE_52.md). No #15/#16 acceptance box is completed here.
