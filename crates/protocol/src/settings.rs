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
}
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
    }
}
#[derive(Debug, Clone)]
pub struct SettingsLoaded {
    pub settings: Settings,
    pub warning: Option<String>,
}
