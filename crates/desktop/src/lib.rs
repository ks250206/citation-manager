//! Desktop views depend only on the replaceable protocol client.
pub mod commands;
pub mod keymap;
pub mod service_client;
#[cfg(all(feature = "native-gpui", not(target_os = "macos")))]
compile_error!("native-gpui currently requires macOS; use the default contract lane elsewhere");
#[cfg(all(feature = "native-gpui", target_os = "macos"))]
mod native;
#[cfg(all(feature = "native-gpui", target_os = "macos"))]
pub use native::run;
