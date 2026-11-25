//! Daemon-owned grant persistence. Only SHA-256 of a credential is stored.
use crate::error::service_error;
use cm_core::{
    ServiceError,
    policy::{Capability, CapabilitySet, ClientKind, Grant, GrantId, Scope, ScopeSet},
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGrant {
    pub grant: Grant,
    pub token_sha256: [u8; 32],
    pub issued_by: Option<GrantId>,
    pub created_at_ms: i64,
    /// #26 metadata. Diagnostic/display only; `grant.capabilities` is the authority.
    pub client_kind: ClientKind,
    pub scopes: Option<ScopeSet>,
    pub last_used_at_ms: Option<i64>,
    pub replaces: Option<GrantId>,
}

pub struct SqliteGrants<'a> {
    pub(crate) connection: &'a Connection,
}
const COLUMNS: &str = "SELECT id, token_sha256, label, capabilities_json, issued_by, created_at_ms, expires_at_ms, revoked_at_ms, client_kind, scopes_json, last_used_at_ms, elevated_until_ms, replaces FROM daemon_grants";
fn json<T: serde::Serialize>(value: &T) -> Result<String, ServiceError> {
    serde_json::to_string(value).map_err(|e| ServiceError::InvalidInput(e.to_string()))
}
impl SqliteGrants<'_> {
    pub fn insert(&self, stored: &StoredGrant) -> Result<(), ServiceError> {
        let capabilities = json(&stored.grant.capabilities)?;
        let scopes = stored.scopes.as_ref().map(json).transpose()?;
        self.connection
            .execute(
                "INSERT INTO daemon_grants(id, token_sha256, label, capabilities_json, issued_by, created_at_ms, expires_at_ms, revoked_at_ms,
                                           client_kind, scopes_json, last_used_at_ms, elevated_until_ms, replaces)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, NULL, ?10, ?11)",
                params![
                    stored.grant.id.0.to_string(),
                    stored.token_sha256.as_slice(),
                    stored.grant.label,
                    capabilities,
                    stored.issued_by.map(|g| g.0.to_string()),
                    stored.created_at_ms,
                    stored.grant.expires_at_ms,
                    stored.client_kind.name(),
                    scopes,
                    stored.grant.elevated_until_ms,
                    stored.replaces.map(|g| g.0.to_string()),
                ],
            )
            .map(|_| ())
            .map_err(service_error)
    }
    pub fn all(&self) -> Result<Vec<StoredGrant>, ServiceError> {
        self.connection
            .prepare(&format!("{COLUMNS} ORDER BY created_at_ms, id"))
            .map_err(service_error)?
            .query_map([], decode)
            .map_err(service_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(service_error)
    }
    pub fn by_token_hash(&self, hash: &[u8; 32]) -> Result<Option<StoredGrant>, ServiceError> {
        self.connection
            .query_row(
                &format!("{COLUMNS} WHERE token_sha256 = ?1"),
                [hash.as_slice()],
                decode,
            )
            .optional()
            .map_err(service_error)
    }
    pub fn by_id(&self, id: GrantId) -> Result<Option<StoredGrant>, ServiceError> {
        self.connection
            .query_row(
                &format!("{COLUMNS} WHERE id = ?1"),
                [id.0.to_string()],
                decode,
            )
            .optional()
            .map_err(service_error)
    }
    /// Idempotent; returns whether the grant exists.
    pub fn revoke(&self, id: GrantId, now_ms: i64) -> Result<bool, ServiceError> {
        self.connection
            .execute(
                "UPDATE daemon_grants SET revoked_at_ms = coalesce(revoked_at_ms, ?2) WHERE id = ?1",
                params![id.0.to_string(), now_ms],
            )
            .map(|n| n == 1)
            .map_err(service_error)
    }
    /// #26 in-place scope change of a live grant (capabilities are the authority).
    pub fn set_access(
        &self,
        id: GrantId,
        capabilities: &CapabilitySet,
        scopes: Option<&ScopeSet>,
        expires_at_ms: Option<i64>,
    ) -> Result<bool, ServiceError> {
        let scopes = scopes.map(json).transpose()?;
        self.connection
            .execute(
                "UPDATE daemon_grants SET capabilities_json = ?2, scopes_json = ?3, expires_at_ms = ?4
                 WHERE id = ?1 AND revoked_at_ms IS NULL",
                params![id.0.to_string(), json(capabilities)?, scopes, expires_at_ms],
            )
            .map(|n| n == 1)
            .map_err(service_error)
    }
    /// #26 time-limited elevation (`None` ends it).
    pub fn set_elevation(&self, id: GrantId, until_ms: Option<i64>) -> Result<bool, ServiceError> {
        self.connection
            .execute(
                "UPDATE daemon_grants SET elevated_until_ms = ?2 WHERE id = ?1 AND revoked_at_ms IS NULL",
                params![id.0.to_string(), until_ms],
            )
            .map(|n| n == 1)
            .map_err(service_error)
    }
    /// #26 coarse last-use timestamp.
    pub fn touch(&self, id: GrantId, at_ms: i64) -> Result<(), ServiceError> {
        self.connection
            .execute(
                "UPDATE daemon_grants SET last_used_at_ms = max(coalesce(last_used_at_ms, 0), ?2) WHERE id = ?1",
                params![id.0.to_string(), at_ms],
            )
            .map(|_| ())
            .map_err(service_error)
    }
}

fn decode(row: &Row<'_>) -> rusqlite::Result<StoredGrant> {
    let uuid = |index: usize, value: String| {
        Uuid::parse_str(&value).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
    };
    let hash: Vec<u8> = row.get(1)?;
    let token_sha256: [u8; 32] = hash.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
    let raw: String = row.get(3)?;
    // Unknown (newer) capability names are dropped: fewer rights, never more.
    let capabilities: CapabilitySet = serde_json::from_str::<Vec<serde_json::Value>>(&raw)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Capability>(v).ok())
        .collect();
    let issued_by: Option<String> = row.get(4)?;
    let revoked_at_ms: Option<i64> = row.get(7)?;
    let kind: String = row.get(8)?;
    let scopes: Option<String> = row.get(9)?;
    // Unknown scope names are dropped like unknown capabilities (display only).
    let scopes = scopes.map(|raw| {
        serde_json::from_str::<Vec<serde_json::Value>>(&raw)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Scope>(v).ok())
            .collect()
    });
    let replaces: Option<String> = row.get(12)?;
    Ok(StoredGrant {
        grant: Grant {
            id: GrantId(uuid(0, row.get(0)?)?),
            label: row.get(2)?,
            capabilities,
            expires_at_ms: row.get(6)?,
            revoked: revoked_at_ms.is_some(),
            elevated_until_ms: row.get(11)?,
        },
        token_sha256,
        issued_by: issued_by.map(|v| uuid(4, v).map(GrantId)).transpose()?,
        created_at_ms: row.get(5)?,
        client_kind: ClientKind::parse(&kind).unwrap_or(ClientKind::Unspecified),
        scopes,
        last_used_at_ms: row.get(10)?,
        replaces: replaces.map(|v| uuid(12, v).map(GrantId)).transpose()?,
    })
}
