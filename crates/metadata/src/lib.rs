//! Shared import policies, BibTeX/RIS exchange and read-only DOI providers.
//! The implementation lives in core so storage can reuse it without reverse edges.
pub use cm_core::import::{citation_keys, dedup, file_metadata, normalize};

pub mod bibtex;
pub mod doi;
pub mod exchange;
pub mod providers;
pub mod ris;
