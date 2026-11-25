//! #26 "API & Integrations" screen state, headless so it is tested without GPUI.
//!
//! The screen only *asks* the daemon (`Command::Api`); every decision (scopes, expiry,
//! elevation, revocation) is made there. A newly issued secret is held here only until the
//! person presses Done or leaves the screen, and is never written to settings or logs.
use cm_protocol::{
    api::{
        ApiOverview, ApiSettings, AuditEntry, BindAddress, ClientKind, IssueToken, Scope, ScopeSet,
        SecretBackend, TokenInfo, lifetime, profiles,
    },
    ipc::GrantId,
};
use std::path::PathBuf;

/// Scopes offered as toggles when issuing a token (elevation is a separate action).
pub const ISSUABLE: [Scope; 9] = [
    Scope::PapersRead,
    Scope::PapersWrite,
    Scope::PapersDelete,
    Scope::FilesRead,
    Scope::AnnotationsRead,
    Scope::AnnotationsWrite,
    Scope::ExportsWrite,
    Scope::JobsRead,
    Scope::JobsWrite,
];
pub const ELEVATION_MINUTES: i64 = 15;
pub const ELEVATE_WARNING: &str = "Bulk delete/move without the per-minute limit for 15 minutes. \
    Only elevate right before a bulk operation you asked the client to do.";
pub const FILE_STORE_WARNING: &str = "No OS keyring is available: token copies are kept in \
    0600 files in your user data folder, outside the library (readable by any program running \
    as you, and copied by home-folder backups).";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenForm {
    pub kind: ClientKind,
    pub scopes: ScopeSet,
    /// Days until expiry as typed; empty = the policy default.
    pub expiry_days: String,
}
impl Default for TokenForm {
    fn default() -> Self {
        Self {
            kind: ClientKind::Api,
            scopes: [Scope::PapersRead].into(),
            expiry_days: String::new(),
        }
    }
}
impl TokenForm {
    pub fn toggle(&mut self, scope: Scope) {
        if !self.scopes.remove(&scope) {
            self.scopes.insert(scope);
        }
    }
    /// Default and maximum lifetime in days for the selected scopes.
    pub fn lifetime_days(&self) -> (i64, i64) {
        let l = lifetime(&self.scopes);
        (l.default_ms / DAY_MS, l.max_ms / DAY_MS)
    }
    /// A request the daemon will validate again. Errors are shown next to the form.
    pub fn request(&self, label: &str, now_ms: i64) -> Result<IssueToken, String> {
        let label = label.trim();
        if label.is_empty() {
            return Err("Name the token (e.g. the client that will use it)".into());
        }
        if self.scopes.is_empty() {
            return Err("Choose at least one scope".into());
        }
        let (_, max) = self.lifetime_days();
        let expires_at_ms = match self.expiry_days.trim() {
            "" => None,
            days => {
                let days: i64 = days
                    .parse()
                    .ok()
                    .filter(|d| (1..=max).contains(d))
                    .ok_or_else(|| format!("Expiry must be 1–{max} days for these scopes"))?;
                Some(now_ms + days * DAY_MS)
            }
        };
        Ok(IssueToken {
            label: label.to_owned(),
            kind: self.kind,
            scopes: self.scopes.clone(),
            expires_at_ms,
        })
    }
}

/// Secret bytes that are overwritten when dropped. Not `Clone`: the one copy lives in
/// `RevealedSecret` until it is concealed. (Copies GPUI makes while the secret is
/// revealed or on the clipboard are outside this wrapper's control.)
pub struct SecretText(Vec<u8>);
impl SecretText {
    pub fn new(secret: String) -> Self {
        Self(secret.into_bytes())
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }
    /// Shown instead of the secret until the person asks to reveal it.
    pub fn masked(&self) -> String {
        "•".repeat(self.0.len().min(32))
    }
}
impl Drop for SecretText {
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&self.0);
    }
}
impl PartialEq<str> for SecretText {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}

/// The secret of a just-issued token: shown once (masked until revealed), then dropped.
pub struct RevealedSecret {
    pub token: TokenInfo,
    pub secret: SecretText,
    pub stored_in: Option<SecretBackend>,
    pub copied: bool,
    /// Masked by default; "Reveal" shows it.
    pub shown: bool,
}
impl std::fmt::Debug for RevealedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevealedSecret")
            .field("token", &self.token.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// How long a copied secret may stay on the clipboard.
pub const CLIPBOARD_CLEAR_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// Which request a reply answers; stale list/overview replies are dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplyKind {
    Overview,
    Tokens,
    Audit,
}

/// Actions that need a second, explicit click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    AllowWrites(GrantId),
    Elevate(GrantId),
    Revoke(GrantId),
    Reissue(GrantId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// REST API and CLI tokens.
    Tokens,
    Mcp,
    NativeHosts,
    Owner,
}
pub fn group(kind: ClientKind) -> Group {
    match kind {
        ClientKind::Mcp => Group::Mcp,
        ClientKind::NativeHost => Group::NativeHosts,
        ClientKind::Desktop => Group::Owner,
        _ => Group::Tokens,
    }
}

#[derive(Debug, Default)]
pub struct ApiSettingsState {
    pub overview: Option<ApiOverview>,
    pub tokens: Vec<TokenInfo>,
    pub audit: Vec<AuditEntry>,
    pub form: TokenForm,
    pub enabled: bool,
    pub bind: BindAddress,
    pub export_dirs: Vec<PathBuf>,
    pub revealed: Option<RevealedSecret>,
    pub confirm: Option<Confirm>,
    pub show_revoked: bool,
    pub notice: Option<(String, bool)>,
    /// Whether the settings draft differs from what the daemon last returned.
    pub dirty: bool,
    /// Request sequence numbers: the next one, and the latest sent per reply kind.
    pub next_seq: u64,
    pub latest: std::collections::HashMap<ReplyKind, u64>,
}

pub const DAY_MS: i64 = 86_400_000;

impl ApiSettingsState {
    /// A refresh keeps an unsaved draft (only `saved` replaces it).
    pub fn set_overview(&mut self, overview: ApiOverview, saved: bool) {
        if saved || !self.dirty {
            self.enabled = overview.settings.enabled;
            self.bind = overview.settings.bind;
            self.export_dirs = overview.settings.export_dirs.clone();
            self.dirty = false;
        }
        self.overview = Some(overview);
    }
    /// A sequence number for a new request of `kind` (it becomes the latest).
    pub fn begin(&mut self, kind: Option<ReplyKind>) -> u64 {
        self.next_seq += 1;
        if let Some(kind) = kind {
            self.latest.insert(kind, self.next_seq);
        }
        self.next_seq
    }
    /// Whether a reply to request `seq` of `kind` is still the latest one.
    pub fn current(&self, kind: ReplyKind, seq: u64) -> bool {
        self.latest.get(&kind).is_none_or(|latest| *latest == seq)
    }
    /// The settings to save, with the port as typed.
    pub fn settings_request(&self, port: &str) -> Result<ApiSettings, String> {
        let port: u16 = port
            .trim()
            .parse()
            .map_err(|_| "Port must be a number between 1024 and 65535".to_owned())?;
        let settings = ApiSettings {
            enabled: self.enabled,
            bind: self.bind,
            port,
            export_dirs: self.export_dirs.clone(),
        };
        settings.validate().map_err(|e| e.to_string())?;
        Ok(settings)
    }
    pub fn rows(&self, which: Group) -> impl Iterator<Item = &TokenInfo> {
        self.tokens
            .iter()
            .filter(move |t| group(t.kind) == which && (self.show_revoked || !t.revoked))
    }
    pub fn token(&self, id: GrantId) -> Option<&TokenInfo> {
        self.tokens.iter().find(|t| t.id == id)
    }
    /// Replace (or add) one token after an update.
    pub fn upsert(&mut self, token: TokenInfo) {
        match self.tokens.iter_mut().find(|t| t.id == token.id) {
            Some(slot) => *slot = token,
            None => self.tokens.push(token),
        }
    }
    /// Forget a shown secret (Done, leaving the screen, or issuing another).
    pub fn conceal(&mut self) {
        self.revealed = None;
    }
    pub fn secret_backend(&self) -> Option<SecretBackend> {
        self.overview.as_ref().map(|o| o.secret_backend)
    }
}

/// Whether an MCP profile currently allows writes.
pub fn allows_writes(token: &TokenInfo) -> bool {
    token.scopes.iter().any(|s| s.is_write())
}
/// The scopes "Allow writes" / "Make read-only" switch the profile to.
pub fn toggled_mcp_scopes(token: &TokenInfo) -> ScopeSet {
    if allows_writes(token) {
        profiles::mcp_read_only()
    } else {
        profiles::mcp_write()
    }
}

pub fn status(token: &TokenInfo, now_ms: i64) -> &'static str {
    if token.revoked {
        "revoked"
    } else if token.expires_at_ms.is_some_and(|at| at <= now_ms) {
        "expired"
    } else if token.elevated_until_ms.is_some_and(|at| at > now_ms) {
        "elevated"
    } else {
        "active"
    }
}
pub fn scope_list(scopes: &ScopeSet) -> String {
    if scopes.is_empty() {
        return "—".into();
    }
    scopes
        .iter()
        .map(|s| s.name())
        .collect::<Vec<_>>()
        .join(" ")
}

/// UTC calendar date of epoch milliseconds (no time-zone database in the GUI).
pub fn format_date(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "never".into();
    };
    let days = ms.div_euclid(DAY_MS);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
/// `YYYY-MM-DD HH:MM UTC`.
pub fn format_time(ms: i64) -> String {
    let minutes = ms.rem_euclid(DAY_MS) / 60_000;
    format!(
        "{} {:02}:{:02} UTC",
        format_date(Some(ms)),
        minutes / 60,
        minutes % 60
    )
}
