use super::{Command, Requirement};

/// #20 annotated PDF export. Replaces the `pdf.save` placeholder that `deferred` held for
/// this issue; the native host connects it (palette, keymap and the viewer toolbar button).
pub mod ids {
    pub const SAVE: &str = "pdf.save";
}

pub fn pdf_export_commands() -> Vec<Command> {
    vec![Command {
        id: ids::SAVE,
        title: "Save Current PDF…",
        description: "Save a copy of the open PDF with its highlights, comments and notes",
        // A paper must be open; the host additionally requires an open PDF at dispatch.
        requirement: Requirement::Selection,
    }]
}
