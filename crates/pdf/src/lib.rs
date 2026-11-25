//! PDF-only boundary. No storage or bibliographic mutation. Runtime is opt-in.
/// Re-exported so existing `cm_pdf::geometry` paths keep working; owned by `cm_core`.
pub use cm_core::geometry;
#[cfg(feature = "bindings-check")]
pub use pdfium_render::prelude::Pdfium;
#[cfg(feature = "runtime")]
mod adapter;
/// The crate's only `unsafe`: PDFium appearance/name setters for exported annotations (#20).
#[cfg(feature = "runtime")]
mod appearance;
#[cfg(feature = "runtime")]
pub mod extract;
#[cfg(feature = "runtime")]
pub mod worker;

#[cfg(feature = "runtime")]
mod metadata;
