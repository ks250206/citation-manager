//! Interim MCP credential sourcing and the owner bootstrap path (#29). **#26 replaces this
//! module** with MCP connection profiles, token storage and elevation; everything that
//! decides *where the MCP secret comes from* or *how an MCP grant is issued* lives here.
//!
//! - The secret is never accepted on argv or in an environment variable value. It is read
//!   from a private file (regular, single link, owned by this user, mode 0600, at most 256
//!   bytes): `--credential-file ABS`, else `$PAPERS_MCP_CREDENTIAL_FILE`, else
//!   `<library>/daemon/mcp.credential`.
//! - The file is re-read before every daemon call, so replacing it (e.g. after the owner
//!   approved a write grant) takes effect in a running server without restart.
//! - `papers-mcp grant` (also `papers mcp grant`) is the documented dev/bootstrap path: it
//!   uses the owner's desktop bootstrap credential (`<library>/daemon/desktop.credential`,
//!   the GrantAdmin grant papersd maintains) to issue an MCP grant and writes its secret to
//!   the credential file. Without `--write` the grant is the read-only preset.
//!
//! The daemon remains the only authority: profiles below are issuance templates and the
//! session ceiling only *narrows* whatever grant the credential maps to.
use cm_protocol::{
    client::{
        ClientError, Session,
        local::{CredentialSource, DaemonPaths, Launch, LocalConnector, read_private_file},
    },
    ipc::{
        Capability, CapabilitySet, Command, Credential, GrantCommand, GrantId, GrantInfo,
        GrantReply, HandshakeRejection, Reply,
    },
};
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub const ENV_CREDENTIAL_FILE: &str = "PAPERS_MCP_CREDENTIAL_FILE";
pub const DEFAULT_FILE_NAME: &str = "mcp.credential";
/// Generous bound for the 256-bit hex secret plus a trailing newline.
pub const MAX_CREDENTIAL_BYTES: u64 = 256;
/// Every grant this module issues carries this label prefix (listing filters on it).
pub const LABEL_PREFIX: &str = "mcp";
/// The diagnostic `client_kind` this server presents. Never authority.
pub const CLIENT_KIND: &str = "mcp";

/// MCP default: `presets::mcp_read_only` (library reads and own-job status).
pub fn read_only() -> CapabilitySet {
    cm_protocol::ipc::CapabilitySet::from([Capability::LibraryRead, Capability::JobsRead])
}
/// Added by a separately approved write grant: register/update papers, organization
/// changes, import commit (LibraryWrite), DOI metadata lookups (NetworkLookup) and
/// add_annotation (AnnotationsWrite: since #26 `LibraryWrite` alone no longer writes
/// annotations). The same set as #26's `profiles::mcp_write` (`papers:write` +
/// `annotations:write`). Delete stays out: LibraryDelete is never requested.
pub fn write_extra() -> CapabilitySet {
    CapabilitySet::from([
        Capability::LibraryWrite,
        Capability::NetworkLookup,
        Capability::AnnotationsWrite,
    ])
}
/// Added by an approved export grant: export_citations has papersd write a new file into
/// this server's private temporary directory (LocalFileWrite), which it reads back. With
/// the read-only base this is #26's `Export` alternative {LibraryRead, LocalFileWrite};
/// `ExportAllowedDir` would not do, since the private temp dir is not an allowed folder.
pub fn export_extra() -> CapabilitySet {
    CapabilitySet::from([Capability::LocalFileWrite])
}

/// Requested (narrowing) set presented in `Hello`. Even a misplaced owner credential is
/// limited to MCP scope: never delete/restore, GrantAdmin, local file reads, UI settings or
/// job control (bulk delete/move elevation is out of scope for #29).
pub fn session_ceiling(read_only_session: bool) -> CapabilitySet {
    let mut set = read_only();
    if !read_only_session {
        set.extend(write_extra());
        set.extend(export_extra());
    }
    set
}

/// Where the server's secret comes from. #26 supplies keychain/profile-backed stores.
pub trait CredentialStore: Send + Sync + 'static {
    /// Called before every (re)connect and request; must be cheap.
    fn load(&self) -> Result<Credential, String>;
    /// Human-readable location for diagnostics (never the secret).
    fn describe(&self) -> String;
}

/// A private credential file (see module docs).
#[derive(Debug, Clone)]
pub struct FileCredential {
    pub path: PathBuf,
    pub uid: u32,
}
impl FileCredential {
    /// `--credential-file`, else `$PAPERS_MCP_CREDENTIAL_FILE`, else the library default.
    pub fn resolve(
        explicit: Option<PathBuf>,
        env: Option<OsString>,
        paths: &DaemonPaths,
        uid: u32,
    ) -> Result<Self, String> {
        let path = match explicit {
            Some(path) => path,
            None => match env.filter(|v| !v.is_empty()) {
                Some(value) => PathBuf::from(value),
                None => paths.state_dir.join(DEFAULT_FILE_NAME),
            },
        };
        if !path.is_absolute() {
            return Err(format!(
                "the credential file path must be absolute ({ENV_CREDENTIAL_FILE} or --credential-file)"
            ));
        }
        // The owner's bootstrap credential (GrantAdmin) must never be served to an AI client,
        // nor overwritten/revoked by `grant`/`revoke`. Compare canonical forms when possible.
        let canonical = |p: &Path| {
            p.parent()
                .and_then(|d| d.canonicalize().ok())
                .zip(p.file_name())
                .map(|(d, n)| d.join(n))
                .unwrap_or_else(|| p.to_owned())
        };
        if path == paths.desktop_credential
            || canonical(&path) == canonical(&paths.desktop_credential)
        {
            return Err(
                "refusing to use the owner's desktop credential as the MCP credential file".into(),
            );
        }
        Ok(Self { path, uid })
    }
}
impl CredentialStore for FileCredential {
    fn load(&self) -> Result<Credential, String> {
        let raw = read_private_file(&self.path, self.uid, MAX_CREDENTIAL_BYTES).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                format!(
                    "no MCP credential at {}; the library owner issues one with \
                     `papers mcp grant --library <LIBRARY>` (read-only by default)",
                    self.path.display()
                )
            } else {
                format!(
                    "cannot use MCP credential file {} (must be a private 0600 regular file owned by you): {e}",
                    self.path.display()
                )
            }
        })?;
        let text = String::from_utf8(raw)
            .map_err(|_| "the MCP credential file is not UTF-8".to_owned())?;
        let secret = text.trim();
        if secret.is_empty() || !secret.chars().all(|c| c.is_ascii_graphic()) {
            return Err("the MCP credential file does not contain a credential".into());
        }
        Ok(Credential(secret.to_owned()))
    }
    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// Writes the secret atomically as a new 0600 file next to `path`, then renames it over.
pub fn write_private_file(path: &Path, secret: &Credential) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let temp = parent.join(temp_name);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&temp)?;
        file.write_all(secret.0.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Which capabilities an issued MCP grant carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Profile {
    pub write: bool,
    pub export: bool,
}
impl Profile {
    pub fn capabilities(self) -> CapabilitySet {
        let mut set = read_only();
        if self.write {
            set.extend(write_extra());
        }
        if self.export {
            set.extend(export_extra());
        }
        set
    }
    pub fn label(self) -> &'static str {
        match (self.write, self.export) {
            (false, false) => "mcp read-only",
            (true, false) => "mcp write",
            (false, true) => "mcp read-only+export",
            (true, true) => "mcp write+export",
        }
    }
    /// Expiry policy. Aligned with #26's `scopes::lifetime` maxima (write 90 d, read-only
    /// 365 d; read-only also keeps its 90 d default) without depending on that branch, but
    /// write/export grants for an AI client default to a short working session.
    pub fn lifetime(self) -> Lifetime {
        if self.write || self.export {
            Lifetime {
                default_ms: 8 * HOUR_MS,
                max_ms: 90 * DAY_MS,
            }
        } else {
            Lifetime {
                default_ms: 90 * DAY_MS,
                max_ms: 365 * DAY_MS,
            }
        }
    }
    /// Absolute expiry for a grant issued at `now_ms` (`ttl_ms` defaults per profile).
    pub fn expires_at(self, ttl_ms: Option<i64>, now_ms: i64) -> Result<i64, String> {
        let life = self.lifetime();
        let ttl = ttl_ms.unwrap_or(life.default_ms);
        if ttl <= 0 {
            return Err("--ttl must be positive".into());
        }
        if ttl > life.max_ms {
            return Err(format!(
                "--ttl exceeds the maximum of {} days for {} grants",
                life.max_ms / DAY_MS,
                self.label()
            ));
        }
        Ok(now_ms.saturating_add(ttl))
    }
}

pub const MINUTE_MS: i64 = 60_000;
pub const HOUR_MS: i64 = 60 * MINUTE_MS;
pub const DAY_MS: i64 = 24 * HOUR_MS;

/// Default and maximum lifetime of an issued MCP grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetime {
    pub default_ms: i64,
    pub max_ms: i64,
}

/// `30m`, `8h`, `7d` (a positive integer and a unit).
pub fn parse_ttl(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let cut = text.char_indices().last().map_or(0, |(i, _)| i);
    let (number, unit) = text.split_at(cut);
    let factor = match unit {
        "m" => MINUTE_MS,
        "h" => HOUR_MS,
        "d" => DAY_MS,
        _ => return Err("--ttl needs a number and a unit: m, h or d (e.g. 8h)".into()),
    };
    number
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .and_then(|n| n.checked_mul(factor))
        .ok_or_else(|| "--ttl needs a positive number and a unit: m, h or d (e.g. 8h)".into())
}

/// The daemon's reserved label of the owner's bootstrap grant (#25).
pub const DESKTOP_BOOTSTRAP_LABEL: &str = "desktop-bootstrap";

/// Only MCP grants may be replaced or revoked by these commands: never the owner's
/// bootstrap grant, another client's grant, or anything holding GrantAdmin.
fn ensure_mcp_grant(label: &str, capabilities: &CapabilitySet) -> Result<(), String> {
    if label == DESKTOP_BOOTSTRAP_LABEL
        || capabilities.contains(&Capability::GrantAdmin)
        || !label.starts_with(LABEL_PREFIX)
    {
        return Err(format!(
            "refusing to touch grant {label:?}: it is not an MCP grant (owner/admin grants are \
             managed in the app)"
        ));
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Owner (GrantAdmin) session over the desktop bootstrap credential, starting papersd.
fn owner_session(paths: &DaemonPaths, uid: u32, launch: Option<Launch>) -> Result<Session, String> {
    let mut connector = LocalConnector::new(paths.clone(), uid, "mcp-bootstrap");
    connector.credential = CredentialSource::DesktopBootstrap;
    connector.launch = launch;
    Session::open(&connector).map_err(|e| {
        format!("cannot open the library as its owner (desktop bootstrap credential): {e}")
    })
}

/// The grant a credential currently maps to, if it still authenticates.
fn grant_of(
    paths: &DaemonPaths,
    uid: u32,
    credential: Credential,
) -> Result<Option<GrantId>, String> {
    let mut connector = LocalConnector::new(paths.clone(), uid, CLIENT_KIND);
    connector.credential = CredentialSource::Explicit(credential);
    match Session::open(&connector) {
        // No narrowing was requested, so these are the grant's full capabilities.
        Ok(session)
            if session
                .welcome
                .capabilities
                .contains(&Capability::GrantAdmin) =>
        {
            Err(
                "the credential file holds an owner (GrantAdmin) credential; refusing to use, \
             replace or revoke it"
                    .into(),
            )
        }
        Ok(session) => Ok(Some(session.welcome.grant)),
        Err(ClientError::Rejected(HandshakeRejection::Unauthenticated)) => Ok(None),
        Err(e) => Err(format!("cannot reach the library daemon: {e}")),
    }
}

fn all_grants(owner: &mut Session) -> Result<Vec<GrantInfo>, String> {
    match owner.call(Command::Grants(GrantCommand::List)) {
        Ok(Ok(Reply::Grants(GrantReply::Grants(grants)))) => Ok(grants),
        Ok(Ok(_)) => Err("unexpected daemon reply".into()),
        Ok(Err(e)) => Err(format!("grant listing refused: {e:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Revokes `grant` after checking (on the daemon's current list) that it is an MCP grant.
fn revoke_with(owner: &mut Session, grant: GrantId) -> Result<(), String> {
    let info = all_grants(owner)?
        .into_iter()
        .find(|g| g.id == grant)
        .ok_or_else(|| format!("grant {grant} does not exist"))?;
    ensure_mcp_grant(&info.label, &info.capabilities)?;
    match owner.call(Command::Grants(GrantCommand::Revoke(grant))) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("revocation of grant {grant} refused: {e:?}")),
        Err(e) => Err(format!("revocation of grant {grant} failed: {e}")),
    }
}

/// Issues a new MCP grant with the owner's desktop credential and stores its secret in
/// `file`. A grant the file held before is revoked afterwards (one live grant per file).
pub fn issue(
    paths: &DaemonPaths,
    uid: u32,
    launch: Option<Launch>,
    profile: Profile,
    ttl_ms: Option<i64>,
    file: &FileCredential,
) -> Result<GrantInfo, String> {
    let expires_at_ms = profile.expires_at(ttl_ms, now_ms())?;
    let mut owner = owner_session(paths, uid, launch)?;
    let previous = match file.load() {
        Ok(credential) => grant_of(paths, uid, credential)?,
        Err(_) => None,
    };
    // Never replace a file whose grant this command may not revoke afterwards.
    if let Some(old) = previous {
        let grants = all_grants(&mut owner)?;
        if let Some(info) = grants.iter().find(|g| g.id == old) {
            ensure_mcp_grant(&info.label, &info.capabilities)?;
        }
    }
    let reply = owner
        .call(Command::Grants(GrantCommand::Issue {
            label: profile.label().into(),
            capabilities: profile.capabilities(),
            expires_at_ms: Some(expires_at_ms),
        }))
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("grant refused: {e:?}"))?;
    let Reply::Grants(GrantReply::Issued { grant, credential }) = reply else {
        return Err("unexpected daemon reply".into());
    };
    if let Err(e) = write_private_file(&file.path, &credential) {
        let undone = revoke_with(&mut owner, grant.id);
        return Err(format!(
            "cannot store the credential at {} ({e}); {}",
            file.path.display(),
            match undone {
                Ok(()) => "the new grant was revoked again".to_owned(),
                Err(r) => r,
            }
        ));
    }
    if let Some(old) = previous.filter(|old| *old != grant.id)
        && let Err(e) = revoke_with(&mut owner, old)
    {
        crate::log::warn(&format!("previous MCP grant {old} is still live: {e}"));
    }
    Ok(grant)
}

/// Revokes `grant`, or the grant `file` maps to (and then removes the file).
pub fn revoke(
    paths: &DaemonPaths,
    uid: u32,
    launch: Option<Launch>,
    grant: Option<GrantId>,
    file: &FileCredential,
) -> Result<GrantId, String> {
    let mut owner = owner_session(paths, uid, launch)?;
    let (target, remove_file) = match grant {
        Some(grant) => (grant, false),
        None => {
            let credential = file.load()?;
            match grant_of(paths, uid, credential)? {
                Some(grant) => (grant, true),
                None => {
                    return Err(format!(
                        "the credential in {} no longer authenticates (already revoked or expired); \
                         delete the file or issue a new grant",
                        file.path.display()
                    ));
                }
            }
        }
    };
    revoke_with(&mut owner, target)?;
    if remove_file {
        fs::remove_file(&file.path)
            .map_err(|e| format!("grant {target} revoked, but removing the file failed: {e}"))?;
    }
    Ok(target)
}

/// MCP grants (label prefix `mcp`) known to the daemon.
pub fn list(
    paths: &DaemonPaths,
    uid: u32,
    launch: Option<Launch>,
) -> Result<Vec<GrantInfo>, String> {
    let mut owner = owner_session(paths, uid, launch)?;
    Ok(all_grants(&mut owner)?
        .into_iter()
        .filter(|g| g.label.starts_with(LABEL_PREFIX))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn euid() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    #[test]
    fn profiles_and_the_session_ceiling_never_include_owner_capabilities() {
        assert_eq!(Profile::default().capabilities(), read_only());
        let all = Profile {
            write: true,
            export: true,
        };
        for forbidden in [
            Capability::GrantAdmin,
            Capability::LibraryDelete,
            Capability::LocalFileRead,
            Capability::UiSettings,
            Capability::JobsControl,
        ] {
            assert!(!all.capabilities().contains(&forbidden));
            assert!(!session_ceiling(false).contains(&forbidden));
        }
        assert_eq!(session_ceiling(true), read_only());
        assert!(Profile::default().label().starts_with(LABEL_PREFIX));
        assert!(all.label().starts_with(LABEL_PREFIX));
    }

    #[test]
    fn each_profile_enables_exactly_its_tools_under_the_daemon_policy() {
        use cm_protocol::ipc::Operation;
        let can = |profile: Profile, operation: Operation| {
            operation
                .requirement()
                .satisfied_by(&profile.capabilities())
        };
        let read = Profile::default();
        let write = Profile {
            write: true,
            export: false,
        };
        let export = Profile {
            write: false,
            export: true,
        };
        // The tools a write grant enables (DESIGN §14.2), add_annotation included: since
        // #26 annotation writes need AnnotationsWrite even where LibraryWrite is held.
        for operation in [
            Operation::MetadataResolve,
            Operation::ImportPreview,
            Operation::ImportCommit,
            Operation::PaperUpdate,
            Operation::OrganizationWrite,
            Operation::AnnotationWrite,
        ] {
            assert!(can(write, operation), "{operation:?}");
            assert!(!can(read, operation), "{operation:?}");
            assert!(!can(export, operation), "{operation:?}");
        }
        // export_citations: the read base plus LocalFileWrite meets #26's Export any-of.
        assert!(can(export, Operation::Export));
        assert!(!can(read, Operation::Export));
        assert!(!can(write, Operation::Export));
        // Never delete, trash or bulk changes, whatever the profile.
        let all = Profile {
            write: true,
            export: true,
        };
        for operation in [
            Operation::PaperTrash,
            Operation::OrganizationDelete,
            Operation::BulkTrash,
            Operation::BulkMove,
            Operation::GrantIssue,
        ] {
            assert!(!can(all, operation), "{operation:?}");
        }
    }

    #[test]
    fn write_grants_expire_soon_by_default_and_ttls_are_capped() {
        let now = 1_000;
        let read = Profile::default();
        let write = Profile {
            write: true,
            export: false,
        };
        let export = Profile {
            write: false,
            export: true,
        };
        assert_eq!(read.expires_at(None, now).unwrap(), now + 90 * DAY_MS);
        assert_eq!(write.expires_at(None, now).unwrap(), now + 8 * HOUR_MS);
        assert_eq!(export.expires_at(None, now).unwrap(), now + 8 * HOUR_MS);
        assert_eq!(
            write.expires_at(Some(90 * DAY_MS), now).unwrap(),
            now + 90 * DAY_MS
        );
        assert!(write.expires_at(Some(91 * DAY_MS), now).is_err());
        assert!(read.expires_at(Some(366 * DAY_MS), now).is_err());
        assert_eq!(parse_ttl("30m").unwrap(), 30 * MINUTE_MS);
        assert_eq!(parse_ttl("8h").unwrap(), 8 * HOUR_MS);
        assert_eq!(parse_ttl("7d").unwrap(), 7 * DAY_MS);
        for bad in [
            "",
            "8",
            "h",
            "0h",
            "-1d",
            "1w",
            "1.5h",
            "8時",
            "99999999999999999d",
        ] {
            assert!(parse_ttl(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_mcp_grants_may_be_replaced_or_revoked() {
        let mcp = read_only();
        assert!(ensure_mcp_grant("mcp write", &mcp).is_ok());
        assert!(ensure_mcp_grant(DESKTOP_BOOTSTRAP_LABEL, &mcp).is_err());
        assert!(ensure_mcp_grant("browser native host", &mcp).is_err());
        let admin = CapabilitySet::from([Capability::GrantAdmin, Capability::LibraryRead]);
        assert!(ensure_mcp_grant("mcp sneaky", &admin).is_err());
    }

    #[test]
    fn credential_files_must_be_private_and_are_written_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.credential");
        let store = FileCredential {
            path: path.clone(),
            uid: euid(),
        };
        assert!(store.load().unwrap_err().contains("papers mcp grant"));
        write_private_file(&path, &Credential("abc123".into())).unwrap();
        assert_eq!(store.load().unwrap(), Credential("abc123".into()));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Replacing keeps exactly one file (no temp leftovers).
        write_private_file(&path, &Credential("def456".into())).unwrap();
        assert_eq!(store.load().unwrap(), Credential("def456".into()));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load().unwrap_err().contains("0600"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, "  \n").unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn resolution_order_is_flag_env_default_and_must_be_absolute() {
        let paths =
            DaemonPaths::new(Path::new("/tmp/cm-mcp-lib"), Some(Path::new("/run/x")), 1).unwrap();
        let default = FileCredential::resolve(None, None, &paths, 1).unwrap();
        assert_eq!(default.path, paths.state_dir.join(DEFAULT_FILE_NAME));
        let env = FileCredential::resolve(None, Some("/e/cred".into()), &paths, 1).unwrap();
        assert_eq!(env.path, PathBuf::from("/e/cred"));
        let flag =
            FileCredential::resolve(Some("/f/cred".into()), Some("/e/cred".into()), &paths, 1)
                .unwrap();
        assert_eq!(flag.path, PathBuf::from("/f/cred"));
        assert!(FileCredential::resolve(Some("rel".into()), None, &paths, 1).is_err());
        // The owner's desktop credential is never accepted, however it is named.
        let desktop = paths.desktop_credential.clone();
        assert!(FileCredential::resolve(Some(desktop.clone()), None, &paths, 1).is_err());
        assert!(FileCredential::resolve(None, Some(desktop.into()), &paths, 1).is_err());
    }
}
