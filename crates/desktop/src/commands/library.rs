use super::{Command, Requirement};

/// Only the known library/import and navigation surface. This is not a product-wide ID list.
pub mod ids {
    pub const NEXT: &str = "library.select_next";
    pub const PREVIOUS: &str = "library.select_previous";
    pub const FIRST: &str = "library.select_first";
    pub const LAST: &str = "library.select_last";
    pub const OPEN: &str = "paper.open";
    pub const SEARCH: &str = "search.focus";
    pub const COPY: &str = "citation.copy";
    pub const RESTORE_FOCUS: &str = "focus.restore";
    pub const PALETTE: &str = "palette.open";
    pub const NEW: &str = "paper.new";
    pub const SAVE: &str = "paper.save";
    pub const DOI: &str = "paper.add_by_doi";
    pub const IMPORT: &str = "import.bibliography";
}

pub fn library_commands() -> Vec<Command> {
    use Requirement::{Library, Selection, Ui};
    [
        (
            ids::NEXT,
            "Next paper",
            "Select the next library/search row",
            Library,
        ),
        (
            ids::PREVIOUS,
            "Previous paper",
            "Select the previous library/search row",
            Library,
        ),
        (
            ids::FIRST,
            "First paper",
            "Select the first library/search row",
            Library,
        ),
        (
            ids::LAST,
            "Last paper",
            "Select the last library/search row",
            Library,
        ),
        (
            ids::OPEN,
            "Open paper",
            "Open the selected paper's details",
            Selection,
        ),
        (
            ids::SEARCH,
            "Focus search",
            "Focus the library search field (not full-text search)",
            Ui,
        ),
        (
            ids::COPY,
            "Copy citation",
            "Copy a citation for the selected paper",
            Selection,
        ),
        (
            ids::RESTORE_FOCUS,
            "Restore focus",
            "Request return to the library/search navigation host",
            Ui,
        ),
        (
            ids::PALETTE,
            "Command Palette",
            "Request the command palette",
            Ui,
        ),
        (
            ids::NEW,
            "New paper",
            "Create a manual paper draft",
            Library,
        ),
        (
            ids::SAVE,
            "Save paper",
            "Save the current manual paper draft",
            Library,
        ),
        (
            ids::DOI,
            "Add Paper by DOI",
            "Request DOI preview and explicit import review",
            Library,
        ),
        (
            ids::IMPORT,
            "Import BibTeX/RIS",
            "Request bibliography preview and explicit import review",
            Library,
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
