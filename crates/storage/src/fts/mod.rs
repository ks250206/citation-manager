//! #18 full-text index: the trigger-maintained `search_documents` projection, the
//! unicode61/trigram FTS5 indexes over it, page text storage and extraction state.
//! All SQL stays here; ranking/merging/snippets are in `cm_core::search::service`.
mod extraction;
#[cfg(test)]
mod measure;
mod rebuild;
mod search;
#[cfg(test)]
mod tests;
pub use extraction::SqliteFulltext;
pub use rebuild::IndexHealth;
pub use search::SqliteSearch;

use crate::{LibraryDb, LibraryDirectory, StorageError};

impl LibraryDb {
    /// Read-side search port (`cm_core::search::SearchIndex`), rebuild and verification.
    pub fn search_index(&self) -> SqliteSearch<'_> {
        SqliteSearch {
            connection: &self.connection,
        }
    }
    /// Extraction store. Rejects pairing a DB with another library's file root.
    pub fn fulltext(
        &mut self,
        directory: &LibraryDirectory,
    ) -> Result<SqliteFulltext<'_>, StorageError> {
        self.check_directory(directory)?;
        Ok(SqliteFulltext {
            connection: &mut self.connection,
            directory: directory.clone(),
        })
    }
    /// Read-only extraction status/page access (no file root needed).
    pub fn fulltext_reader(&self) -> extraction::FulltextReader<'_> {
        extraction::FulltextReader {
            connection: &self.connection,
        }
    }
}

/// `has:pdf` metadata predicate shared with M1 `LibraryQuery` (alias `a`).
pub(crate) const PDF_ATTACHMENT: &str = "a.kind='pdf' AND lower(trim(substr(a.mime_type,1,instr(a.mime_type||';',';')-1),char(9)||' '))='application/pdf'";
