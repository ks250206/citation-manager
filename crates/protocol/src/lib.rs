//! Shared transport boundary. Feature PRs add their own modules here.
//! `ipc` (envelope/handshake/framing), `jobs` (JobStatus) and `client` are #25;
//! command IDs are #16.
//! Import DTOs remain owned by core and independent of the wire format.
/// Shared PDF page geometry (unrotated points ↔ display pixels).
pub use cm_core::geometry;
pub use cm_core::import;

// Let clients name shared values without depending on infrastructure.
pub use cm_domain as domain;

pub mod library;

pub mod annotations;
/// #26 token/grant management and local API settings.
pub mod api;
pub mod client;
pub mod ipc;
pub mod jobs;

pub mod attachments;
pub mod exchange;
pub mod organization;
pub mod search;
pub mod settings;
pub mod library_query {
    pub use cm_core::library_query::*;
    pub use cm_core::papers::PaperVisibility;
}
