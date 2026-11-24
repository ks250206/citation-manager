//! Desktop views depend only on the replaceable protocol client.
pub mod commands;
pub mod deep_links;
pub mod keymap;
pub mod palette;
pub mod pdf_viewer;
#[cfg(any(
    test,
    all(feature = "native-gpui", any(target_os = "macos", target_os = "linux"))
))]
mod request_policy;
pub mod search;
pub mod service_client;
#[cfg(all(
    feature = "native-gpui",
    not(any(target_os = "macos", target_os = "linux"))
))]
compile_error!("native-gpui supports macOS and Linux; use the default contract lane elsewhere");
#[cfg(all(feature = "native-gpui", any(target_os = "macos", target_os = "linux")))]
mod native;
#[cfg(all(feature = "native-gpui", any(target_os = "macos", target_os = "linux")))]
pub use native::run;

pub mod annotations;
pub mod attachments;
pub mod details;
pub mod import_dialogs;
pub mod library;
pub mod settings;
