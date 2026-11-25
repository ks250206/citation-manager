//! PDF-only boundary. No storage or bibliographic mutation. Runtime is opt-in.
pub mod geometry;
#[cfg(feature = "bindings-check")]
pub use pdfium_render::prelude::Pdfium;
#[cfg(feature = "runtime")]
mod adapter;
#[cfg(feature = "runtime")]
pub mod worker;

#[cfg(feature = "runtime")]
mod metadata;
