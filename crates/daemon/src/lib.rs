//! Service composition root. HTTP/IPC lifecycle remains owned by #25.
#[cfg(all(target_os = "macos", feature = "layout-smoke"))]
pub mod app_layout;
pub mod desktop_worker;
