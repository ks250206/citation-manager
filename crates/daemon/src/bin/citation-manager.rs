//! Optional local macOS composition root. No HTTP/IPC listener is started.
#[cfg(target_os = "macos")]
fn main() {
    use cm_daemon::desktop_worker::InProcessLibraryClient;
    use std::{path::PathBuf, sync::Arc};
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
    let client =
        InProcessLibraryClient::start_with_settings(path, settings).unwrap_or_else(|error| {
            eprintln!("Cannot start library worker: {error}");
            std::process::exit(1);
        });
    cm_desktop::run(Arc::new(client));
}
#[cfg(not(target_os = "macos"))]
compile_error!("desktop-bootstrap currently requires macOS");
