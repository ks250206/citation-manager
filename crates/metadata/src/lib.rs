//! Shared import policies and read-only DOI providers (#14). BibTeX/RIS are #13.
//! The implementation lives in core so storage can reuse it without reverse edges.
pub use cm_core::import::{citation_keys, dedup, file_metadata, normalize};

pub mod doi;
pub mod providers;
