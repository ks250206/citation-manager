//! Private UI preferences. Persistence belongs to the worker, not the GUI.
pub use cm_core::library_query::{LibrarySort, SortDirection, SortField};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub name: String,
    pub visible: bool,
    pub width: u16,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u8,
    pub columns: Vec<Column>,
    pub sort: LibrarySort,
    pub navigation_width: u16,
    pub details_width: u16,
    pub reading: bool,
    pub details_tab: String,
    /// Vim-style Normal mode for the library table (j/k/gg/G/y and /). Off = plain keys only.
    #[serde(default = "enabled")]
    pub vim: bool,
    /// Most-recent-first Command Palette history (command IDs only, no arguments).
    #[serde(default)]
    pub recent_commands: Vec<String>,
}
fn enabled() -> bool {
    true
}
/// Upper bound for `recent_commands`, matching the palette's history length.
pub const MAX_RECENT_COMMANDS: usize = 16;
/// Library table columns in display order: (name, default width, visible by default).
/// Title must stay visible so rows remain identifiable.
pub const COLUMNS: [(&str, u16, bool); 8] = [
    ("Author", 160, true),
    ("Title", 300, true),
    ("Year", 60, true),
    ("Journal", 170, true),
    ("PDF", 50, true),
    ("Type", 110, false),
    ("DOI", 180, false),
    ("Added", 100, false),
];
/// The column layout written before #84 (Title/Year/Type/DOI, all visible by default).
const LEGACY_COLUMNS: [&str; 4] = ["Title", "Year", "Type", "DOI"];
fn default_columns() -> Vec<Column> {
    COLUMNS
        .into_iter()
        .map(|(name, width, visible)| Column {
            name: name.into(),
            visible,
            width,
        })
        .collect()
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            columns: default_columns(),
            sort: LibrarySort::default(),
            navigation_width: 220,
            details_width: 380,
            reading: false,
            details_tab: "Info".into(),
            vim: true,
            recent_commands: vec![],
        }
    }
}
impl Settings {
    pub fn valid(&self) -> bool {
        self.version == 1
            && (160..=360).contains(&self.navigation_width)
            && (300..=700).contains(&self.details_width)
            && ["Info", "Notes", "PDF"].contains(&self.details_tab.as_str())
            && self.columns.len() == COLUMNS.len()
            && self
                .columns
                .iter()
                .zip(COLUMNS)
                .all(|(c, (name, _, _))| c.name == name && (50..=800).contains(&c.width))
            && self.columns.iter().any(|c| c.name == "Title" && c.visible)
            && self.recent_commands.len() <= MAX_RECENT_COMMANDS
            && self.recent_commands.iter().all(|id| {
                (3..=96).contains(&id.len())
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._".contains(&b))
            })
    }
    /// Bring a settings file written by an older version to the current column layout.
    /// The pre-#84 four-column table keeps its widths; visibility follows the new
    /// defaults (Author/Title/Year/Journal/PDF shown, Type/DOI/Added via the column
    /// toggles), since the old file never chose anything for columns that did not exist.
    /// Any other layout is left as it is and still has to pass `valid`.
    pub fn upgrade(&mut self) {
        let legacy = self.columns.len() == LEGACY_COLUMNS.len()
            && self
                .columns
                .iter()
                .zip(LEGACY_COLUMNS)
                .all(|(c, name)| c.name == name);
        if !legacy {
            return;
        }
        let old = std::mem::replace(&mut self.columns, default_columns());
        for column in &mut self.columns {
            if let Some(previous) = old.iter().find(|c| c.name == column.name) {
                column.width = previous.width;
            }
        }
    }
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SettingsLoaded {
    pub settings: Settings,
    pub warning: Option<String>,
    /// Raw user keymap document (`keymap.json` beside the settings file), if one exists.
    /// The desktop validates it as a whole and falls back to defaults with diagnostics.
    pub keymap: Option<Vec<u8>>,
    /// Where a user keymap is read from, for display; present even when no file exists.
    pub keymap_path: Option<std::path::PathBuf>,
}
