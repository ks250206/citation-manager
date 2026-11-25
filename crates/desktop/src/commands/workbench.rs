use super::{Command, Requirement};

/// Workbench (#15/#16) commands the native host connects: screens, layout, organization
/// and keyboard settings. Additive feature module; library/import IDs live in `library`.
pub mod ids {
    pub const VIEW_LIBRARY: &str = "view.library";
    pub const VIEW_IMPORT_REVIEW: &str = "view.import_review";
    pub const VIEW_PDFS: &str = "view.pdfs";
    pub const VIEW_ORGANIZE: &str = "view.organize";
    pub const RELOAD: &str = "library.reload";
    pub const CLEAR_FILTERS: &str = "library.clear_filters";
    pub const SHOW_TRASH: &str = "library.show_trash";
    pub const SHOW_ALL: &str = "library.show_all";
    pub const TRASH: &str = "paper.move_to_trash";
    pub const DISCARD: &str = "paper.discard_edits";
    pub const READING: &str = "layout.toggle_reading";
    pub const EXPORT: &str = "export.bibliography";
    pub const TOGGLE_VIM: &str = "keymap.toggle_vim";
    pub const RELOAD_KEYMAP: &str = "keymap.reload";
    pub const COPY_DEFAULT_KEYMAP: &str = "keymap.copy_default";
}

pub fn workbench_commands() -> Vec<Command> {
    use Requirement::{Library, Selection, Ui};
    [
        (
            ids::VIEW_LIBRARY,
            "Go to Library",
            "Show the library table",
            Ui,
        ),
        (
            ids::VIEW_IMPORT_REVIEW,
            "Go to Import Review",
            "Show pending import previews and decisions",
            Ui,
        ),
        (
            ids::VIEW_PDFS,
            "Add PDFs",
            "Register PDF files or a folder after review",
            Library,
        ),
        (
            ids::VIEW_ORGANIZE,
            "Organize & Export",
            "Collections, tags, export and layout settings",
            Ui,
        ),
        (
            ids::RELOAD,
            "Reload Library",
            "Re-run the current library query",
            Library,
        ),
        (
            ids::CLEAR_FILTERS,
            "Clear Filters",
            "Reset keyword, author, year, type, PDF, collection and tag filters",
            Library,
        ),
        (
            ids::SHOW_TRASH,
            "Show Trash",
            "List papers moved to Trash",
            Library,
        ),
        (
            ids::SHOW_ALL,
            "Show All Papers",
            "List all active papers",
            Library,
        ),
        (
            ids::TRASH,
            "Move Paper to Trash / Restore",
            "Move the selected paper to Trash, or restore it from Trash",
            Selection,
        ),
        (
            ids::DISCARD,
            "Discard Metadata Edits",
            "Restore the unsaved form to the stored values",
            Ui,
        ),
        (
            ids::READING,
            "Toggle Reading Layout",
            "Expand the details pane or return to three panes",
            Ui,
        ),
        (
            ids::EXPORT,
            "Export Bibliography",
            "Choose format and papers, then write a new BibTeX/RIS file",
            Library,
        ),
        (
            ids::TOGGLE_VIM,
            "Toggle Vim Keys",
            "Turn j/k/gg/G/y and / navigation in the library table on or off",
            Ui,
        ),
        (
            ids::RELOAD_KEYMAP,
            "Reload Keymap",
            "Re-read keymap.json beside the settings file",
            Ui,
        ),
        (
            ids::COPY_DEFAULT_KEYMAP,
            "Copy Default Keymap JSON",
            "Copy the built-in keymap document to start a keymap.json",
            Ui,
        ),
    ]
    .into_iter()
    .map(|(id, title, description, requirement)| Command {
        id,
        title,
        description,
        requirement,
    })
    .collect()
}
