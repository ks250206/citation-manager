//! `papers-native-host`: started by Chrome/Chromium/Firefox through Native Messaging, or run
//! with a setup subcommand (`register`, `pair`, `refresh`, `unregister`, `status`, `manifest`).
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let subcommand = args.first().and_then(|a| a.to_str()).is_some_and(|a| {
        matches!(
            a,
            "register"
                | "pair"
                | "refresh"
                | "unregister"
                | "status"
                | "manifest"
                | "help"
                | "--help"
                | "-h"
        )
    });
    std::process::ExitCode::from(if subcommand {
        cm_native_host::cli::main(&args)
    } else {
        cm_native_host::host::main(&args)
    })
}
#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("papers-native-host: only macOS and Linux are supported");
    std::process::ExitCode::from(1)
}
