use super::{Command, Requirement};

/// #21: PDF navigation/search/annotation commands and cross search. `pdf.save` is #20's
/// (`commands::pdf_export`); API settings stay with #26.
pub mod ids {
    pub const NEXT_PAGE: &str = "pdf.next_page";
    pub const PREVIOUS_PAGE: &str = "pdf.previous_page";
    pub const FIRST_PAGE: &str = "pdf.first_page";
    pub const LAST_PAGE: &str = "pdf.last_page";
    pub const FIND: &str = "pdf.find";
    pub const FIND_NEXT: &str = "pdf.find_next";
    pub const FIND_PREVIOUS: &str = "pdf.find_previous";
    pub const ZOOM_IN: &str = "pdf.zoom_in";
    pub const ZOOM_OUT: &str = "pdf.zoom_out";
    pub const FIT_WIDTH: &str = "pdf.fit_width";
    pub const ROTATE: &str = "pdf.rotate";
    pub const HIGHLIGHT: &str = "pdf.highlight_selection";
    pub const ADD_NOTE: &str = "pdf.add_note";
    pub const SHOW_ANNOTATIONS: &str = "pdf.show_annotations";
    pub const FULL_TEXT: &str = "search.full_text";
    pub const COPY_LINK: &str = "link.copy";
    pub const OPEN_LINK: &str = "link.open_clipboard";
}

pub fn pdf_commands() -> Vec<Command> {
    use Requirement::{Library, Selection};
    [
        (
            ids::NEXT_PAGE,
            "Next Page",
            "Show the next PDF page",
            Selection,
        ),
        (
            ids::PREVIOUS_PAGE,
            "Previous Page",
            "Show the previous PDF page",
            Selection,
        ),
        (
            ids::FIRST_PAGE,
            "First Page",
            "Show the first PDF page",
            Selection,
        ),
        (
            ids::LAST_PAGE,
            "Last Page",
            "Show the last PDF page",
            Selection,
        ),
        (
            ids::FIND,
            "Find in PDF",
            "Search the open PDF's text",
            Selection,
        ),
        (
            ids::FIND_NEXT,
            "Find Next",
            "Next match in the open PDF",
            Selection,
        ),
        (
            ids::FIND_PREVIOUS,
            "Find Previous",
            "Previous match in the open PDF",
            Selection,
        ),
        (ids::ZOOM_IN, "Zoom In", "Enlarge the PDF", Selection),
        (ids::ZOOM_OUT, "Zoom Out", "Shrink the PDF", Selection),
        (
            ids::FIT_WIDTH,
            "Fit Width",
            "Fit the PDF to the pane width",
            Selection,
        ),
        (
            ids::ROTATE,
            "Rotate Pages",
            "Rotate PDF pages clockwise",
            Selection,
        ),
        (
            ids::HIGHLIGHT,
            "Highlight Selection",
            "Highlight the selected PDF text",
            Selection,
        ),
        (
            ids::ADD_NOTE,
            "Add Page Note",
            "Add a note to the current PDF page",
            Selection,
        ),
        (
            ids::SHOW_ANNOTATIONS,
            "Show Annotations",
            "List highlights and notes of the selected paper",
            Selection,
        ),
        (
            ids::FULL_TEXT,
            "Search Full Text",
            "Search titles, abstracts, annotations and PDF text",
            Library,
        ),
        (
            ids::COPY_LINK,
            "Copy Link to Paper/Page",
            "Copy a paper:// link to the paper or the current PDF page",
            Selection,
        ),
        (
            ids::OPEN_LINK,
            "Open paper:// Link from Clipboard",
            "Jump to the paper, page or annotation a copied paper:// link points to",
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
