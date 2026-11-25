//! Service composition root and `papersd` (#25): the single library writer behind
//! authenticated local IPC, plus the optional loopback REST API (#27, off by default).
/// #26 CLI bootstrap (`papersd token …`, `papersd api …`).
pub mod admin;
/// #26 tokens, MCP profiles, elevation and local API settings.
pub mod api;
/// #26 bounded audit log.
pub mod audit;
pub mod desktop_worker;
pub mod fulltext;
pub mod ipc;
pub mod jobs;
pub mod lifecycle;
/// #27 local REST API listener (disabled unless the API settings enable it).
pub mod rest;
