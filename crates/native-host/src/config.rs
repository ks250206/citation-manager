//! Per-user host configuration written by `register`/`pair` and read by every browser launch.
//!
//! `<config>/native-host/` (0700) holds `config.json` (0600: library, allowlist, manifest
//! records, grant id) and `credential` (0600: the daemon-issued capture grant's secret).
//! `<config>` is `$XDG_CONFIG_HOME/citation-manager` (else `~/.config/citation-manager`) on
//! Linux and `~/Library/Application Support/Citation Manager` on macOS. Browsers start the
//! host with the user's environment, so no path is taken from the browser or the page.
use crate::{caller::Allowlist, manifest::Browser};
use cm_protocol::{
    client::local::{read_private_file, verify_private_dir},
    ipc::{Credential, GrantId},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub const CONFIG_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_CREDENTIAL_BYTES: u64 = 256;

pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRecord {
    pub browser: Browser,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub version: u32,
    /// Absolute library the browser saves into (one per user installation).
    pub library: PathBuf,
    /// Private runtime directory override (tests/unusual setups); default as for the GUI.
    pub runtime_dir: Option<PathBuf>,
    /// Absolute host executable the manifests point to (refreshed after a move/update).
    pub host_path: PathBuf,
    pub allowed: Allowlist,
    pub manifests: Vec<ManifestRecord>,
    /// The daemon grant whose secret is in `credential` (for re-pairing/revocation).
    pub grant: Option<GrantId>,
    /// Grants whose secret is gone but whose revocation has not succeeded yet (a failed
    /// `unregister`, a replaced or half-stored pairing). Retried by `pair` and `unregister`
    /// so no native-host grant is left live and forgotten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoke_pending: Vec<GrantId>,
    /// Manifests were removed by `unregister`; the host refuses browser launches until the
    /// next `register`. Kept only while a grant is retained or a revocation is pending.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unregistered: bool,
}

/// Directory containing `native-host/`, from the user's environment only.
pub fn base_dir() -> io::Result<PathBuf> {
    base_dir_for(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_CONFIG_HOME"),
    )
}
/// `base_dir` for an explicit environment, resolved for the platform this binary runs on
/// (tests use it to find the directory the host executable will read).
pub fn base_dir_for(home: Option<OsString>, xdg: Option<OsString>) -> io::Result<PathBuf> {
    base_dir_from(home, xdg, cfg!(target_os = "macos"))
}
fn base_dir_from(
    home: Option<OsString>,
    xdg: Option<OsString>,
    macos: bool,
) -> io::Result<PathBuf> {
    let home = home
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not an absolute path"));
    if macos {
        return Ok(home?.join("Library/Application Support/Citation Manager"));
    }
    match xdg.map(PathBuf::from).filter(|x| x.is_absolute()) {
        Some(xdg) => Ok(xdg.join("citation-manager")),
        None => Ok(home?.join(".config/citation-manager")),
    }
}

#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}
impl Store {
    pub fn new(base: &Path) -> Self {
        Self {
            dir: base.join("native-host"),
        }
    }
    pub fn default_location() -> io::Result<Self> {
        Ok(Self::new(&base_dir()?))
    }
    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.json")
    }
    pub fn credential_path(&self) -> PathBuf {
        self.dir.join("credential")
    }
    /// Creates the base (0700 when new) and the private store directory.
    pub fn prepare(&self) -> io::Result<()> {
        if let Some(base) = self.dir.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(base)?;
        }
        match fs::DirBuilder::new().mode(0o700).create(&self.dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        verify_private_dir(&self.dir, euid())
    }
    pub fn load(&self) -> io::Result<HostConfig> {
        verify_private_dir(&self.dir, euid())?;
        let raw = read_private_file(&self.config_path(), euid(), MAX_CONFIG_BYTES)?;
        let config: HostConfig = serde_json::from_slice(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("config.json: {e}")))?;
        if config.version != CONFIG_VERSION
            || !config.library.is_absolute()
            || !config.host_path.is_absolute()
            || config
                .runtime_dir
                .as_ref()
                .is_some_and(|d| !d.is_absolute())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "config.json: unsupported version or relative path",
            ));
        }
        Ok(config)
    }
    /// `Ok(None)` only when nothing is registered (no directory or no config.json). A config
    /// that exists but is unreadable, unsafe or corrupt is an error, never "unregistered".
    pub fn load_optional(&self) -> Result<Option<HostConfig>, String> {
        match self.load() {
            Ok(config) => Ok(Some(config)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!(
                "{} cannot be used ({e}); fix or remove it",
                self.config_path().display()
            )),
        }
    }
    pub fn remove_credential(&self) -> io::Result<()> {
        match fs::remove_file(self.credential_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
    pub fn save(&self, config: &HostConfig) -> io::Result<()> {
        self.prepare()?;
        let mut body = serde_json::to_vec_pretty(config).map_err(io::Error::other)?;
        body.push(b'\n');
        write_atomic(&self.config_path(), &body, 0o600)
    }
    pub fn credential(&self) -> io::Result<Credential> {
        verify_private_dir(&self.dir, euid())?;
        let raw = read_private_file(&self.credential_path(), euid(), MAX_CREDENTIAL_BYTES)?;
        let text = String::from_utf8(raw)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "credential is not UTF-8"))?;
        Ok(Credential(text.trim().to_owned()))
    }
    pub fn save_credential(&self, credential: &Credential) -> io::Result<()> {
        self.prepare()?;
        write_atomic(&self.credential_path(), credential.0.as_bytes(), 0o600)
    }
    /// Removes config and credential (and the directory when it is then empty).
    pub fn remove(&self) -> io::Result<()> {
        for path in [self.credential_path(), self.config_path()] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let _ = fs::remove_dir(&self.dir);
        Ok(())
    }
}

/// Write to a same-directory temp file with `mode`, fsync, rename over `path`, fsync the dir.
pub fn write_atomic(path: &Path, body: &[u8], mode: u32) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".tmp-{}", std::process::id()));
    let temp = parent.join(temp_name);
    let _ = fs::remove_file(&temp);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        file.write_all(body)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn config_location_follows_platform_conventions() {
        assert_eq!(
            base_dir_from(Some("/home/u".into()), None, false).unwrap(),
            PathBuf::from("/home/u/.config/citation-manager")
        );
        assert_eq!(
            base_dir_from(Some("/home/u".into()), Some("/x/cfg".into()), false).unwrap(),
            PathBuf::from("/x/cfg/citation-manager")
        );
        // A relative XDG_CONFIG_HOME is ignored per the XDG spec.
        assert_eq!(
            base_dir_from(Some("/home/u".into()), Some("cfg".into()), false).unwrap(),
            PathBuf::from("/home/u/.config/citation-manager")
        );
        assert_eq!(
            base_dir_from(Some("/Users/u".into()), Some("/x".into()), true).unwrap(),
            PathBuf::from("/Users/u/Library/Application Support/Citation Manager")
        );
        assert!(base_dir_from(Some("rel".into()), None, false).is_err());
        assert!(base_dir_from(None, None, true).is_err());
    }

    #[test]
    fn config_and_credential_are_private_and_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(&temp.path().join("cfg"));
        let config = HostConfig {
            version: CONFIG_VERSION,
            library: "/l".into(),
            runtime_dir: None,
            host_path: "/opt/bin/papers-native-host".into(),
            allowed: Allowlist {
                chrome: ["a".repeat(32)].into(),
                firefox: Default::default(),
            },
            manifests: vec![ManifestRecord {
                browser: Browser::Chromium,
                path: "/m/dev.citation_manager.native_host.json".into(),
            }],
            grant: None,
            revoke_pending: Vec::new(),
            unregistered: false,
        };
        store.save(&config).unwrap();
        store.save_credential(&Credential("secret".into())).unwrap();
        assert_eq!(store.load().unwrap(), config);
        assert_eq!(store.credential().unwrap().0, "secret");
        for path in [store.config_path(), store.credential_path()] {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            fs::metadata(&store.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // A group-readable credential is refused rather than used.
        fs::set_permissions(store.credential_path(), fs::Permissions::from_mode(0o640)).unwrap();
        assert!(store.credential().is_err());
        store.remove().unwrap();
        assert!(!store.dir.exists());
        assert!(store.load().is_err());
        assert_eq!(store.load_optional(), Ok(None));
        // Corrupt (or unsafe) is not the same as unregistered.
        store.prepare().unwrap();
        assert_eq!(store.load_optional(), Ok(None));
        write_atomic(&store.config_path(), b"{not json", 0o600).unwrap();
        assert!(store.load_optional().unwrap_err().contains("fix or remove"));
        let body = serde_json::to_vec(&config).unwrap();
        write_atomic(&store.config_path(), &body, 0o644).unwrap();
        assert!(store.load_optional().is_err());
    }
}
