//! #26 token plaintext storage, std-only so the daemon, CLI (#28) and MCP (#29) share it.
//!
//! The daemon keeps only SHA-256 of a token. The plaintext is shown once to the person who
//! issued it and kept in a per-user secret store so local clients (e.g. `papers mcp`) can
//! load their credential without the user pasting it into config files:
//!
//! - macOS: the login Keychain through `/usr/bin/security`. Commands go through
//!   `security -i` on stdin so the secret never appears in a process argument list.
//! - Linux: the freedesktop Secret Service through `/usr/bin/secret-tool` (secret on stdin).
//! - Fallback: one 0600 file per token in a 0700 per-library directory **outside** the
//!   library (`DaemonPaths::secrets_dir`: `$XDG_DATA_HOME` or `~/.local/share`, macOS
//!   `~/Library/Application Support`, `/citation-manager/secrets/<library key>`), so a
//!   library folder that is synced or copied does not carry token copies. Weaker than an
//!   OS store: readable by any process of the same user and copied by home-directory
//!   backups. Chosen automatically when no OS store is usable.
//!
//! Helpers are started by absolute path only (never a PATH search) with a timeout. Entries
//! are keyed by `account(library, grant)`; names are hex/`-`/`:` only.
use crate::{api::SecretBackend, ipc::Credential};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

/// Longest a keychain/secret-service helper may take before it is killed.
const HELPER_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_SECRET: u64 = 512;

pub trait SecretStore: Send + Sync {
    /// Preferred backend (what `put` normally uses).
    fn backend(&self) -> SecretBackend;
    /// Stores (or replaces) the secret; returns where it went.
    fn put(&self, account: &str, secret: &Credential) -> io::Result<SecretBackend>;
    fn get(&self, account: &str) -> io::Result<Option<Credential>>;
    /// Idempotent: a missing entry is not an error.
    fn delete(&self, account: &str) -> io::Result<()>;
}

/// `<library key>:<grant id>`, the library key being the endpoint hash of the canonical
/// library path (`DaemonPaths::library_key`).
pub fn account(library_key: &str, grant: impl std::fmt::Display) -> String {
    format!("{library_key}:{grant}")
}

fn check_account(account: &str) -> io::Result<()> {
    if account.is_empty()
        || account.len() > 128
        || !account
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-' || b == b':')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid secret account name",
        ));
    }
    Ok(())
}
fn check_secret(secret: &Credential) -> io::Result<()> {
    // Daemon credentials are lowercase hex; anything else is refused rather than escaped.
    if secret.0.is_empty()
        || secret.0.len() as u64 > MAX_SECRET
        || !secret.0.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected credential format",
        ));
    }
    Ok(())
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemorySecretStore {
    entries: Mutex<HashMap<String, Credential>>,
}
impl MemorySecretStore {
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl SecretStore for MemorySecretStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::Memory
    }
    fn put(&self, account: &str, secret: &Credential) -> io::Result<SecretBackend> {
        check_account(account)?;
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account.into(), secret.clone());
        Ok(SecretBackend::Memory)
    }
    fn get(&self, account: &str) -> io::Result<Option<Credential>> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(account)
            .cloned())
    }
    fn delete(&self, account: &str) -> io::Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account);
        Ok(())
    }
}

/// Weaker fallback: `<dir>/<account>.secret`, 0600, in a 0700 directory owned by `uid`.
pub struct FileSecretStore {
    dir: PathBuf,
    uid: u32,
}
impl FileSecretStore {
    pub fn new(dir: PathBuf, uid: u32) -> Self {
        Self { dir, uid }
    }
    fn path(&self, account: &str) -> io::Result<PathBuf> {
        check_account(account)?;
        Ok(self
            .dir
            .join(format!("{}.secret", account.replace(':', "_"))))
    }
    fn prepare(&self) -> io::Result<()> {
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
        {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        super::local::verify_private_dir(&self.dir, self.uid)
    }
}
impl SecretStore for FileSecretStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::File
    }
    fn put(&self, account: &str, secret: &Credential) -> io::Result<SecretBackend> {
        check_secret(secret)?;
        let path = self.path(account)?;
        self.prepare()?;
        let temp = path.with_extension(format!("tmp-{}", std::process::id()));
        let _ = fs::remove_file(&temp);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(secret.0.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(SecretBackend::File)
    }
    fn get(&self, account: &str) -> io::Result<Option<Credential>> {
        let path = self.path(account)?;
        match read_no_follow(&path, self.uid) {
            Ok(raw) => String::from_utf8(raw)
                .map(|s| Some(Credential(s.trim().to_owned())))
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn delete(&self, account: &str) -> io::Result<()> {
        match fs::remove_file(self.path(account)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Reads a private secret file without following a symlink: `lstat` must show a regular
/// 0600 single-link file owned by `uid`, and the opened file's `fstat` must be that same
/// inode (a swap between the two calls is refused). std exposes no `O_NOFOLLOW`; this
/// comparison gives the same guarantee without a platform constant.
fn read_no_follow(path: &Path, uid: u32) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;
    let before = fs::symlink_metadata(path)?;
    let private = |m: &fs::Metadata| {
        m.is_file() && m.uid() == uid && m.mode() & 0o7777 == 0o600 && m.nlink() == 1
    };
    if before.file_type().is_symlink() || !private(&before) || before.len() > MAX_SECRET {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe secret file",
        ));
    }
    let mut file = fs::File::open(path)?;
    let after = file.metadata()?;
    if (after.dev(), after.ino()) != (before.dev(), before.ino()) || !private(&after) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "secret file changed while opening",
        ));
    }
    let mut out = Vec::new();
    (&mut file).take(MAX_SECRET + 1).read_to_end(&mut out)?;
    if out.len() as u64 > MAX_SECRET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "secret too large",
        ));
    }
    Ok(out)
}

/// Runs a helper by absolute path with optional stdin, a timeout and captured stdout.
/// Returns (exit code, stdout). stderr is discarded (it may echo attribute names only).
/// The child is always reaped: every error after spawning kills and waits for it.
fn run_helper(program: &Path, args: &[&str], stdin: Option<&[u8]>) -> io::Result<(i32, Vec<u8>)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let reap = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let written = pipe.write_all(input);
        drop(pipe);
        if let Err(e) = written {
            reap(&mut child);
            return Err(e);
        }
    }
    let Some(mut stdout) = child.stdout.take() else {
        reap(&mut child);
        return Err(io::Error::other("helper stdout unavailable"));
    };
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() <= HELPER_TIMEOUT => {
                thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                reap(&mut child);
                let _ = reader.join();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "secret store helper timed out",
                ));
            }
            Err(e) => {
                reap(&mut child);
                let _ = reader.join();
                return Err(e);
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), out))
}
fn parse_secret(out: Vec<u8>) -> io::Result<Option<Credential>> {
    let text = String::from_utf8(out)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))?;
    let secret = Credential(text.trim().to_owned());
    check_secret(&secret)?;
    Ok(Some(secret))
}

/// macOS login Keychain via `/usr/bin/security` (not verified on this platform's CI; #34).
pub struct KeychainSecretStore {
    pub program: PathBuf,
}
impl SecretStore for KeychainSecretStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::Keychain
    }
    fn put(&self, account: &str, secret: &Credential) -> io::Result<SecretBackend> {
        check_account(account)?;
        check_secret(secret)?;
        // `-U` updates an existing item. Interactive mode reads this line from stdin, so the
        // secret is not visible in `ps`. Account and secret are hex: nothing to quote.
        let line = format!(
            "add-generic-password -U -a {account} -s {} -w {}\n",
            crate::api::SECRET_SERVICE,
            secret.0
        );
        let (code, _) = run_helper(&self.program, &["-i"], Some(line.as_bytes()))?;
        // `security -i` exits 0 even when a command fails; read back to confirm.
        if code != 0 || self.get(account)?.as_ref() != Some(secret) {
            return Err(io::Error::other("keychain did not store the token"));
        }
        Ok(SecretBackend::Keychain)
    }
    fn get(&self, account: &str) -> io::Result<Option<Credential>> {
        check_account(account)?;
        let (code, out) = run_helper(
            &self.program,
            &[
                "find-generic-password",
                "-a",
                account,
                "-s",
                crate::api::SECRET_SERVICE,
                "-w",
            ],
            None,
        )?;
        match code {
            0 => parse_secret(out),
            44 => Ok(None), // errSecItemNotFound
            _ => Err(io::Error::other("keychain lookup failed")),
        }
    }
    fn delete(&self, account: &str) -> io::Result<()> {
        check_account(account)?;
        let (code, _) = run_helper(
            &self.program,
            &[
                "delete-generic-password",
                "-a",
                account,
                "-s",
                crate::api::SECRET_SERVICE,
            ],
            None,
        )?;
        match code {
            0 | 44 => Ok(()),
            _ => Err(io::Error::other("keychain delete failed")),
        }
    }
}

/// freedesktop Secret Service via `/usr/bin/secret-tool` (libsecret).
pub struct SecretServiceStore {
    pub program: PathBuf,
}
impl SecretStore for SecretServiceStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::SecretService
    }
    fn put(&self, account: &str, secret: &Credential) -> io::Result<SecretBackend> {
        check_account(account)?;
        check_secret(secret)?;
        let (code, _) = run_helper(
            &self.program,
            &[
                "store",
                "--label=Citation Manager token",
                "service",
                crate::api::SECRET_SERVICE,
                "account",
                account,
            ],
            Some(secret.0.as_bytes()),
        )?;
        if code != 0 {
            return Err(io::Error::other("secret service did not store the token"));
        }
        Ok(SecretBackend::SecretService)
    }
    fn get(&self, account: &str) -> io::Result<Option<Credential>> {
        check_account(account)?;
        let (code, out) = run_helper(
            &self.program,
            &[
                "lookup",
                "service",
                crate::api::SECRET_SERVICE,
                "account",
                account,
            ],
            None,
        )?;
        match code {
            0 => parse_secret(out),
            1 if out.is_empty() => Ok(None),
            _ => Err(io::Error::other("secret service lookup failed")),
        }
    }
    fn delete(&self, account: &str) -> io::Result<()> {
        check_account(account)?;
        // `clear` succeeds whether or not a matching item exists.
        let (code, _) = run_helper(
            &self.program,
            &[
                "clear",
                "service",
                crate::api::SECRET_SERVICE,
                "account",
                account,
            ],
            None,
        )?;
        if code != 0 {
            return Err(io::Error::other("secret service delete failed"));
        }
        Ok(())
    }
}

/// The OS store when usable, else the file fallback. A failed OS write falls back to the
/// file store (reported by `put`); lookups/deletes consult both.
pub struct PlatformSecretStore {
    primary: Option<Box<dyn SecretStore>>,
    fallback: FileSecretStore,
}
impl PlatformSecretStore {
    /// `fallback_dir`: normally `DaemonPaths::secrets_dir()` (outside the library).
    /// `CM_SECRET_STORE=file` forces the file store (headless sessions, CI).
    pub fn detect(fallback_dir: PathBuf, uid: u32) -> Self {
        let forced_file = std::env::var_os("CM_SECRET_STORE").is_some_and(|v| v == "file");
        let primary: Option<Box<dyn SecretStore>> = if forced_file {
            None
        } else if cfg!(target_os = "macos") && Path::new("/usr/bin/security").is_file() {
            Some(Box::new(KeychainSecretStore {
                program: "/usr/bin/security".into(),
            }))
        } else if cfg!(target_os = "linux")
            && Path::new("/usr/bin/secret-tool").is_file()
            && std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        {
            Some(Box::new(SecretServiceStore {
                program: "/usr/bin/secret-tool".into(),
            }))
        } else {
            None
        };
        Self {
            primary,
            fallback: FileSecretStore::new(fallback_dir, uid),
        }
    }
}
impl SecretStore for PlatformSecretStore {
    fn backend(&self) -> SecretBackend {
        self.primary
            .as_ref()
            .map_or(SecretBackend::File, |p| p.backend())
    }
    fn put(&self, account: &str, secret: &Credential) -> io::Result<SecretBackend> {
        if let Some(primary) = &self.primary
            && let Ok(backend) = primary.put(account, secret)
        {
            let _ = self.fallback.delete(account);
            return Ok(backend);
        }
        self.fallback.put(account, secret)
    }
    fn get(&self, account: &str) -> io::Result<Option<Credential>> {
        if let Some(primary) = &self.primary
            && let Ok(Some(secret)) = primary.get(account)
        {
            return Ok(Some(secret));
        }
        self.fallback.get(account)
    }
    fn delete(&self, account: &str) -> io::Result<()> {
        let primary = self.primary.as_ref().map(|p| p.delete(account));
        self.fallback.delete(account)?;
        primary.unwrap_or(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn uid() -> u32 {
        fs::metadata(std::env::temp_dir()).unwrap();
        // The temp file we create is owned by us; read our uid from it.
        let probe = std::env::temp_dir().join(format!("cm-uid-{}", uuid::Uuid::now_v7()));
        fs::write(&probe, b"").unwrap();
        let uid = fs::metadata(&probe).unwrap().uid();
        fs::remove_file(probe).unwrap();
        uid
    }

    #[test]
    fn file_store_is_private_round_trips_and_refuses_odd_names() {
        let root = std::env::temp_dir().join(format!("cm-secrets-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = FileSecretStore::new(root.join("secrets"), uid());
        let secret = Credential("ab".repeat(32));
        let account = account("0123456789abcdef", "01900000-0000-7000-8000-000000000001");
        assert_eq!(store.get(&account).unwrap(), None);
        assert_eq!(store.put(&account, &secret).unwrap(), SecretBackend::File);
        assert_eq!(store.get(&account).unwrap(), Some(secret.clone()));
        let dir = fs::metadata(root.join("secrets")).unwrap();
        assert_eq!(dir.mode() & 0o777, 0o700);
        let file = fs::read_dir(root.join("secrets"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o600);
        // A widened file is refused rather than trusted.
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.get(&account).is_err());
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o600)).unwrap();
        // A symlink in place of the secret file is never followed.
        let target = root.join("elsewhere");
        fs::write(&target, "ab".repeat(32)).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(file.path()).unwrap();
        std::os::unix::fs::symlink(&target, file.path()).unwrap();
        assert!(store.get(&account).is_err());
        fs::remove_file(file.path()).unwrap();
        store.delete(&account).unwrap();
        store.delete(&account).unwrap();
        assert_eq!(store.get(&account).unwrap(), None);
        for bad in ["../x", "a b", "", "x/y", "ABCDEF;rm"] {
            assert!(store.put(bad, &secret).is_err(), "{bad}");
        }
        assert!(
            store
                .put(&account, &Credential("not hex -w x".into()))
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn memory_store_and_forced_file_fallback() {
        let memory = MemorySecretStore::default();
        let secret = Credential("cd".repeat(32));
        memory.put("aa:bb", &secret).unwrap();
        assert_eq!(memory.get("aa:bb").unwrap(), Some(secret));
        memory.delete("aa:bb").unwrap();
        assert!(memory.is_empty());
        // Without an OS store binary the platform store reports the file fallback.
        let store = PlatformSecretStore {
            primary: None,
            fallback: FileSecretStore::new("/nonexistent/secrets".into(), 0),
        };
        assert_eq!(store.backend(), SecretBackend::File);
    }
}
