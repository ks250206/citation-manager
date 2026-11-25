//! Native Messaging host manifests. Always generated from the installed executable's
//! absolute path (`<prefix>/bin/papers-native-host`, `.app/Contents/MacOS/papers-native-host`),
//! never from a template with a relative or development-tree path.
use crate::config::write_atomic;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};

/// Host name registered with the browsers (lowercase, dots and underscores only).
pub const HOST_NAME: &str = "dev.citation_manager.native_host";
pub const DESCRIPTION: &str = "Citation Manager browser capture host";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Browser {
    Chrome,
    Chromium,
    Firefox,
}
impl Browser {
    pub const ALL: [Browser; 3] = [Self::Chrome, Self::Chromium, Self::Firefox];
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
        }
    }
    pub fn is_firefox(self) -> bool {
        self == Self::Firefox
    }
    /// Per-user manifest directory and the profile root whose existence means "installed".
    pub fn user_dirs(self, home: &Path) -> (PathBuf, PathBuf) {
        let (profile, hosts) = if cfg!(target_os = "macos") {
            match self {
                Self::Chrome => (
                    "Library/Application Support/Google/Chrome",
                    "NativeMessagingHosts",
                ),
                Self::Chromium => (
                    "Library/Application Support/Chromium",
                    "NativeMessagingHosts",
                ),
                Self::Firefox => (
                    "Library/Application Support/Mozilla",
                    "NativeMessagingHosts",
                ),
            }
        } else {
            match self {
                Self::Chrome => (".config/google-chrome", "NativeMessagingHosts"),
                Self::Chromium => (".config/chromium", "NativeMessagingHosts"),
                Self::Firefox => (".mozilla", "native-messaging-hosts"),
            }
        };
        let profile = home.join(profile);
        (profile.join(hosts), profile)
    }
}

/// The manifest document for one browser family.
pub fn document(browser: Browser, host_path: &Path, ids: &BTreeSet<String>) -> Value {
    let path = host_path.to_string_lossy();
    if browser.is_firefox() {
        json!({
            "name": HOST_NAME,
            "description": DESCRIPTION,
            "path": path,
            "type": "stdio",
            "allowed_extensions": ids,
        })
    } else {
        let origins: Vec<String> = ids
            .iter()
            .map(|id| format!("chrome-extension://{id}/"))
            .collect();
        json!({
            "name": HOST_NAME,
            "description": DESCRIPTION,
            "path": path,
            "type": "stdio",
            "allowed_origins": origins,
        })
    }
}

pub fn file_name() -> String {
    format!("{HOST_NAME}.json")
}

/// Writes (atomically, 0644) the manifest into `dir`, creating the directory when needed.
pub fn write(
    dir: &Path,
    browser: Browser,
    host_path: &Path,
    ids: &BTreeSet<String>,
) -> io::Result<PathBuf> {
    if !dir.is_absolute() || !host_path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "manifest paths must be absolute",
        ));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)?;
    let path = dir.join(file_name());
    let mut body =
        serde_json::to_vec_pretty(&document(browser, host_path, ids)).map_err(io::Error::other)?;
    body.push(b'\n');
    write_atomic(&path, &body, 0o644)?;
    Ok(path)
}

/// The host path recorded in an existing manifest of ours, if it is one.
pub fn recorded_host_path(path: &Path) -> Option<PathBuf> {
    let raw = fs::read(path).ok()?;
    let doc: Value = serde_json::from_slice(&raw).ok()?;
    if doc.get("name")? != HOST_NAME {
        return None;
    }
    Some(PathBuf::from(doc.get("path")?.as_str()?))
}

/// Removes a manifest only when it is a regular file carrying our host name.
pub fn remove(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && recorded_host_path(path).is_some() => {
            fs::remove_file(path)?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Executable checks before a path is published to browsers: absolute regular file, executable,
/// owned by the user or root, and neither it nor its directory writable by group/other.
pub fn check_executable(path: &Path, uid: u32) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{} is not absolute", path.display()));
    }
    let m = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !m.is_file() || m.mode() & 0o111 == 0 {
        return Err(format!("{} is not an executable file", path.display()));
    }
    let safe = |m: &fs::Metadata| (m.uid() == uid || m.uid() == 0) && m.mode() & 0o022 == 0;
    if !safe(&m) {
        return Err(format!(
            "{} must be owned by you or root and not group/other-writable",
            path.display()
        ));
    }
    let dir = path.parent().ok_or("executable has no parent directory")?;
    let dm = fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !safe(&dm) {
        return Err(format!(
            "{} must be owned by you or root and not group/other-writable",
            dir.display()
        ));
    }
    Ok(())
}

/// A cargo build output inside a source checkout (`<checkout>/target/...` next to a
/// `Cargo.toml`). Manifests must not point browsers into a development tree.
pub fn in_development_tree(path: &Path) -> bool {
    path.ancestors().any(|dir| {
        dir.file_name().is_some_and(|n| n == "target")
            && dir.parent().is_some_and(|p| p.join("Cargo.toml").is_file())
    })
}

/// Why browsers must not be pointed at `path`, if they must not: a macOS App Translocation
/// copy (Gatekeeper runs a quarantined app that was never moved from a randomized, read-only
/// location that disappears) or any read-only filesystem such as the mounted `.dmg`. Either
/// would leave the manifest pointing at a path that is gone or can never be updated (#34).
pub fn transient_location(path: &Path) -> Option<&'static str> {
    if is_translocated(path) {
        return Some("is an App Translocation copy (the app was started without being moved)");
    }
    match rustix::fs::statvfs(path) {
        Ok(fs) if fs.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY) => {
            Some("is on a read-only volume (e.g. the mounted disk image)")
        }
        _ => None,
    }
}

fn is_translocated(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == "AppTranslocation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn chrome_and_firefox_documents_use_their_own_allowlist_keys() {
        let ids: BTreeSet<String> = ["a".repeat(32)].into();
        let chrome = document(
            Browser::Chrome,
            Path::new("/opt/cm/bin/papers-native-host"),
            &ids,
        );
        assert_eq!(chrome["name"], HOST_NAME);
        assert_eq!(chrome["type"], "stdio");
        assert_eq!(chrome["path"], "/opt/cm/bin/papers-native-host");
        assert_eq!(
            chrome["allowed_origins"],
            json!([format!("chrome-extension://{}/", "a".repeat(32))])
        );
        assert!(chrome.get("allowed_extensions").is_none());
        let firefox = document(
            Browser::Firefox,
            Path::new("/x/papers-native-host"),
            &["a@b".into()].into(),
        );
        assert_eq!(firefox["allowed_extensions"], json!(["a@b"]));
        assert!(firefox.get("allowed_origins").is_none());
        // Chrome requires lowercase alnum, '_' and '.'.
        assert!(
            HOST_NAME
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_')
        );
    }

    #[test]
    fn platform_directories() {
        let home = Path::new("/home/u");
        let (chrome, _) = Browser::Chrome.user_dirs(home);
        let (firefox, profile) = Browser::Firefox.user_dirs(home);
        if cfg!(target_os = "macos") {
            assert_eq!(
                chrome,
                home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts")
            );
            assert_eq!(
                firefox,
                home.join("Library/Application Support/Mozilla/NativeMessagingHosts")
            );
        } else {
            assert_eq!(
                chrome,
                home.join(".config/google-chrome/NativeMessagingHosts")
            );
            assert_eq!(
                Browser::Chromium.user_dirs(home).0,
                home.join(".config/chromium/NativeMessagingHosts")
            );
            assert_eq!(firefox, home.join(".mozilla/native-messaging-hosts"));
            assert_eq!(profile, home.join(".mozilla"));
        }
    }

    #[test]
    fn write_records_and_removes_only_our_manifests() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("NativeMessagingHosts");
        let ids: BTreeSet<String> = ["a@b".into()].into();
        let path = write(
            &dir,
            Browser::Firefox,
            Path::new("/new/papers-native-host"),
            &ids,
        )
        .unwrap();
        assert_eq!(path, dir.join("dev.citation_manager.native_host.json"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            recorded_host_path(&path),
            Some(PathBuf::from("/new/papers-native-host"))
        );
        assert!(write(Path::new("rel"), Browser::Firefox, Path::new("/x"), &ids).is_err());
        let foreign = dir.join("other.json");
        fs::write(&foreign, br#"{"name":"other.host","path":"/x"}"#).unwrap();
        assert!(!remove(&foreign).unwrap());
        assert!(foreign.exists());
        assert!(remove(&path).unwrap());
        assert!(!remove(&path).unwrap());
    }

    #[test]
    fn executable_and_development_tree_checks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let bin = root.join("prefix/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let exe = bin.join("papers-native-host");
        fs::write(&exe, b"x").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o644)).unwrap();
        let uid = crate::config::euid();
        assert!(check_executable(&exe, uid).is_err());
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_executable(&exe, uid).is_ok());
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(check_executable(&exe, uid).is_err());
        assert!(check_executable(Path::new("relative"), uid).is_err());

        assert!(!in_development_tree(&exe));
        let checkout = root.join("checkout");
        fs::create_dir_all(checkout.join("target/release")).unwrap();
        fs::write(checkout.join("Cargo.toml"), b"").unwrap();
        assert!(in_development_tree(
            &checkout.join("target/release/papers-native-host")
        ));
    }

    #[test]
    fn translocated_and_read_only_locations_are_never_published() {
        let translocated = Path::new(
            "/private/var/folders/ab/cd/T/AppTranslocation/0A1B/d/Citation Manager.app/Contents/MacOS/papers-native-host",
        );
        assert!(is_translocated(translocated));
        assert!(transient_location(translocated).is_some());
        let installed =
            Path::new("/Applications/Citation Manager.app/Contents/MacOS/papers-native-host");
        assert!(!is_translocated(installed));
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(transient_location(temp.path()), None, "writable temp dir");
    }
}
