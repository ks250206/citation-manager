//! Neutral reusable import policies and BibTeX/RIS adapters. DOI adapters belong to #14.
//! The implementation lives in core so storage can reuse it without reverse edges.
pub use cm_core::import::{citation_keys, dedup, file_metadata, normalize};

pub mod bibtex;
pub mod exchange;
pub mod ris;
