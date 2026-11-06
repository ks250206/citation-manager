//! Shared transport boundary. Feature PRs add their own modules here.
//! IPC envelopes, authentication, and JobStatus belong to #25; command IDs to #16.
//! Import DTOs remain owned by core and independent of the wire format.
pub use cm_core::import;

// Let clients name shared values without depending on infrastructure.
pub use cm_domain as domain;

pub mod library;

pub mod attachments;
pub mod exchange;
pub mod organization;
pub mod settings;
pub mod library_query {
    pub use cm_core::library_query::*;
    pub use cm_core::papers::PaperVisibility;
}
