//! #34 launch context of the `citation-manager` executable inside `Citation Manager.app`
//! ([app layout ADR](../../../docs/ADR/app-layout.md)).
//!
//! Every path is derived from this executable's own canonical location, never from the
//! current directory, `PATH` or a development checkout:
//!
//! - `Contents/MacOS/{citation-manager,papersd,papers,papers-mcp,papers-native-host}` are
//!   siblings; clients start `papersd` by absolute path.
//! - `Contents/Frameworks/libpdfium.dylib` is resolved by `cm_pdf::worker::bundled_library`.
//! - A Finder/Dock/`open` launch carries no arguments, so inside a bundle (and only there) the
//!   per-user library `~/Library/Application Support/Citation Manager/library` is used. An
//!   explicit `--library` always wins; outside a bundle the path stays mandatory (#10).
//! - Each bundle launch runs `papers-native-host refresh` so browser manifests follow the app
//!   after it was moved or replaced by an update (#23). `refresh` itself is a no-op when the
//!   host was never registered and refuses translocated or read-only locations.
use std::{
    ffi::{OsStr, OsString},
    io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

/// Per-user application directory on macOS, relative to `$HOME`. The browser host keeps its
/// registration in `<this>/native-host/` (#23).
pub const APP_SUPPORT: &str = "Library/Application Support/Citation Manager";
/// Sibling executable that owns browser registration (#23).
pub const NATIVE_HOST: &str = "papers-native-host";

/// This executable with symlinks resolved (macOS `current_exe()` returns the path that was
/// executed, which may be a link).
pub fn own_executable() -> io::Result<PathBuf> {
    std::env::current_exe()?.canonicalize()
}

/// The bundle's `Contents` directory when `executable` is `<Name>.app/Contents/MacOS/<exe>`.
pub fn bundle_contents(executable: &Path) -> Option<&Path> {
    if !executable.is_absolute() {
        return None;
    }
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then_some(contents)
}

/// Arguments without the `-psn_<n>_<n>` process serial number that LaunchServices passed to
/// bundle executables on older macOS releases.
pub fn launch_arguments(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    args.into_iter()
        .filter(|a| !a.to_str().is_some_and(|a| a.starts_with("-psn_")))
        .collect()
}

/// The per-user default library of a bundle launch, from an absolute `$HOME`.
pub fn default_library(home: Option<&OsStr>) -> Option<PathBuf> {
    let home = Path::new(home?);
    home.is_absolute()
        .then(|| home.join(APP_SUPPORT).join("library"))
}

/// Creates the default library's parent directories (0700 for every directory created here;
/// existing ones are left alone) and returns the library path. The library directory itself is
/// created and checked by `papersd`.
pub fn prepare_default_library(home: Option<&OsStr>) -> io::Result<PathBuf> {
    let library = default_library(home).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is not an absolute path; start with --library /absolute/path",
        )
    })?;
    if let Some(parent) = library.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    Ok(library)
}

/// Starts `papers-native-host refresh` next to `executable` (stdin/stdout closed, stderr
/// inherited for diagnostics). `None` when this installation has no browser host. The
/// caller reaps the child; its failure never blocks the app.
pub fn refresh_native_host(executable: &Path) -> Option<Child> {
    let host = executable.with_file_name(NATIVE_HOST);
    if !host.is_file() {
        return None;
    }
    Command::new(host)
        .arg("refresh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn only_a_bundle_macos_directory_is_a_bundle() {
        let exe =
            Path::new("/Volumes/Data/移動 先/Citation Manager.app/Contents/MacOS/citation-manager");
        assert_eq!(
            bundle_contents(exe),
            Some(Path::new(
                "/Volumes/Data/移動 先/Citation Manager.app/Contents"
            ))
        );
        for not_bundle in [
            "/home/u/.local/bin/citation-manager",
            "/src/checkout/target/release/citation-manager",
            "/x/Contents/MacOS/citation-manager",
            "/x/App.bundle/Contents/MacOS/citation-manager",
            "/x/App.app/Contents/Resources/citation-manager",
            "App.app/Contents/MacOS/citation-manager",
        ] {
            assert_eq!(bundle_contents(Path::new(not_bundle)), None, "{not_bundle}");
        }
    }

    #[test]
    fn launch_services_serial_numbers_are_dropped() {
        let args = launch_arguments(["-psn_0_12345", "--library", "/l"].map(OsString::from));
        assert_eq!(args, ["--library", "/l"].map(OsString::from));
    }

    #[test]
    fn default_library_needs_an_absolute_home() {
        assert_eq!(
            default_library(Some(OsStr::new("/Users/a b"))),
            Some(PathBuf::from(
                "/Users/a b/Library/Application Support/Citation Manager/library"
            ))
        );
        assert_eq!(default_library(Some(OsStr::new("relative"))), None);
        assert_eq!(default_library(None), None);
        assert!(prepare_default_library(None).is_err());
    }

    #[test]
    fn default_library_parents_are_created_private() {
        let temp = tempfile::tempdir().unwrap();
        let library = prepare_default_library(Some(temp.path().as_os_str())).unwrap();
        assert!(library.starts_with(temp.path()) && !library.exists());
        let app = temp.path().join(APP_SUPPORT);
        assert_eq!(
            std::fs::metadata(&app).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // Idempotent on an existing directory.
        assert_eq!(
            prepare_default_library(Some(temp.path().as_os_str())).unwrap(),
            library
        );
    }

    #[test]
    fn refresh_runs_the_sibling_host_only() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("citation-manager");
        assert!(refresh_native_host(&exe).is_none(), "no host installed");
        let out = temp.path().join("args");
        let host = temp.path().join(NATIVE_HOST);
        std::fs::write(
            &host,
            format!("#!/bin/sh\nprintf '%s' \"$*\" > '{}'\n", out.display()),
        )
        .unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Retry: a concurrently forked test process may briefly hold the script open (ETXTBSY).
        let mut child = (0..50)
            .find_map(|_| {
                refresh_native_host(&exe).or_else(|| {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                })
            })
            .expect("host started");
        let status = child.wait().unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(out).unwrap(), "refresh");
    }
}
