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
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            columns: [("Title", 300), ("Year", 65), ("Type", 90), ("DOI", 180)]
                .into_iter()
                .map(|(name, width)| Column {
                    name: name.into(),
                    visible: true,
                    width,
                })
                .collect(),
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
            && self.columns.len() == 4
            && self
                .columns
                .iter()
                .zip(["Title", "Year", "Type", "DOI"])
                .all(|(c, name)| c.name == name && (50..=800).contains(&c.width))
            && self.columns[0].visible
            && self.recent_commands.len() <= MAX_RECENT_COMMANDS
            && self.recent_commands.iter().all(|id| {
                (3..=96).contains(&id.len())
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._".contains(&b))
            })
    }
}
#[derive(Debug, Clone)]
pub struct SettingsLoaded {
    pub settings: Settings,
    pub warning: Option<String>,
    /// Raw user keymap document (`keymap.json` beside the settings file), if one exists.
    /// The desktop validates it as a whole and falls back to defaults with diagnostics.
    pub keymap: Option<Vec<u8>>,
    /// Where a user keymap is read from, for display; present even when no file exists.
    pub keymap_path: Option<std::path::PathBuf>,
}
