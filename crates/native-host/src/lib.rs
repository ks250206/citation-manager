//! #23 browser Native Messaging host (`papers-native-host`).
//!
//! A small, short-lived process the browser starts for the capture extension. It validates
//! the caller and every message, maps a closed set of message types to #22 capture /
//! destination commands, and forwards them over the #25 IPC with its own daemon-issued
//! capture grant, starting `papersd` from its own directory when needed. It never carries
//! PDF bytes (the daemon fetches public PDFs itself) and is not a generic IPC proxy.
//! Contract: docs/ADR/native-messaging.md.
#[cfg(unix)]
pub mod caller;
#[cfg(unix)]
pub mod cli;
#[cfg(unix)]
pub mod config;
#[cfg(unix)]
pub mod daemon;
pub mod frame;
#[cfg(unix)]
pub mod host;
#[cfg(unix)]
pub mod manifest;
#[cfg(unix)]
pub mod message;
