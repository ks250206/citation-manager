use super::{Command, Requirement};

/// Known design examples only. Their feature owners replace these descriptors when connected.
pub fn deferred_commands() -> Vec<Command> {
    [
        (
            "pdf.save",
            "Save Current PDF",
            20,
            "PDF export is not connected",
        ),
        (
            "pdf.highlight_selection",
            "Highlight Selection",
            21,
            "PDF selection/annotations are not connected",
        ),
        (
            "pdf.show_annotations",
            "Show Annotations",
            21,
            "PDF annotations are not connected",
        ),
        (
            "search.full_text",
            "Search Full Text",
            18,
            "Full-text search is not connected",
        ),
        (
            "api.open_settings",
            "Open API Settings",
            26,
            "API settings are not connected",
        ),
    ]
    .into_iter()
    .map(|(id, title, issue, reason)| Command {
        id,
        title,
        description: reason,
        requirement: Requirement::Deferred { issue, reason },
    })
    .collect()
}
