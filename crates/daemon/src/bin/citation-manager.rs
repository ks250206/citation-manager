//! Optional local macOS composition root. No HTTP/IPC listener is started.
#[cfg(target_os = "macos")]
fn main() {
    use cm_daemon::desktop_worker::InProcessLibraryClient;
    use std::{path::PathBuf, sync::Arc};
    let mut args = std::env::args_os().skip(1);
    #[cfg(feature = "layout-smoke")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == "--layout-smoke")
    {
        let result = match (args.next(), args.next(), args.next()) {
            (Some(_), Some(out), None) => cm_daemon::app_layout::prepare(PathBuf::from(out)),
            _ => Err(
                "Usage: citation-manager --layout-smoke /absolute/private/test-directory".into(),
            ),
        };
        let path = result.unwrap_or_else(|error| {
            eprintln!("Layout smoke failed: {error}");
            std::process::exit(1);
        });
        let client = InProcessLibraryClient::start(path).expect("start synthetic library worker");
        cm_desktop::run(Arc::new(client));
        return;
    }
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
