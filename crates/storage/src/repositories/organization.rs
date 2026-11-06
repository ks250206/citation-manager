use crate::error::service_error;
use cm_core::ServiceError;
use cm_domain::PaperId;
use rusqlite::Connection;
use uuid::Uuid;

pub(super) fn require_id(id: Uuid) -> Result<(), ServiceError> {
    if id.is_nil() {
        return Err(ServiceError::InvalidInput("non-nil ID required".into()));
    }
    Ok(())
}
pub(super) fn active_paper(db: &Connection, paper: PaperId) -> Result<(), ServiceError> {
    require_id(paper.0)?;
    let found: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM papers WHERE id=?1 AND deleted_at IS NULL)",
            [paper.0.to_string()],
            |r| r.get(0),
        )
        .map_err(service_error)?;
    if found {
        Ok(())
    } else {
        Err(ServiceError::NotFound)
    }
}
