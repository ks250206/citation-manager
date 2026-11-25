# #20 Linux native evidence

Debug build of this branch installed with `scripts/install_linux.py` (pinned PDFium) into a
scratch prefix, Xvfb + xdotool, a copy of a scratch library with the synthetic corpus attached
and one #19 highlight.

| File | Shows |
|---|---|
| 01-viewer-save-pdf-button.jpg | Paper → PDF tab: the viewer toolbar has "Save PDF…" next to Copy |
| 02-palette-save-current-pdf.jpg | Ctrl+K, "save pdf": the `pdf.save` command "Save Current PDF…" is listed and enabled (not the former #16 deferred placeholder) |
| 03-no-portal-status.jpg | Clicking the button / running the command calls `cx.prompt_for_new_path`; this container has no XDG desktop portal, so the dialog fails and the status bar says "The save dialog failed; nothing written" |

Not verified here: choosing a file in a real save dialog (macOS NSSavePanel or an XDG portal),
the replace confirmation of those dialogs, and the GUI progress/outcome messages of a completed
export. The export behind the dialog is the same `PdfRequest::Export` exercised end-to-end by
`crates/daemon/tests/pdf_export_runtime.rs`; the status texts are unit-tested in
`crates/desktop/tests/pdf_export.rs`.
