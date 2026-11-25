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
    /// #84: RIS has its own entry next to BibTeX (the ＋ Add menu and the palette).
    pub const IMPORT_RIS: &str = "import.ris";
    /// #84 the ＋ Add menu (new paper, DOI, PDFs, BibTeX, RIS).
    pub const ADD_MENU: &str = "add.menu";
    /// #84 row context menu for the selection (Shift+F10 / Menu key).
    pub const ACTIONS: &str = "paper.actions";
    pub const SELECT_ALL: &str = "library.select_all";
    pub const COLUMNS: &str = "library.columns";
    pub const SORT_AUTHOR: &str = "library.sort_author";
    pub const SORT_TITLE: &str = "library.sort_title";
    pub const SORT_YEAR: &str = "library.sort_year";
    pub const SORT_JOURNAL: &str = "library.sort_journal";
    pub const SORT_ADDED: &str = "library.sort_added";
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
            "Import BibTeX…",
            "Choose a .bib file, then review the preview before anything is saved",
            Library,
        ),
        (
            ids::IMPORT_RIS,
            "Import RIS…",
            "Choose a .ris file, then review the preview before anything is saved",
            Library,
        ),
        (
            ids::ADD_MENU,
            "Add…",
            "New paper, DOI, PDFs or a BibTeX/RIS file (the ＋ Add menu)",
            Ui,
        ),
        (
            ids::ACTIONS,
            "Paper Actions…",
            "Open, copy citation, collections, tags and Trash for the selected papers",
            Selection,
        ),
        (
            ids::SELECT_ALL,
            "Select All Papers",
            "Select every paper on the loaded page",
            Library,
        ),
        (
            ids::COLUMNS,
            "Table Columns…",
            "Show or hide Author, Journal, PDF, Type, DOI and Added columns",
            Ui,
        ),
        (
            ids::SORT_AUTHOR,
            "Sort by First Author",
            "Sort the library by first author (again: reverse)",
            Library,
        ),
        (
            ids::SORT_TITLE,
            "Sort by Title",
            "Sort the library by title (again: reverse)",
            Library,
        ),
        (
            ids::SORT_YEAR,
            "Sort by Year",
            "Sort the library by year (again: reverse)",
            Library,
        ),
        (
            ids::SORT_JOURNAL,
            "Sort by Journal",
            "Sort the library by journal (again: reverse)",
            Library,
        ),
        (
            ids::SORT_ADDED,
            "Sort by Date Added",
            "Newest papers first (again: oldest first)",
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
