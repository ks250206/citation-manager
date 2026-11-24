//! Credential -> daemon-owned grant resolution (#25 security ADR).
//!
//! Credentials are 256-bit random bearer secrets; only their SHA-256 is persisted. Every
//! request re-resolves the grant from this registry, so revocation and expiry apply to
//! live sessions. Self-declared client kind/scopes never create or widen a grant.
//!
//! #26: SHA-256 (no slow KDF) is deliberate: credentials are 256-bit uniformly random, so a
//! stolen hash cannot be brute-forced and a KDF would only add per-request latency. The
//! lookup is keyed by that digest and the match is confirmed with a constant-time compare.
use cm_core::{
    ServiceError,
    policy::{
        self, Capability, CapabilitySet, ClientKind, Denial, Grant, GrantId, Operation, ScopeSet,
    },
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
/// rotate the previous bootstrap grant when its credential file is lost or replaced, and is
/// therefore reserved: `GrantCommand::Issue` cannot create a grant with it.
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
/// Constant-time equality of two digests (no early exit on the first differing byte).
pub fn digest_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Last-use timestamps are kept with minute granularity in memory and written to the
/// database only when the daemon exits (`flush_last_used`): every committed library write
/// invalidates open import previews (daemon-ipc ADR), so a connecting client must not cause
/// one. A crash loses the latest update; the value is informational only.
const LAST_USED_GRANULARITY_MS: i64 = 60_000;

/// Everything needed to create a grant. `issuer = None` only for the daemon's own bootstrap.
pub struct NewGrant<'a> {
    pub issuer: Option<GrantId>,
    pub issuer_requested: Option<&'a CapabilitySet>,
    pub label: &'a str,
    pub capabilities: CapabilitySet,
    pub expires_at_ms: Option<i64>,
    pub client_kind: ClientKind,
    pub scopes: Option<ScopeSet>,
    pub replaces: Option<GrantId>,
}

#[derive(Default)]
struct Cache {
    by_id: HashMap<GrantId, StoredGrant>,
    by_hash: HashMap<[u8; 32], GrantId>,
    /// Grants whose `last_used_at_ms` changed since the last flush.
    used: std::collections::HashSet<GrantId>,
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
    /// This is also the token verification entry point for in-process adapters (#27 REST).
    pub fn authenticate(&self, credential: &Credential, now: i64) -> Result<Grant, Denial> {
        let digest = credential_hash(credential);
        let grant = {
            let cache = self.read();
            let stored = cache
                .by_hash
                .get(&digest)
                .and_then(|id| cache.by_id.get(id))
                .filter(|s| digest_eq(&s.token_sha256, &digest));
            policy::check_live(stored.map(|s| &s.grant), now)
                .cloned()
                .map_err(|_| Denial::Unauthenticated)?
        };
        self.touch(grant.id, now);
        Ok(grant)
    }
    /// Coarse last-use tracking in memory (see `LAST_USED_GRANULARITY_MS`).
    fn touch(&self, id: GrantId, now: i64) {
        if self.read().by_id.get(&id).is_some_and(|s| {
            s.last_used_at_ms
                .is_some_and(|at| now - at < LAST_USED_GRANULARITY_MS)
        }) {
            return;
        }
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        if let Some(stored) = cache.by_id.get_mut(&id) {
            stored.last_used_at_ms = Some(now);
            cache.used.insert(id);
        }
    }
    /// Persists last-use timestamps; called when the daemon stops.
    pub fn flush_last_used(&self) {
        let used: Vec<(GrantId, i64)> = {
            let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<GrantId> = cache.used.drain().collect();
            ids.into_iter()
                .filter_map(|id| {
                    cache
                        .by_id
                        .get(&id)
                        .and_then(|s| s.last_used_at_ms)
                        .map(|at| (id, at))
                })
                .collect()
        };
        let db = self.control.lock().unwrap_or_else(|e| e.into_inner());
        for (id, at) in used {
            let _ = db.daemon_grants().touch(id, at);
        }
    }
    pub fn stored(&self, id: GrantId) -> Option<StoredGrant> {
        self.read().by_id.get(&id).cloned()
    }
    pub fn list_stored(&self) -> Vec<StoredGrant> {
        let mut grants: Vec<StoredGrant> = self.read().by_id.values().cloned().collect();
        grants.sort_by_key(|g| (g.created_at_ms, g.grant.id));
        grants
    }
    /// Effective capabilities of a live session right now (elevation included).
    pub fn held(
        &self,
        grant: GrantId,
        requested: Option<&CapabilitySet>,
        now: i64,
    ) -> Result<CapabilitySet, Denial> {
        let cache = self.read();
        policy::check_live(cache.by_id.get(&grant).map(|s| &s.grant), now)
            .map(|g| policy::effective_at(g, requested, now))
    }
    pub fn kind_of(&self, grant: GrantId) -> Option<ClientKind> {
        self.read().by_id.get(&grant).map(|s| s.client_kind)
    }
    pub fn control(&self) -> &Arc<Mutex<LibraryDb>> {
        &self.control
    }
    /// #26 in-place capability change. Narrowing updates the live registry first (fails
    /// closed like revocation); widening is persisted first, so a failed write never leaves
    /// a wider grant in memory.
    pub fn set_access(
        &self,
        id: GrantId,
        capabilities: CapabilitySet,
        scopes: Option<ScopeSet>,
        expires_at_ms: Option<i64>,
    ) -> Result<bool, ServiceError> {
        let Some(current) = self.stored(id).filter(|s| !s.grant.revoked) else {
            return Ok(false);
        };
        let narrowing = capabilities.is_subset(&current.grant.capabilities)
            && match (expires_at_ms, current.grant.expires_at_ms) {
                (Some(new), Some(old)) => new <= old,
                (Some(_), None) => true,
                (None, old) => old.is_none(),
            };
        let apply = |cache: &mut Cache| {
            if let Some(s) = cache.by_id.get_mut(&id) {
                s.grant.capabilities = capabilities.clone();
                s.grant.expires_at_ms = expires_at_ms;
                s.scopes = scopes.clone();
            }
        };
        if narrowing {
            apply(&mut self.cache.write().unwrap_or_else(|e| e.into_inner()));
        }
        let stored = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .set_access(id, &capabilities, scopes.as_ref(), expires_at_ms)?;
        if !narrowing && stored {
            apply(&mut self.cache.write().unwrap_or_else(|e| e.into_inner()));
        }
        Ok(stored || narrowing)
    }
    /// #26 time-limited elevation (`None` ends it; ending is applied in memory first).
    pub fn set_elevation(&self, id: GrantId, until_ms: Option<i64>) -> Result<bool, ServiceError> {
        let apply = |cache: &mut Cache| {
            cache
                .by_id
                .get_mut(&id)
                .filter(|s| !s.grant.revoked)
                .map(|s| s.grant.elevated_until_ms = until_ms)
                .is_some()
        };
        if until_ms.is_none() {
            apply(&mut self.cache.write().unwrap_or_else(|e| e.into_inner()));
        }
        let stored = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .set_elevation(id, until_ms)?;
        if until_ms.is_some() && stored {
            apply(&mut self.cache.write().unwrap_or_else(|e| e.into_inner()));
        }
        Ok(stored)
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
        self.issue_with(
            NewGrant {
                issuer,
                issuer_requested,
                label,
                capabilities,
                expires_at_ms,
                client_kind: ClientKind::Unspecified,
                scopes: None,
                replaces: None,
            },
            now,
        )
        .map(|(stored, credential)| (info(&stored), credential))
    }
    pub fn issue_with(
        &self,
        spec: NewGrant<'_>,
        now: i64,
    ) -> Result<(StoredGrant, Credential), IssueError> {
        let NewGrant {
            issuer,
            issuer_requested,
            label,
            capabilities,
            expires_at_ms,
            client_kind,
            scopes,
            replaces,
        } = spec;
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
        if issuer.is_some() && label.eq_ignore_ascii_case(DESKTOP_BOOTSTRAP) {
            return Err(IssueError::Invalid("label is reserved"));
        }
        if capabilities.is_empty() {
            return Err(IssueError::Invalid("at least one capability is required"));
        }
        // #26: permanent bulk elevation belongs to the owner's bootstrap grant only.
        if issuer.is_some() && capabilities.contains(&Capability::BulkElevated) {
            return Err(IssueError::Invalid(
                "bulk elevation is only granted for a limited time",
            ));
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
                elevated_until_ms: None,
            },
            token_sha256: credential_hash(&credential),
            issued_by: issuer,
            created_at_ms: now,
            client_kind,
            scopes,
            last_used_at_ms: None,
            replaces,
        };
        self.control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .insert(&stored)
            .map_err(IssueError::Service)?;
        self.cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(stored.clone());
        Ok((stored, credential))
    }
    /// Takes effect for every live session at its next request. Fails closed: the in-memory
    /// registry (which every request consults) is updated first, so the grant stays revoked
    /// for this daemon's lifetime even when persisting the revocation fails (`Err`).
    pub fn revoke(&self, id: GrantId, now: i64) -> Result<bool, ServiceError> {
        let cached = self
            .cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .by_id
            .get_mut(&id)
            .map(|s| s.grant.revoked = true)
            .is_some();
        let stored = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daemon_grants()
            .revoke(id, now)?;
        Ok(stored || cached)
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
            && let Ok(grant) = self.authenticate(&Credential(text.trim().to_owned()), now)
            && grant.label == DESKTOP_BOOTSTRAP
        {
            // Capabilities added by later versions (#26) are granted to the owner in place,
            // keeping the credential and the jobs it owns.
            let preset = policy::presets::desktop();
            if grant.capabilities != preset {
                self.set_access(grant.id, preset, None, None)
                    .map_err(io::Error::other)?;
            }
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
            .issue_with(
                NewGrant {
                    issuer: None,
                    issuer_requested: None,
                    label: DESKTOP_BOOTSTRAP,
                    capabilities: policy::presets::desktop(),
                    expires_at_ms: None,
                    client_kind: ClientKind::Desktop,
                    scopes: None,
                    replaces: None,
                },
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

impl crate::jobs::OwnerAuthority for Authenticator {
    fn owner_capabilities(&self, owner: GrantId, now_ms: i64) -> Result<CapabilitySet, Denial> {
        let cache = self.read();
        policy::check_live(cache.by_id.get(&owner).map(|s| &s.grant), now_ms)
            .map(|g| g.capabilities.clone())
    }
}

/// Display kind: pre-#26 grants are classified by their bootstrap label or native-host
/// capabilities. Diagnostic only; never consulted for authorization.
pub trait StoredGrantExt {
    fn display_kind(&self) -> ClientKind;
}
impl StoredGrantExt for StoredGrant {
    fn display_kind(&self) -> ClientKind {
        let caps = &self.grant.capabilities;
        match self.client_kind {
            ClientKind::Unspecified if self.grant.label == DESKTOP_BOOTSTRAP => ClientKind::Desktop,
            ClientKind::Unspecified
                if !caps.is_empty() && caps.is_subset(&policy::presets::native_host()) =>
            {
                ClientKind::NativeHost
            }
            kind => kind,
        }
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
