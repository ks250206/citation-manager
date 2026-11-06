//! Neutral reusable import policies. Concrete BibTeX/RIS/DOI adapters are #13/#14.
//! The implementation lives in core so storage can reuse it without reverse edges.
pub use cm_core::import::{citation_keys, dedup, file_metadata, normalize};
