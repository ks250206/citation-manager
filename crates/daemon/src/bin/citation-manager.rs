//! Optional local macOS/Linux GUI composition. Since #25 the GUI owns no database: it talks
//! to the library's `papersd` (started on demand from next to this executable) through the
//! authenticated `IpcLibraryClient`. The open connection keeps the daemon alive while the GUI
//! runs. No HTTP listener is started.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() {
    use cm_daemon::lifecycle::{DaemonPaths, Launch, euid, local_connector};
    use cm_protocol::client::IpcLibraryClient;
    use std::{path::PathBuf, sync::Arc, time::Duration};
    let mut args = std::env::args_os().skip(1);
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
    let Some(path) = path else {
        eprintln!(
            "Usage: citation-manager --library /absolute/private/library [--settings /absolute/private/ui.json]"
        );
        std::process::exit(2);
    };
    let fail = |error: &dyn std::fmt::Display| -> ! {
        eprintln!("Cannot prepare library service: {error}");
        std::process::exit(1);
    };
    let paths = DaemonPaths::new(&path, None, euid()).unwrap_or_else(|e| fail(&e));
    // Same directory as this executable (.app MacOS/ or <prefix>/bin); never a PATH search.
    let papersd = std::env::current_exe()
        .map(|exe| exe.with_file_name("papersd"))
        .unwrap_or_else(|e| fail(&e));
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
