//! #84 library table columns: what each column shows, which ones sort on the daemon, and
//! how a header click changes the sort. Framework-free so the rules are unit-tested.
use cm_protocol::{
    domain::{Bibliography, Contributor, Paper},
    library_query::{LibrarySort, SortDirection, SortField},
    settings::Column,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Author,
    Title,
    Year,
    Journal,
    Pdf,
    Type,
    Doi,
    Added,
}

impl ColumnKind {
    /// Same order and names as `cm_protocol::settings::COLUMNS`.
    pub const ALL: [ColumnKind; 8] = [
        Self::Author,
        Self::Title,
        Self::Year,
        Self::Journal,
        Self::Pdf,
        Self::Type,
        Self::Doi,
        Self::Added,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Author => "Author",
            Self::Title => "Title",
            Self::Year => "Year",
            Self::Journal => "Journal",
            Self::Pdf => "PDF",
            Self::Type => "Type",
            Self::Doi => "DOI",
            Self::Added => "Added",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
    /// The daemon sort key behind a header click. PDF, Type and DOI have none: sorting
    /// them would need keys the keyset cursor does not carry.
    pub fn sort_field(self) -> Option<SortField> {
        match self {
            Self::Author => Some(SortField::Author),
            Self::Title => Some(SortField::Title),
            Self::Year => Some(SortField::Year),
            Self::Journal => Some(SortField::Journal),
            Self::Added => Some(SortField::CreatedAt),
            Self::Pdf | Self::Type | Self::Doi => None,
        }
    }
    /// Title takes the spare width; the others keep their configured width.
    pub fn flexible(self) -> bool {
        self == Self::Title
    }
    /// Narrowest width a text column gives way to when the pane is too narrow for the
    /// configured widths (Title first); None keeps the configured width (Year, PDF, Added).
    pub fn min_width(self) -> Option<u16> {
        match self {
            Self::Title => Some(120),
            Self::Author | Self::Journal | Self::Type => Some(70),
            Self::Doi => Some(100),
            Self::Year | Self::Pdf | Self::Added => None,
        }
    }
    /// Title cannot be hidden so rows stay identifiable.
    pub fn can_hide(self) -> bool {
        self != Self::Title
    }
    /// Cell text. `has_pdf` comes from the page (`LibraryPage::with_pdf`).
    pub fn cell(self, paper: &Paper, has_pdf: bool) -> String {
        let b = &paper.bibliography;
        match self {
            Self::Author => author_label(b),
            Self::Title => paper.title.replace(['\r', '\n'], " "),
            Self::Year => b.year.map_or_else(|| "—".into(), |y| y.to_string()),
            Self::Journal => b.journal.clone().unwrap_or_default(),
            Self::Pdf => if has_pdf { "📄" } else { "" }.into(),
            Self::Type => b.paper_type.clone().unwrap_or_default(),
            Self::Doi => b.doi.clone().unwrap_or_default(),
            // RFC 3339 timestamps: the date part is enough for a list.
            Self::Added => paper
                .created_at
                .as_deref()
                .map(|t| t.chars().take(10).collect())
                .unwrap_or_default(),
        }
    }
}

/// Columns shown, in order, with their widths. Unknown names (never valid settings) are
/// skipped rather than shown blank.
pub fn visible(columns: &[Column]) -> Vec<(ColumnKind, u16)> {
    columns
        .iter()
        .filter(|c| c.visible)
        .filter_map(|c| ColumnKind::from_name(&c.name).map(|k| (k, c.width)))
        .collect()
}

/// Show or hide a column. Title always stays visible; returns whether anything changed.
pub fn toggle(columns: &mut [Column], kind: ColumnKind) -> bool {
    if !kind.can_hide() {
        return false;
    }
    match columns.iter_mut().find(|c| c.name == kind.name()) {
        Some(column) => {
            column.visible = !column.visible;
            true
        }
        None => false,
    }
}

/// Header click: the same field flips direction; another field starts ascending, except
/// the added date, which starts with the newest papers.
pub fn next_sort(current: LibrarySort, field: SortField) -> LibrarySort {
    if current.field == field {
        LibrarySort {
            field,
            direction: match current.direction {
                SortDirection::Asc => SortDirection::Desc,
                SortDirection::Desc => SortDirection::Asc,
            },
        }
    } else {
        LibrarySort {
            field,
            direction: if field == SortField::CreatedAt {
                SortDirection::Desc
            } else {
                SortDirection::Asc
            },
        }
    }
}

/// Header arrow for a column under the current sort.
pub fn sort_arrow(kind: ColumnKind, sort: LibrarySort) -> &'static str {
    match (kind.sort_field(), sort.direction) {
        (Some(f), SortDirection::Asc) if f == sort.field => " ▲",
        (Some(f), SortDirection::Desc) if f == sort.field => " ▼",
        _ => "",
    }
}

/// Human name of a sort, for the status line and the sort menu.
pub fn sort_label(sort: LibrarySort) -> String {
    let field = match sort.field {
        SortField::Id => "Default order",
        SortField::Title => "Title",
        SortField::Year => "Year",
        SortField::CreatedAt => "Date added",
        SortField::UpdatedAt => "Date modified",
        SortField::Author => "First author",
        SortField::Journal => "Journal",
    };
    if sort.field == SortField::Id {
        return field.into();
    }
    let direction = match (sort.field, sort.direction) {
        (SortField::Year | SortField::CreatedAt | SortField::UpdatedAt, SortDirection::Asc) => {
            "oldest first"
        }
        (SortField::Year | SortField::CreatedAt | SortField::UpdatedAt, SortDirection::Desc) => {
            "newest first"
        }
        (_, SortDirection::Asc) => "A→Z",
        (_, SortDirection::Desc) => "Z→A",
    };
    format!("{field}, {direction}")
}

fn contributor_name(c: &Contributor) -> Option<String> {
    // Same preference as the daemon's author sort key: family, literal, then given name.
    [&c.family_name, &c.literal_name, &c.given_name]
        .into_iter()
        .flatten()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

/// "Smith" for one author, "Smith et al." for more. Only contributors with the author
/// role count (editors and translators do not); no authors shows an empty cell.
pub fn author_label(b: &Bibliography) -> String {
    let authors: Vec<&Contributor> = b
        .authors
        .iter()
        .filter(|a| a.role.is_empty() || a.role == "author")
        .collect();
    let Some(first) = authors.first().and_then(|a| contributor_name(a)) else {
        return String::new();
    };
    if authors.len() > 1 {
        format!("{first} et al.")
    } else {
        first
    }
}
