//! PDF boundary. Runtime loading, ABI, rendering, and fixtures belong to #11.
//! This optional re-export checks bindings compilation without loading PDFium.
#[cfg(feature = "bindings-check")]
pub use pdfium_render::prelude::Pdfium;
