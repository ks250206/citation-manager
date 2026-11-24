//! Credential -> daemon-owned grant resolution (#25 security ADR).
//!
//! Credentials are 256-bit random bearer secrets; only their SHA-256 is persisted. Every
//! request re-resolves the grant from this registry, so revocation and expiry apply to
//! live sessions. Self-declared client kind/scopes never create or widen a grant.
use cm_core::{
    ServiceError,
    policy::{self, CapabilitySet, Denial, Grant, GrantId, Operation},
};
use cm_protocol::ipc::{Credential, GrantInfo};
use cm_storage::{LibraryDb, daemon::StoredGrant};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{Arc, Mutex, RwLock},
};

/// Label of the owner's bootstrap grant. A label is diagnostic; this one is only used to
/// rotate the previous bootstrap grant when its credential file is lost or replaced.
pub const DESKTOP_BOOTSTRAP: &str = "desktop-bootstrap";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_credential() -> io::Result<Credential> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(Credential(
        bytes.iter().map(|b| format!("{b:02x}")).collect(),
    ))
}
pub fn credential_hash(credential: &Credential) -> [u8; 32] {
    Sha256::digest(credential.0.as_bytes()).into()
}

#[derive(Default)]
struct Cache {
    by_id: HashMap<GrantId, StoredGrant>,
    by_hash: HashMap<[u8; 32], GrantId>,
}
impl Cache {
    fn insert(&mut self, stored: StoredGrant) {
        self.by_hash.insert(stored.token_sha256, stored.grant.id);
        self.by_id.insert(stored.grant.id, stored);
    }
}

pub struct Authenticator {
    control: Arc<Mutex<LibraryDb>>,
    cache: RwLock<Cache>,
}
impl Authenticator {
    pub fn load(control: Arc<Mutex<LibraryDb>>) -> Result<Self, ServiceError> {
        let mut cache = Cache::default();
        for stored in control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .all()?
        {
            cache.insert(stored);
        }
        Ok(Self {
            control,
            cache: RwLock::new(cache),
        })
    }
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Cache> {
        self.cache.read().unwrap_or_else(|e| e.into_inner())
    }
    /// Handshake: unknown/revoked/expired credentials are all `Unauthenticated` on the wire.
    pub fn authenticate(&self, credential: &Credential, now: i64) -> Result<Grant, Denial> {
        let cache = self.read();
        let grant = cache
            .by_hash
            .get(&credential_hash(credential))
            .and_then(|id| cache.by_id.get(id))
            .map(|s| &s.grant);
        policy::check_live(grant, now)
            .cloned()
            .map_err(|_| Denial::Unauthenticated)
    }
    /// Per-request decision against the *current* stored grant.
    pub fn authorize(
        &self,
        grant: GrantId,
        requested: Option<&CapabilitySet>,
        operation: Operation,
        now: i64,
    ) -> Result<CapabilitySet, Denial> {
        let cache = self.read();
        policy::authorize(
            cache.by_id.get(&grant).map(|s| &s.grant),
            requested,
            operation,
            now,
        )
    }
    pub fn issue(
        &self,
        issuer: Option<GrantId>,
        issuer_requested: Option<&CapabilitySet>,
        label: &str,
        capabilities: CapabilitySet,
        expires_at_ms: Option<i64>,
        now: i64,
    ) -> Result<(GrantInfo, Credential), IssueError> {
        if let Some(issuer) = issuer {
            let cache = self.read();
            policy::may_issue(
                cache.by_id.get(&issuer).map(|s| &s.grant),
                issuer_requested,
                &capabilities,
                now,
            )
            .map_err(IssueError::Denied)?;
        }
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 128 || label.chars().any(char::is_control) {
            return Err(IssueError::Invalid(
                "label must be 1..=128 printable characters",
            ));
        }
        if capabilities.is_empty() {
            return Err(IssueError::Invalid("at least one capability is required"));
        }
        if expires_at_ms.is_some_and(|at| at <= now) {
            return Err(IssueError::Invalid("expiry must be in the future"));
        }
        let credential = new_credential().map_err(|_| IssueError::Unavailable)?;
        let stored = StoredGrant {
            grant: Grant {
                id: GrantId(uuid_v7()),
                label: label.into(),
                capabilities,
                expires_at_ms,
                revoked: false,
            },
            token_sha256: credential_hash(&credential),
            issued_by: issuer,
            created_at_ms: now,
        };
        self.control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .insert(&stored)
            .map_err(IssueError::Service)?;
        let info = info(&stored);
        self.cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(stored);
        Ok((info, credential))
    }
    /// Takes effect for every live session at its next request.
    pub fn revoke(&self, id: GrantId, now: i64) -> Result<bool, ServiceError> {
        let found = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .revoke(id, now)?;
        if let Some(s) = self
            .cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .by_id
            .get_mut(&id)
        {
            s.grant.revoked = true;
        }
        Ok(found)
    }
    pub fn list(&self) -> Vec<GrantInfo> {
        let mut grants: Vec<GrantInfo> = self.read().by_id.values().map(info).collect();
        grants.sort_by_key(|g| (g.created_at_ms, g.id));
        grants
    }
    /// Ensures the owner's credential file matches a live bootstrap grant; otherwise rotates:
    /// revokes previous bootstrap grants, issues a new desktop grant and atomically writes the
    /// secret (0600) into the private state directory. Same-UID processes can read it; see ADR.
    pub fn bootstrap_desktop(&self, path: &Path, now: i64) -> io::Result<()> {
        if let Ok(raw) = crate::lifecycle::read_private_file(path, crate::lifecycle::euid(), 256)
            && let Ok(text) = String::from_utf8(raw)
            && self
                .authenticate(&Credential(text.trim().to_owned()), now)
                .is_ok_and(|g| g.label == DESKTOP_BOOTSTRAP)
        {
            return Ok(());
        }
        let stale: Vec<GrantId> = self
            .read()
            .by_id
            .values()
            .filter(|s| s.grant.label == DESKTOP_BOOTSTRAP && !s.grant.revoked)
            .map(|s| s.grant.id)
            .collect();
        for id in stale {
            self.revoke(id, now).map_err(io::Error::other)?;
        }
        let (_, credential) = self
            .issue(
                None,
                None,
                DESKTOP_BOOTSTRAP,
                policy::presets::desktop(),
                None,
                now,
            )
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
        let temp = path.with_extension(format!("tmp-{}", std::process::id()));
        let _ = fs::remove_file(&temp);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(credential.0.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueError {
    Denied(Denial),
    Invalid(&'static str),
    Service(ServiceError),
    Unavailable,
}

fn info(stored: &StoredGrant) -> GrantInfo {
    GrantInfo {
        id: stored.grant.id,
        label: stored.grant.label.clone(),
        capabilities: stored.grant.capabilities.clone(),
        created_at_ms: stored.created_at_ms,
        expires_at_ms: stored.grant.expires_at_ms,
        revoked: stored.grant.revoked,
    }
}

fn uuid_v7() -> uuid::Uuid {
    uuid::Uuid::now_v7()
}

/// OS-level peer check. Linux: SO_PEERCRED UID must equal ours. macOS: the private 0700
/// runtime directory is the OS boundary (LOCAL_PEERCRED is not exposed by rustix; #34).
pub fn peer_allowed(stream: &std::os::unix::net::UnixStream) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        rustix::net::sockopt::socket_peercred(stream)
            .is_ok_and(|cred| cred.uid.as_raw() == crate::lifecycle::euid())
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = stream;
        true
    }
}
