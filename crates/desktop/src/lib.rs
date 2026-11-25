//! Desktop views depend only on the replaceable protocol client.
/// #84 actions on several selected papers, sent one request at a time.
pub mod bulk;
/// #84 library table columns, header sorting and cell text.
pub mod columns;
pub mod commands;
pub mod deep_links;
pub mod keymap;
/// #84 popup menus (row context menu, ＋ Add, settings) and their keyboard navigation.
pub mod menu;
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
pub mod api_settings;
pub mod attachments;
pub mod details;
pub mod import_dialogs;
pub mod library;
/// #20 Save Current PDF: request/status logic shared by the native host and tests.
pub mod pdf_export;
pub mod settings;
