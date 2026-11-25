//! Desktop views depend only on the replaceable protocol client.
pub mod commands;
pub mod keymap;
#[cfg(any(test, all(feature = "native-gpui", target_os = "macos")))]
mod request_policy;
pub mod service_client;
#[cfg(all(feature = "native-gpui", not(target_os = "macos")))]
compile_error!("native-gpui currently requires macOS; use the default contract lane elsewhere");
#[cfg(all(feature = "native-gpui", target_os = "macos"))]
mod native;
#[cfg(all(feature = "native-gpui", target_os = "macos"))]
pub use native::run;

pub mod attachments;
pub mod details;
pub mod import_dialogs;
pub mod library;
pub mod settings;
