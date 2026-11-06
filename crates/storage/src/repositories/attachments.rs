use crate::error::service_error;
use cm_core::{ServiceError, ports::AttachmentRepository};
use cm_domain::{Attachment, AttachmentId, PaperId, StorageKey};
use rusqlite::{Connection, OptionalExtension, Row};
use uuid::Uuid;

/// Existing attachment read port. Managed byte ingestion and hash verification are #9.
pub struct SqliteAttachments<'a> {
    pub(crate) connection: &'a Connection,
}
const COLUMNS: &str =
    "SELECT a.id,a.paper_id,a.relative_path,a.file_hash,a.original_name,a.mime_type,a.byte_size
    FROM attachments a JOIN papers p ON p.id=a.paper_id WHERE p.deleted_at IS NULL";
impl AttachmentRepository for SqliteAttachments<'_> {
    fn get(&self, id: AttachmentId) -> Result<Option<Attachment>, ServiceError> {
        self.connection
            .query_row(
                &format!("{COLUMNS} AND a.id=?1"),
                [id.0.to_string()],
                decode,
            )
            .optional()
            .map_err(service_error)
    }
    fn for_paper(&self, id: PaperId) -> Result<Vec<Attachment>, ServiceError> {
        self.connection
            .prepare(&format!(
                "{COLUMNS} AND a.paper_id=?1 ORDER BY a.added_at,a.id"
            ))
            .map_err(service_error)?
            .query_map([id.0.to_string()], decode)
            .map_err(service_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(service_error)
    }
}
fn decode(row: &Row<'_>) -> rusqlite::Result<Attachment> {
    fn parse<T: std::str::FromStr>(row: &Row<'_>, index: usize) -> rusqlite::Result<T>
    where
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        let value: String = row.get(index)?;
        value.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
    }
    let key: String = row.get(2)?;
    let storage_key = StorageKey::try_from(key).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let hash: Vec<u8> = row.get(3)?;
    let sha256: [u8; 32] = hash.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(Attachment {
        id: AttachmentId(parse::<Uuid>(row, 0)?),
        paper_id: PaperId(parse::<Uuid>(row, 1)?),
        storage_key,
        sha256,
        original_name: row.get(4)?,
        media_type: row.get(5)?,
        byte_size: u64::try_from(row.get::<_, i64>(6)?)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, -1))?,
    })
}
