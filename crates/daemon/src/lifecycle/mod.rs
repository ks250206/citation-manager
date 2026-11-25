//! Daemon lifecycle (#25): single-writer lock, private endpoint directories, idle exit.
//!
//! Stale locks: the lock is an OS advisory lock (`flock`) on an open file, released by the
//! kernel when the holder dies, so a crashed daemon never leaves a lock that needs guessing.
//! A leftover socket file is only removed by the new lock holder.
pub use cm_protocol::client::local::{
    CredentialSource, DaemonPaths, EXIT_ALREADY_RUNNING, Launch, LocalConnector, read_private_file,
    verify_private_dir,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Connector for executables in the daemon composition root (GUI bootstrap, tests).
pub fn local_connector(paths: DaemonPaths, client_kind: &str) -> LocalConnector {
    LocalConnector::new(paths, euid(), client_kind)
}

/// Creates `path` (one level, 0700) if missing, then requires it to be private to us.
pub fn prepare_private_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    verify_private_dir(path, euid())
}

/// Exclusive per-library writer/migration lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct LibraryLock {
    _file: File,
}
impl LibraryLock {
    /// `Ok(None)`: another live process holds it (concurrent start lost the race).
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        let m = file.metadata()?;
        if !m.is_file() || m.uid() != euid() || m.mode() & 0o7777 != 0o600 || m.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe daemon lock file",
            ));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(None),
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
        // Diagnostic only: the lock, not this PID, decides liveness.
        let mut file = file;
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_all()?;
        Ok(Some(Self { _file: file }))
    }
}

/// Removes a leftover socket from a dead daemon. Only call while holding `LibraryLock`.
pub fn remove_stale_socket(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() && m.uid() == euid() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket path is occupied by something else",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Connections and active jobs keep the daemon alive; idle time counts from the moment
/// both became zero.
#[derive(Debug)]
pub struct Activity {
    connections: AtomicUsize,
    /// #27 holders that keep the daemon from idling without being a connection (the
    /// enabled REST listener).
    pinned: AtomicUsize,
    active_jobs: AtomicUsize,
    idle_since: Mutex<Instant>,
}
impl Default for Activity {
    fn default() -> Self {
        Self {
            connections: AtomicUsize::new(0),
            pinned: AtomicUsize::new(0),
            active_jobs: AtomicUsize::new(0),
            idle_since: Mutex::new(Instant::now()),
        }
    }
}
pub struct ConnectionGuard(Arc<Activity>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}
/// Keeps the daemon from idle exit while held (see `Activity::pin`).
pub struct PinGuard(Arc<Activity>);
impl Drop for PinGuard {
    fn drop(&mut self) {
        self.0.pinned.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}
impl Activity {
    /// #27: an enabled REST listener keeps papersd running (REST clients cannot start it).
    pub fn pin(self: &Arc<Self>) -> PinGuard {
        self.pinned.fetch_add(1, Ordering::SeqCst);
        PinGuard(self.clone())
    }
    pub fn connect(self: &Arc<Self>) -> ConnectionGuard {
        self.connections.fetch_add(1, Ordering::SeqCst);
        ConnectionGuard(self.clone())
    }
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
    pub fn active_jobs(&self) -> usize {
        self.active_jobs.load(Ordering::SeqCst)
    }
    pub fn set_active_jobs(&self, count: usize) {
        if self.active_jobs.swap(count, Ordering::SeqCst) != count {
            self.touch();
        }
    }
    fn touch(&self) {
        *self.idle_since.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }
    /// True only with no connection and no queued/running/retry_wait job for `timeout`.
    pub fn idle_for(&self, timeout: Duration) -> bool {
        self.connections() == 0
            && self.pinned.load(Ordering::SeqCst) == 0
            && self.active_jobs() == 0
            && self
                .idle_since
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed()
                >= timeout
    }
}
