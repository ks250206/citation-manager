//! Per-user local endpoint discovery and lazy start, std-only so every client crate
//! (GUI composition, CLI, MCP, native host) can share it. The caller supplies its effective
//! UID (std has no portable getter); the daemon independently verifies peers.
use super::{Connector, Stream};
use crate::ipc::{CapabilitySet, Credential, Hello, PROTOCOL_VERSION};
use std::{
    ffi::OsString,
    fs, io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Exit status of a `papersd` that found another live daemon holding the library lock.
pub const EXIT_ALREADY_RUNNING: i32 = 75;
/// sockaddr_un.sun_path is 104 bytes on macOS and 108 on Linux (including NUL).
const MAX_SOCKET_PATH: usize = 100;
/// Minimum spacing of re-launches while a lock holder without a socket is going away.
const RESPAWN_INTERVAL: Duration = Duration::from_millis(500);

/// Where one library's daemon lives. Library-scoped state (lock, bootstrap credential) is
/// inside the private library directory; the socket is in a short private runtime directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPaths {
    pub library: PathBuf,
    /// `<library>/daemon`, 0700.
    pub state_dir: PathBuf,
    /// Exclusive single-writer lock held for the daemon's lifetime.
    pub lock: PathBuf,
    /// Bootstrap credential of the local desktop owner (0600, rotated if missing).
    pub desktop_credential: PathBuf,
    /// Private per-user directory (0700) holding sockets.
    pub runtime_dir: PathBuf,
    pub socket: PathBuf,
}
impl DaemonPaths {
    /// `library` is canonicalized first (symlinks, `.`/`..`; a not-yet-created library via its
    /// nearest existing ancestor), so every spelling of one library maps to one socket, lock
    /// and `Hello::library`.
    ///
    /// `runtime_dir`: explicit override (tests, unusual setups); otherwise
    /// `$XDG_RUNTIME_DIR/citation-manager`, then `$TMPDIR/citation-manager-<uid>` (macOS
    /// per-user temp), then `/tmp/citation-manager-<uid>`.
    pub fn new(library: &Path, runtime_dir: Option<&Path>, uid: u32) -> io::Result<Self> {
        if !library.is_absolute() {
            return Err(invalid("library path must be absolute"));
        }
        let library = &canonical_library(library)?;
        let runtime_dir = match runtime_dir {
            Some(dir) if dir.is_absolute() => dir.to_owned(),
            Some(_) => return Err(invalid("runtime directory must be absolute")),
            None => default_runtime_dir(uid),
        };
        let socket = runtime_dir.join(format!("{}.sock", endpoint_name(library)));
        if socket.as_os_str().len() >= MAX_SOCKET_PATH {
            return Err(invalid(
                "socket path too long; choose a shorter runtime directory",
            ));
        }
        let state_dir = library.join("daemon");
        Ok(Self {
            library: library.to_owned(),
            lock: state_dir.join("papersd.lock"),
            desktop_credential: state_dir.join("desktop.credential"),
            state_dir,
            runtime_dir,
            socket,
        })
    }
}
fn default_runtime_dir(uid: u32) -> PathBuf {
    runtime_dir_from(
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("TMPDIR"),
        uid,
    )
}
fn runtime_dir_from(xdg: Option<OsString>, tmp: Option<OsString>, uid: u32) -> PathBuf {
    // XDG_RUNTIME_DIR is per-user by definition; TMPDIR may be shared, so suffix the UID.
    let candidates = [
        (xdg, "citation-manager".to_owned()),
        (tmp, format!("citation-manager-{uid}")),
    ];
    for (dir, name) in candidates {
        if let Some(dir) = dir.map(PathBuf::from)
            && dir.is_absolute()
        {
            let candidate = dir.join(name);
            if candidate.as_os_str().len() + ENDPOINT_NAME_LEN + 6 < MAX_SOCKET_PATH {
                return candidate;
            }
        }
    }
    PathBuf::from(format!("/tmp/citation-manager-{uid}"))
}
/// Hex characters of the socket name (the first 64 bits of SHA-256, to fit `sun_path`).
const ENDPOINT_NAME_LEN: usize = 16;
/// Socket name of a canonical library path: truncated SHA-256, so distinct libraries do not
/// collide in practice (a crafted FNV collision could make one library's daemon remove the
/// other's socket). Authority never depends on it: the daemon also checks `Hello::library`.
fn endpoint_name(library: &Path) -> String {
    super::sha256::digest(library.as_os_str().as_bytes())[..ENDPOINT_NAME_LEN / 2]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
/// `fs::canonicalize`, or for a library that does not exist yet, its nearest existing
/// ancestor canonicalized plus the remaining plain components.
fn canonical_library(library: &Path) -> io::Result<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = library;
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(std::path::Component::Normal(name))) =
                    (existing.parent(), existing.components().next_back())
                else {
                    return Err(invalid("library path cannot be resolved"));
                };
                missing.push(name.to_owned());
                existing = parent;
            }
            Err(e) => return Err(e),
        }
    }
}
fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}
fn unsafe_path(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "unsafe owner/type/permissions: {}",
            path.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ),
    )
}

/// A directory only this user can enter: real directory (not a symlink), owned by `uid`,
/// no group/other permission bits. Only such a directory can hold a trusted socket.
pub fn verify_private_dir(path: &Path, uid: u32) -> io::Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() || m.uid() != uid || m.mode() & 0o077 != 0 {
        return Err(unsafe_path(path));
    }
    Ok(())
}
/// A regular private file (0600, single link) owned by `uid`, at most `max` bytes.
pub fn read_private_file(path: &Path, uid: u32, max: u64) -> io::Result<Vec<u8>> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file()
        || m.file_type().is_symlink()
        || m.uid() != uid
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || m.len() > max
    {
        return Err(unsafe_path(path));
    }
    fs::read(path)
}

/// How to start `papersd` on demand when nothing answers on the socket.
#[derive(Debug, Clone)]
pub struct Launch {
    /// Absolute path, normally next to the calling executable (never a PATH search).
    pub papersd: PathBuf,
    pub extra_args: Vec<OsString>,
    pub timeout: Duration,
}

#[derive(Clone)]
pub enum CredentialSource {
    /// The owner's bootstrap credential written by the daemon into the library.
    DesktopBootstrap,
    /// A credential issued through `GrantCommand::Issue` (CLI/MCP/native host).
    Explicit(Credential),
}

/// Connects to (and optionally starts) the library's daemon and presents a credential.
#[derive(Clone)]
pub struct LocalConnector {
    pub paths: DaemonPaths,
    pub uid: u32,
    pub launch: Option<Launch>,
    pub client_kind: String,
    pub requested: Option<CapabilitySet>,
    pub credential: CredentialSource,
}
impl LocalConnector {
    pub fn new(paths: DaemonPaths, uid: u32, client_kind: &str) -> Self {
        Self {
            paths,
            uid,
            launch: None,
            client_kind: client_kind.into(),
            requested: None,
            credential: CredentialSource::DesktopBootstrap,
        }
    }
    fn try_connect(&self) -> io::Result<UnixStream> {
        verify_private_dir(&self.paths.runtime_dir, self.uid)?;
        let m = fs::symlink_metadata(&self.paths.socket)?;
        if !m.file_type().is_socket() || m.uid() != self.uid {
            return Err(unsafe_path(&self.paths.socket));
        }
        UnixStream::connect(&self.paths.socket)
    }
    /// Starts the daemon and waits until it accepts, or the launch timeout.
    pub fn connect_or_start(&self) -> io::Result<UnixStream> {
        match self.try_connect() {
            Ok(stream) => return Ok(stream),
            Err(e) if self.launch.is_none() => return Err(e),
            Err(e)
                if !matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Err(e);
            }
            Err(_) => {}
        }
        let launch = self.launch.as_ref().expect("checked");
        if !launch.papersd.is_absolute() {
            return Err(invalid("papersd path must be absolute"));
        }
        let spawn = || {
            Command::new(&launch.papersd)
                .arg("--library")
                .arg(&self.paths.library)
                .arg("--runtime-dir")
                .arg(&self.paths.runtime_dir)
                .args(&launch.extra_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                // Own process group: terminal signals to the client do not stop the daemon.
                .process_group(0)
                .spawn()
        };
        let mut child = spawn()?;
        let mut spawned = Instant::now();
        let started = Instant::now();
        let mut delay = Duration::from_millis(10);
        // Exit status of the last launched papersd that went away without serving.
        let mut last_exit: Option<std::process::ExitStatus> = None;
        loop {
            match self.try_connect() {
                Ok(stream) => {
                    // Reap the launcher's child whenever it exits (idle exit or lost race).
                    thread::spawn(move || child.wait());
                    return Ok(stream);
                }
                Err(e) if started.elapsed() >= launch.timeout => {
                    if let Ok(Some(status)) = child.try_wait() {
                        last_exit = Some(status);
                    }
                    thread::spawn(move || child.wait());
                    let exit = last_exit
                        .map(|s| format!("; last papersd exit: {s}"))
                        .unwrap_or_default();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("daemon did not start: {e}{exit}"),
                    ));
                }
                Err(_) => {
                    // A loser of a start race exits EXIT_ALREADY_RUNNING; keep waiting for
                    // the winner's socket. If the lock holder was a daemon in its idle exit,
                    // no socket will come: start again (rate-limited) once it is gone.
                    // Any other early exit (e.g. a transient I/O error while the previous
                    // daemon is still releasing the library) is retried the same way, bounded
                    // by the launch timeout; its status is reported if we finally give up.
                    if let Ok(Some(status)) = child.try_wait() {
                        if status.code() != Some(EXIT_ALREADY_RUNNING) {
                            last_exit = Some(status);
                        }
                        if spawned.elapsed() >= RESPAWN_INTERVAL {
                            child = spawn()?;
                            spawned = Instant::now();
                        }
                    }
                    thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_millis(200));
                }
            }
        }
    }
    pub fn hello(&self) -> io::Result<Hello> {
        let credential = match &self.credential {
            CredentialSource::Explicit(c) => c.clone(),
            CredentialSource::DesktopBootstrap => {
                verify_private_dir(&self.paths.state_dir, self.uid)?;
                let raw = read_private_file(&self.paths.desktop_credential, self.uid, 256)?;
                Credential(
                    String::from_utf8(raw)
                        .map_err(|_| invalid("credential is not UTF-8"))?
                        .trim()
                        .to_owned(),
                )
            }
        };
        Ok(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_kind: self.client_kind.clone(),
            requested: self.requested.clone(),
            library: Some(self.paths.library.clone()),
            credential,
        })
    }
}
impl Connector for LocalConnector {
    fn connect(&self) -> io::Result<(Box<dyn Stream>, Hello)> {
        let stream = self.connect_or_start()?;
        Ok((Box::new(stream), self.hello()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_library_maps_to_one_endpoint() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "cm-paths-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let runtime = Path::new("/run/test");
        let canonical = DaemonPaths::new(&root.join("real/lib"), Some(runtime), 1).unwrap();
        assert_eq!(canonical.library, root.join("real/lib"));
        for spelling in ["link/lib", "real/./lib", "link/../real/lib"] {
            let paths = DaemonPaths::new(&root.join(spelling), Some(runtime), 1).unwrap();
            assert_eq!(paths, canonical, "{spelling}");
        }
        // Still the same endpoint once the library exists.
        fs::create_dir(root.join("real/lib")).unwrap();
        assert_eq!(
            DaemonPaths::new(&root.join("link/lib"), Some(runtime), 1).unwrap(),
            canonical
        );
        let other = DaemonPaths::new(&root.join("real/other"), Some(runtime), 1).unwrap();
        assert_ne!(other.socket, canonical.socket);
        let name = canonical.socket.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), ENDPOINT_NAME_LEN + ".sock".len());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shared_tmpdir_runtime_directories_are_per_uid() {
        assert_eq!(
            runtime_dir_from(Some("/run/user/501".into()), Some("/tmp".into()), 501),
            PathBuf::from("/run/user/501/citation-manager")
        );
        assert_eq!(
            runtime_dir_from(None, Some("/var/folders/ab/T".into()), 501),
            PathBuf::from("/var/folders/ab/T/citation-manager-501")
        );
        assert_eq!(
            runtime_dir_from(Some("relative".into()), None, 7),
            PathBuf::from("/tmp/citation-manager-7")
        );
    }
}
