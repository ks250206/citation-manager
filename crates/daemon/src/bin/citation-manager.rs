//! Optional local macOS/Linux GUI composition. Since #25 the GUI owns no database: it talks
//! to the library's `papersd` (started on demand from next to this executable) through the
//! authenticated `IpcLibraryClient`. The open connection keeps the daemon alive while the GUI
//! runs. No HTTP listener is started.
//!
//! #34: inside `Citation Manager.app` a launch without arguments (Finder, Dock, `open`) uses
//! the per-user library under `~/Library/Application Support/Citation Manager`, and every
//! bundle launch re-points the browser host manifests (`papers-native-host refresh`). See
//! `cm_daemon::app_bundle`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() {
    use cm_daemon::{
        app_bundle,
        lifecycle::{DaemonPaths, Launch, euid, local_connector},
    };
    use cm_protocol::client::IpcLibraryClient;
    use std::{path::PathBuf, sync::Arc, time::Duration};
    let fail = |error: &dyn std::fmt::Display| -> ! {
        eprintln!("Cannot prepare library service: {error}");
        std::process::exit(1);
    };
    // Canonical: siblings and the bundle are found from the real file, never from cwd/PATH.
    let exe = app_bundle::own_executable().unwrap_or_else(|e| fail(&e));
    let in_bundle = app_bundle::bundle_contents(&exe).is_some();
    let arguments = app_bundle::launch_arguments(std::env::args_os().skip(1));
    let bundle_default = in_bundle && arguments.is_empty();
    let mut args = arguments.into_iter();
    let mut path = None;
    let mut settings = None;
    while let Some(flag) = args.next() {
        let Some(value) = args.next().map(PathBuf::from).filter(|p| p.is_absolute()) else {
            eprintln!("Paths must be explicit and absolute");
            std::process::exit(2);
        };
        if flag == "--library" && path.is_none() {
            path = Some(value);
        } else if flag == "--settings" && settings.is_none() {
            settings = Some(value);
        } else {
            eprintln!("Unknown or duplicate option");
            std::process::exit(2);
        }
    }
    if path.is_none() && bundle_default {
        path = Some(
            app_bundle::prepare_default_library(std::env::var_os("HOME").as_deref())
                .unwrap_or_else(|e| fail(&e)),
        );
    }
    let Some(path) = path else {
        eprintln!(
            "Usage: citation-manager --library /absolute/private/library [--settings /absolute/private/ui.json]"
        );
        std::process::exit(2);
    };
    if in_bundle && let Some(mut refresh) = app_bundle::refresh_native_host(&exe) {
        // Reaped in the background; a failed refresh never blocks the app.
        let _ = std::thread::Builder::new()
            .name("native-host-refresh".into())
            .spawn(move || refresh.wait());
    }
    let paths = DaemonPaths::new(&path, None, euid()).unwrap_or_else(|e| fail(&e));
    // Same directory as this executable (.app MacOS/ or <prefix>/bin); never a PATH search.
    let papersd = exe.with_file_name("papersd");
    let mut connector = local_connector(paths, "desktop");
    let mut extra_args = Vec::new();
    if let Some(settings) = settings {
        extra_args.push("--settings".into());
        extra_args.push(settings.into_os_string());
    }
    connector.launch = Some(Launch {
        papersd,
        extra_args,
        timeout: Duration::from_secs(15),
    });
    let client = IpcLibraryClient::new(connector).unwrap_or_else(|e| fail(&e));
    client.connect_in_background();
    cm_desktop::run(Arc::new(client));
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("desktop-bootstrap supports macOS and Linux");
