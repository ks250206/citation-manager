# #15: native library workbench and reviewed local imports

Status: implementation connected; **native acceptance is blocked**, not complete.
See [executed validation and remaining paths](../validation/ISSUE_15.md).

Desktop uses only owned protocol DTOs and an injected `LibraryClient`. Feature-local
organization, exchange, attachments and settings requests extend the existing client
with default Unavailable methods, preserving lightweight older clients. This is the
existing in-process adapter, not IPC/auth/lifecycle, a daemon or a durable job envelope.
No command registry/keymap design, viewer/annotation implementation or note persistence
is introduced. The Notes/PDF tabs state unavailable capabilities plainly.

The library worker owns two ordinary #8 connections to the same explicit private
library: one for services and one borrowed for the lifetime of the retained #33
ImportCoordinator. Its connection-bound revision detects writes through the other
connection. Open/upgrade gates remain unchanged. The existing #10 optional macOS
`daemon → desktop` edge stays off by default. `desktop-bootstrap` additionally enables
`pdf-metadata → cm-pdf/runtime`; the validator checks this exact additive wiring and
still rejects reverse/framework/driver/default-enabled/non-macOS edges. Default
workspace tests retain the compile-only PDF boundary. No dependency versions/checksums
changed: all 636 identities remain. Exact roxmltree 0.21.2 was already locked; only its
PDF-owned XML parsing edge was activated. Existing reqwest/GPUI/PDFium pins remain.

The worker queue is bounded to 32 nonblocking submissions. DOI has a separate bounded
8-request synchronous worker, reusing #14 HttpProviders and its cancellation/cache/
backoff. HTTP/provider/parse/normalization/SQLite/filesystem/PDFium work stays off the
GUI event thread. Local callbacks may run on workers; UI-side dropped-completion guards
release pending states. Per-operation generations ignore stale query/editor/provider/
attachment results. Editing/replacing the DOI editor cancels its token; stale preview
handles are cancelled instead of being retained invisibly. This does not provide process
failure replay. Quit during a write can still leave an unknown result: inspect the library
before retrying, and wait for confirmation before quitting.

DOI and PDF cancellation tokens are separate. Idle PDF Cancel has no effect. Navigating
to PDF or Import review invalidates a pending DOI lookup generation. Every Editor,
Import or PDF ownership request invalidates that ticket and cancels only the DOI token
before submission; the receive ticket filter rejects already-queued replies as well.
DOI reply/manual-preview handlers also reject replacement while locked. Unrelated reads
retain the lookup. These guards keep late provider replies from replacing or releasing
a PDF preview during commit. Read-only Get completions do not claim a save or clear a PDF failure summary.

Library pages use #12's real combined filters and cursor identity, at most 100 full
papers per UI page, with uniform_list virtualization. Filter/sort/membership/metadata/
import/attachment changes reload the cursor. Selection across pages is explicit IDs.
The tree uses direct membership, tags support AND/OR, and metadata search has no FTS claim.
Forward/back navigation commits its cursor and history only when the page query succeeds.
A failed query keeps the previous visible page and permits the same move to be retried;
stale replies cannot commit or cancel a newer navigation.
Reload/filter/sort resets clear the old table rows so a failure cannot present them
under a different cursor identity. The separate metadata form is retained.
CRUD, multiple memberships and trash/restore delegate to existing services. Unedited
bibliography/roles/ORCID/external IDs/extra data survive form edits; editing Authors uses
explicit semicolon-delimited literal names, without guessing a person's name structure.
Unsaved library metadata remains in its form when changing screens. New, paper selection,
DOI replacement and trash/restore are blocked until Save or the explicit Discard metadata
edits control. Background Get also retains a dirty form, including after a separate
import/PDF commit: the confirmed record is saved, but the form is not silently replaced.
Discard restores the form's original values and does not delete any committed record.
The persistent unsaved banner is independent of operation status messages. Quit still
has the documented unknown-write/unsaved-draft limitation; no quit confirmation is claimed.

New manual/DOI and bibliography records pass through #33 preview/decisions. #13 adapters
parse bounded UTF-8 on the worker, retain both file and row diagnostics, source lines,
case/compatibility warnings and normalized duplicate field differences. Error candidates
can only Skip. Choosing Create for eligible rows is a user action for the current preview;
commit separately requires a decision for every row. No automatic next preview, approval,
retry or larger budget. Default batch size 100 can be reduced explicitly for payload limits.
A 1000-record file requires ten separately approved previews/commits. Cancellation between
batches preserves prior commits. The displayed destination is fixed for each preview.
Switching to PDF/manual review releases the current retained preview and pauses file
batch counters. Resume restores file diagnostics/counters and requires a fresh preview
and fresh decisions. An unrelated PDF report preserves a non-PDF preview. Cancel submission
failure keeps the old handle/decisions and blocks replacement. Selecting another bibliography
explicitly reports stopped progress before starting a new file session.
PDF reports still refresh the list/form/retained paper if preview-release submission
fails, while displaying the release error and retaining the handle for retry. Cancelling
an active non-file preview leaves paused bibliography resumable only by explicit Resume;
the cancellation status distinguishes this from stopping the active file session.
Export All/selected/direct collection calls #13 ExportService on the actual #12 query,
writes a new selected filename, and surfaces losses and the bibliography-only migration
notice. An export IO failure may leave a partial new output file; it never overwrites an
existing destination or claims completion in that case.
The native Save picker starts at the absolute HOME directory rather than a shared
temporary directory. Missing/relative HOME produces a visible error without selecting a
fallback path. The operator still chooses the actual output; filesystem IO remains on
the worker, including the existing private-output and no-overwrite checks.

PDF selection uses actual GPUI native Open/Save pickers and ExternalPaths drop handlers.
PDF metadata uses a dedicated form bound to row index, source path and digest; it cannot
read fields from the library/manual editor. Matching sources retain edits, while changed
origins require reloading. Cmd+S only saves on the Library screen; PDF registration always
uses the explicit PDF preview and reviewed commit, preserving attachment intent.
Unsubmitted PDF edits belong to the current row; changing rows resets that form, as the
metadata review text states. Per-row draft persistence is not claimed.
Explicit AttachExisting never imports bibliography or creates a paper. RegisterNew uses
#33 `file_metadata::propose`, then a retained ordinary preview. The serial #11 worker has
one additive bounded Metadata operation, returning source digest, page count and neutral
suggestions. Input remains capped at 32 MiB for PDFium; selected attachment ingestion is
capped at 256 MiB. Larger/unreadable/scanned/corrupt/encrypted/missing-runtime documents
can use an explicitly reviewed filename/manual draft. Original bytes are never rewritten.

Info tags come from the real PDFium document. Bounded (64 KiB), namespace-aware UTF-8
uncompressed XMP packet hints are parsed without DTD/entity expansion or TeX. Compressed
packets are not inferred, and packet association with the current PDF catalog is not proven
by the byte scan; all hints are untrusted suggestions requiring review, not authority over
bibliography. Competing Info title/DOI values are displayed. No OCR, password UI, fulltext
search, PDF download, viewer or annotation editing is claimed.

Folder traversal is explicit nonrecursive/recursive, scoped to the chosen directory, with
1000 entry/directory bounds, no symlink ancestors or hard-linked/nonregular sources.
Non-PDF, oversized, duplicate hash and per-entry errors remain visible. Hashing is streamed
and cancellable; progress is sent after each assessed/attached file and coalesced to one latest UI snapshot,
so a delayed frame does not queue growing folder reports. Native calls and one
atomic #9 ingest are cooperative boundaries, not forcibly interrupted. The confirmed
source/digest is revalidated before paper commit and again by #9 atomic shared-blob ingestion.
If the paper commits and attachment fails/cancels, its real ID remains in the report. Retry
uses that ID, not another paper commit. A bounded in-process partial-file list preserves this
binding across reinspection of the same source/digest; it is not a future #25 durable job.
After a process loss, inspect committed records/references before retrying. No deletion,
GC, file restore, permission repair or in-place blob replacement is added.

Settings are worker-owned versioned `ui-settings.json` (or an explicit absolute private
path). Load is bounded to 16 KiB, validates columns/order/visibility/width/sort/panes/tab,
and falls back with a visible warning. Save requires owned private 0700 parent/0600 file,
rejects links, flushes a unique tempfile, atomically renames and flushes its directory.
Unsafe files are never silently repaired. UI preferences are outside schema migrations
and, consistently with #9, outside portable library-data backups. Restore gets default
layout unless settings are copied separately by an authorized user.

API signatures were checked via find-docs and the official pinned [GPUI source](https://github.com/zed-industries/zed/tree/4a6121bb0f10c6e8037b741b7dc0a4cd639ae289/crates/gpui),
local pdfium-render 0.9.5 source and existing [reqwest 0.13.5](https://docs.rs/reqwest/0.13.5/reqwest/).
The fixed PDFium digest remains the #11 runtime-lock value; the product has no environment,
PATH/system-library or downloaded override fallback. The temporary native test bundle is
not #34's product layout/browser/lifetime gate; that work remains held. #23/#25, native-host
registration, browser profiles, VMs, signing/notarization and user settings are untouched.


## Native acceptance corrections (2026-11-07)

Actual CUA observation found that GPUI collapses leading spaces in collection labels;
hierarchy now uses per-depth padding. Long import titles use a single-line truncated
row with the decision first, while the right pane retains full metadata. These are
rendering corrections, not changes to stored titles or collection ownership.

A successful Trash action no longer submits the active-only membership read for a
deleted paper, which previously replaced success with an expected NotFound error.
Restore refreshes membership when the paper is active again. Manual/PDF review progress
counts its own candidates; paused bibliography counters retain their separate total.

The [awake native evidence](../validation/issue-15/native-awake.md) records real GUI
imports, pickers, private settings, partial attachment failures/cancellation and source
hashes. It supersedes the historical inability to observe a window, while explicitly
leaving D&D, offline/concurrency and failed-query acceptance incomplete. PR45 remains
Draft; #34 stays HELD. No command registry/keymap or search experiment is added here.
