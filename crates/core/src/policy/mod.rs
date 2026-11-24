//! Transport-neutral authorization policy (#25). Grants are resolved and stored by the daemon.
//!
//! #26 adds the API/MCP/CLI scope vocabulary and endpoint table (`scopes`), the bulk
//! elevation guard (`elevation`), export destination rules (`export`) and the validated
//! local API listener settings (`api`). Every transport maps onto the same `Operation`s and
//! the daemon's single `authorize` decision; nothing here grants authority by itself.
pub mod api;
pub mod authorizer;
pub mod capabilities;
pub mod elevation;
pub mod export;
pub mod scopes;
pub use authorizer::{
    Denial, Grant, GrantId, authorize, check_live, effective, effective_at, may_issue,
};
pub use capabilities::{Capability, CapabilitySet, Operation, Requirement, presets};
pub use scopes::{ClientKind, Scope, ScopeSet};
