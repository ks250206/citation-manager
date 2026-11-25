//! Shared import policies, BibTeX/RIS exchange, read-only DOI providers and (#22) browser
//! capture extraction plus the safe public PDF fetcher.
//! The implementation lives in core so storage can reuse it without reverse edges.
pub use cm_core::import::{citation_keys, dedup, file_metadata, normalize};

pub mod bibtex;
pub mod capture;
pub mod doi;
pub mod exchange;
pub mod fetch_policy;
pub mod html;
pub mod providers;
pub mod ris;
