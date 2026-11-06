# #10: native desktop bootstrap and replaceable library client

Status: adopted for the M0 vertical slice; transition to #25 is explicit.

`cm-desktop → cm-protocol` remains the view boundary. `protocol::library` exposes
`LibraryClient::submit(request, completion)` and feature-owned CRUD DTOs (reusing
#8 `NewPaper` / `PaperList` / `Paper`). No transport, SQLite or GPUI enters protocol.
Submit enqueues work; completion may run on a worker and is delivered to the UI
through a response channel. Future IPC implements this client without changing views.
This does not freeze #25's wire envelope, auth, JobStatus or daemon lifecycle.

The optional **macOS-only**, **off-by-default** `cm-daemon/desktop-bootstrap` feature
adds the single `daemon → desktop` composition edge and the real `citation-manager`
executable. It injects `daemon::desktop_worker::InProcessLibraryClient` into the UI.
There is no 13th crate. The architecture validator checks the target, optional flag,
normal dependency kind, exact bootstrap feature wiring, and transitive default-feature
exclusion. Negative tests reject unconditional/non-macOS/dev/build/default-enabled
forms and the reverse desktop-to-daemon edge. Other boundaries are unchanged.

All path preparation, database open/migration, reads and writes run on the named
`library-worker` thread. The worker owns the #8 `LibraryDb` and delegates to
`PaperService`; views never receive a repository/connection or execute SQL.
Open failures are retryable. Existing DB migrations still require #9's backup hook:
this bootstrap fails closed when an upgrade needs that hook, instead of bypassing it.
No attachment file-store/backup implementation is added by #10.

The executable requires an explicit absolute `--library` path. It never silently
opens a default real library. Temp paths are used for all automated/native checks.
The worker lives inside the desktop process; **no daemon process, IPC socket, HTTP
listener, grant issuance, or multi-process writer exclusion is implemented**.
HTTP remains off. #25 will replace the transport, add lifecycle/single-writer
ownership and enforce the existing security ADR from the first IPC request.
The current adapter is not a remotely callable unauthenticated server.

UI response generations reject stale editor data and errors. List/Load more has a
separate pending slot: New/Get does not discard its rows or replace the editor. Get/Save
replies completed after a list began override that list's older rows, with its server cursor
kept separate. Saving locks in-window mutation and navigation until confirmation; failures
retain the draft and last confirmed paper. A dropped callback (including worker unwind)
is converted into Unavailable by a UI-side completion guard and releases the save lock. The minimal editor updates title/abstract only and preserves all
other bibliographic fields/IDs. A previously absent abstract remains None after title-only
edits; CR/LF stays in stored values and is replaced only for single-line display, preserving
byte and UTF-16 positions. IME preedit save attempts show an instruction to finish conversion. List paging retains the service cursor independently
of newly created rows, avoiding skipped pages after an insert. Navigation/PDF host
functions in `native/bootstrap_views.rs` are the replacement points for #15/#17.
The manual editor is single-line, supports Unicode/grapheme movement, text selection,
clipboard, native IME callbacks and horizontal caret scrolling. Undo/redo, multi-line
abstract editing, dirty-navigation confirmation, collections, DOI resolution/import,
PDF rendering and command palette are future work. Native IME composition is wired
from the pinned official GPUI example, but Japanese input-source candidate selection
was not exercised in this session; Unicode paste/edit was exercised.

The hosted native CI lane checks/clippies/tests/builds the **actual executable** with
its feature, without launching a GUI. Native acceptance evidence is separate in
[ISSUE_10](../validation/ISSUE_10.md). The unsigned throwaway .app made by the smoke
helper only allows CUA/LaunchServices to address the executable. It is not #34's
product bundle or #31's signing/notarization/distribution work.

API evidence: `find-docs` resolved GPUI and queried `EntityInputHandler` and async
execution on 2026-11-06; API signatures were then checked against the official pinned
[source](https://github.com/zed-industries/zed/tree/4a6121bb0f10c6e8037b741b7dc0a4cd639ae289/crates/gpui).
`text_input.rs` adapts that revision's `examples/input.rs`; attribution and the
[Apache license](../licenses/GPUI-APACHE-2.0.txt) are retained. No GPUI revision changed.

Quitting is a separate boundary: Cmd+Q/window close currently ends the process even while
Saving. The UI does not claim a completed save until Saved locally appears. Closing earlier
leaves the outcome unknown (SQLite commits atomically); reopen and inspect the record before
retrying. The worker only drains on last-client drop while its process remains alive; app quit
does not join it. A future quit-confirmation UI belongs with the full editor, not this bootstrap.
