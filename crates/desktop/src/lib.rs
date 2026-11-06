//! GPUI dependency probe only. Window creation and event loop belong to #10.
#[cfg(all(feature = "native-gpui", not(target_os = "macos")))]
compile_error!("native-gpui currently requires macOS; use the default contract lane elsewhere");

/// Compile the actual macOS platform constructor without launching a GUI.
#[cfg(all(feature = "native-gpui", target_os = "macos"))]
pub fn platform_constructor() -> fn() -> gpui::Application {
    gpui_platform::application
}
