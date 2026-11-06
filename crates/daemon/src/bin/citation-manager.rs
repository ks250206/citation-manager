//! Optional local macOS composition root. No HTTP/IPC listener is started.
#[cfg(target_os = "macos")]
fn main() {
    use cm_daemon::desktop_worker::InProcessLibraryClient;
    use std::{path::PathBuf, sync::Arc};
    let mut args = std::env::args_os().skip(1);
    // Explicit path keeps the bootstrap from accidentally opening a real user library.
    let path = match (args.next(), args.next(), args.next()) {
        (Some(flag), Some(path), None)
            if flag == "--library" && PathBuf::from(&path).is_absolute() =>
        {
            PathBuf::from(path)
        }
        _ => {
            eprintln!("Usage: citation-manager --library /absolute/path/to/dedicated-library");
            std::process::exit(2);
        }
    };
    let client = InProcessLibraryClient::start(path).unwrap_or_else(|error| {
        eprintln!("Cannot start library worker: {error}");
        std::process::exit(1);
    });
    cm_desktop::run(Arc::new(client));
}
#[cfg(not(target_os = "macos"))]
compile_error!("desktop-bootstrap currently requires macOS");
