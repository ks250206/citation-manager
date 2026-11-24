//! Search contracts (#18): bounded DSL parser (#46), transport-neutral DTOs, the search
//! orchestration over a storage port, and the page text extraction contract.
pub mod extraction;
pub mod query;
pub mod service;
pub mod types;
pub use service::{SearchIndex, search};
pub use types::*;
