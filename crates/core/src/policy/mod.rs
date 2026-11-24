//! Transport-neutral authorization policy (#25). Grants are resolved and stored by the daemon.
pub mod authorizer;
pub mod capabilities;
pub use authorizer::{Denial, Grant, GrantId, authorize, check_live, effective, may_issue};
pub use capabilities::{Capability, CapabilitySet, Operation, Requirement, presets};
