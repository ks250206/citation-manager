//! `papersd`: on-demand per-library daemon (#25). Normally started by a client's
//! `LocalConnector` (GUI/CLI/MCP/native host); exits when idle. #27: when the persisted
//! API settings enable the loopback REST API, it listens and the daemon stays running.
//!
//! Exit status: 0 idle exit, 75 another daemon already serves the library, 2 usage, 1 error.
//!
//! #26: `papersd token …` and `papersd api …` are the GUI-less bootstrap (see `admin`).
#[cfg(unix)]
fn main() {
    use cm_daemon::ipc::{DaemonConfig, StartError, start};
    use std::{io::IsTerminal, path::PathBuf, time::Duration};
    let all: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if cm_daemon::admin::is_admin(all.first().map(|a| a.as_os_str())) {
        let tty = std::io::stdout().is_terminal();
        std::process::exit(cm_daemon::admin::run(
            &all,
            tty,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        ));
    }
    let mut args = all.into_iter();
    let mut library = None;
    let mut runtime_dir = None;
    let mut settings = None;
    let mut idle = None;
    let mut origins = Vec::new();
    let usage = || -> ! {
        eprintln!(
            "Usage: papersd --library /absolute/library [--runtime-dir /absolute/private-dir] \
             [--settings /absolute/ui.json] [--idle-timeout-ms N] \
             [--api-allow-origin scheme://host[:port]]..."
        );
        std::process::exit(2);
    };
    while let Some(flag) = args.next() {
        let Some(value) = args.next() else { usage() };
        let path = PathBuf::from(&value);
        match flag.to_str() {
            Some("--library") if library.is_none() && path.is_absolute() => library = Some(path),
            Some("--runtime-dir") if runtime_dir.is_none() && path.is_absolute() => {
                runtime_dir = Some(path)
            }
            Some("--settings") if settings.is_none() && path.is_absolute() => settings = Some(path),
            Some("--api-allow-origin") => match value.to_str() {
                Some(origin) if cm_daemon::rest::valid_origin(origin) => {
                    origins.push(origin.to_owned())
                }
                _ => usage(),
            },
            Some("--idle-timeout-ms") if idle.is_none() => {
                idle = Some(
                    value
                        .to_str()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or_else(|| usage()),
                )
            }
            _ => usage(),
        }
    }
    let Some(library) = library else { usage() };
    let mut config = DaemonConfig::new(library);
    config.runtime_dir = runtime_dir;
    config.settings = settings;
    config.rest.allowed_origins = origins;
    if let Some(ms) = idle {
        config.idle_timeout = Duration::from_millis(ms);
    }
    // #18: index PDF page text when this installation bundles the pinned PDFium library.
    #[cfg(feature = "pdf-metadata")]
    if let Some(extractor) = cm_daemon::fulltext::PdfiumExtractor::bundled() {
        config.extractor = Some(std::sync::Arc::new(extractor));
    }
    match start(config) {
        Ok(daemon) => daemon.wait(),
        Err(StartError::AlreadyRunning) => {
            std::process::exit(cm_daemon::lifecycle::EXIT_ALREADY_RUNNING)
        }
        Err(e) => {
            eprintln!("papersd: {e}");
            std::process::exit(1);
        }
    }
}
#[cfg(not(unix))]
fn main() {
    eprintln!("papersd requires a Unix platform");
    std::process::exit(1);
}
