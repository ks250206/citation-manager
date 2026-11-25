//! The single place where the CLI chooses the credential it presents and the capabilities it
//! asks for. Authority always comes from the daemon's stored grant: `requested` only narrows it.
//!
//! Sources today:
//! - default: the owner's local desktop bootstrap credential (`<library>/daemon/desktop.credential`,
//!   0600), exactly like the GUI, presented with `client_kind = "cli"`;
//! - `--credential-file` / `PAPERS_CREDENTIAL_FILE`: a credential issued through
//!   `GrantCommand::Issue` (for example a read-only grant), read from a private 0600 file.
//!
//! #26 (CLI bootstrap token flow) plugs in here: add a `CliCredential` variant and map it in
//! `CliCredential::source`; nothing else in the CLI reads credentials.
use crate::output::{CliError, Kind};
use cm_protocol::{
    client::local::{CredentialSource, read_private_file},
    ipc::{Capability, CapabilitySet, Credential},
};
use std::path::{Path, PathBuf};

/// Diagnostic only; the daemon never grants anything because of it.
pub const CLIENT_KIND: &str = "cli";
/// Upper bound for a credential file (the secrets are 64 hex characters).
const MAX_CREDENTIAL_BYTES: u64 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCredential {
    /// The owner's desktop bootstrap credential, written by papersd into the library.
    DesktopBootstrap,
    /// An explicitly issued credential stored in a private file.
    File(PathBuf),
}
impl CliCredential {
    pub fn from_option(file: Option<&Path>) -> Self {
        file.map_or(Self::DesktopBootstrap, |f| Self::File(f.to_owned()))
    }
    /// Resolves to the connector's credential source. File credentials must be a regular,
    /// single-link 0600 file owned by this user (the same rule papersd uses for its own).
    pub fn source(&self, uid: u32) -> Result<CredentialSource, CliError> {
        match self {
            Self::DesktopBootstrap => Ok(CredentialSource::DesktopBootstrap),
            Self::File(path) => {
                let denied = |reason: String| {
                    CliError::new(
                        Kind::PermissionDenied,
                        format!("credential file {}: {reason}", path.display()),
                    )
                };
                let raw = read_private_file(path, uid, MAX_CREDENTIAL_BYTES)
                    .map_err(|e| denied(e.to_string()))?;
                let secret = String::from_utf8(raw)
                    .map_err(|_| denied("not UTF-8".into()))?
                    .trim()
                    .to_owned();
                if secret.is_empty() {
                    return Err(denied("empty".into()));
                }
                Ok(CredentialSource::Explicit(Credential(secret)))
            }
        }
    }
}

/// Minimum capabilities per subcommand. Read commands never ask for write capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// `server status|start|stop`: an authenticated live grant only.
    Status,
    /// list/show/search/fulltext/annotations.
    Read,
    /// DOI lookup + import preview without saving (`add doi --dry-run`).
    LookupPreview,
    /// DOI lookup + preview + commit.
    LookupWrite,
    /// Parse a named local bibliography and preview without saving (`import --dry-run`).
    FilePreview,
    /// Read a named local file and commit (import, add file new/attach).
    FileWrite,
    /// Inspect a local PDF only (`add file --attach-to --dry-run`).
    FileInspect,
    /// Bibliography export to a file the CLI then publishes.
    Export,
}
impl Need {
    pub fn capabilities(self) -> CapabilitySet {
        use Capability::*;
        let set: &[Capability] = match self {
            Self::Status => &[],
            Self::Read => &[LibraryRead],
            Self::LookupPreview => &[NetworkLookup, LibraryRead, CapturePreview],
            Self::LookupWrite => &[NetworkLookup, LibraryRead, LibraryWrite],
            Self::FilePreview => &[LocalFileRead, LibraryRead, CapturePreview],
            Self::FileWrite => &[LocalFileRead, LibraryRead, LibraryWrite],
            Self::FileInspect => &[LocalFileRead, LibraryRead],
            Self::Export => &[LibraryRead, LocalFileWrite],
        };
        set.iter().copied().collect()
    }
}

/// The capability set requested in `Hello`. `--read-only` caps every session at LibraryRead,
/// so a write attempted under it is refused by the daemon (exit 5), not by the CLI.
pub fn requested(need: Need, read_only: bool) -> CapabilitySet {
    match (read_only, need) {
        (false, need) | (true, need @ Need::Status) => need.capabilities(),
        (true, _) => Need::Read.capabilities(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_commands_never_request_write_capabilities() {
        let read = requested(Need::Read, false);
        assert_eq!(read, [Capability::LibraryRead].into_iter().collect());
        for need in [Need::LookupPreview, Need::FilePreview, Need::FileInspect] {
            assert!(!requested(need, false).contains(&Capability::LibraryWrite));
        }
        assert!(requested(Need::Status, false).is_empty());
        for need in [Need::LookupWrite, Need::FileWrite] {
            assert!(requested(need, false).contains(&Capability::LibraryWrite));
        }
        assert!(!requested(Need::Export, false).contains(&Capability::LibraryWrite));
        assert!(!requested(Need::Read, false).contains(&Capability::GrantAdmin));
    }

    #[test]
    fn read_only_caps_every_session_at_library_read() {
        for need in [Need::LookupWrite, Need::FileWrite, Need::Export, Need::Read] {
            assert_eq!(
                requested(need, true),
                [Capability::LibraryRead].into_iter().collect(),
                "{need:?}"
            );
        }
        assert!(requested(Need::Status, true).is_empty());
    }

    #[test]
    fn credential_files_must_be_private() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cred");
        std::fs::write(&path, "secret\n").unwrap();
        let uid = std::fs::metadata(&path).unwrap().uid();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let file = CliCredential::from_option(Some(&path));
        assert_eq!(
            file.source(uid).err().map(|e| e.kind),
            Some(Kind::PermissionDenied)
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        match file.source(uid).unwrap() {
            CredentialSource::Explicit(c) => assert_eq!(c.0, "secret"),
            CredentialSource::DesktopBootstrap => panic!("explicit expected"),
        }
        assert!(matches!(
            CliCredential::from_option(None).source(uid).unwrap(),
            CredentialSource::DesktopBootstrap
        ));
    }
}
