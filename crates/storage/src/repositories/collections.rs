use super::organization::{active_paper, require_id};
use crate::error::service_error;
use cm_core::{
    ServiceError,
    collections::{CollectionInput, CollectionLibrary, DestinationLibrary},
    import::ImportDestination,
};
use cm_domain::{Collection, CollectionId, PaperId};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use uuid::Uuid;

pub struct SqliteCollections<'a> {
    pub(crate) connection: &'a mut Connection,
}
// UUID strings are decoded explicitly: rusqlite's uuid feature is not required.
fn collect(
    db: &Connection,
    sql: &str,
    id: Option<String>,
) -> Result<Vec<Collection>, ServiceError> {
    let mut statement = db.prepare(sql).map_err(service_error)?;
    let mut rows = if let Some(id) = id {
        statement.query([id]).map_err(service_error)?
    } else {
        statement.query([]).map_err(service_error)?
    };
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(service_error)? {
        result.push(decode(row)?);
    }
    Ok(result)
}
fn decode(row: &rusqlite::Row<'_>) -> Result<Collection, ServiceError> {
    let id: String = row.get(0).map_err(service_error)?;
    let parent: Option<String> = row.get(1).map_err(service_error)?;
    Ok(Collection {
        id: CollectionId(Uuid::parse_str(&id).map_err(|_| ServiceError::Unavailable)?),
        parent: parent
            .map(|p| Uuid::parse_str(&p).map(CollectionId))
            .transpose()
            .map_err(|_| ServiceError::Unavailable)?,
        name: row.get(2).map_err(service_error)?,
        sort_order: row.get(3).map_err(service_error)?,
    })
}
pub(crate) fn get(db: &Connection, id: CollectionId) -> Result<Option<Collection>, ServiceError> {
    require_id(id.0)?;
    Ok(collect(
        db,
        "SELECT id,parent_id,name,sort_order FROM collections WHERE id=?1",
        Some(id.0.to_string()),
    )?
    .pop())
}
fn require_collection(db: &Connection, id: CollectionId) -> Result<Collection, ServiceError> {
    get(db, id)?.ok_or(ServiceError::NotFound)
}
impl CollectionLibrary for SqliteCollections<'_> {
    fn create(&mut self, input: CollectionInput) -> Result<Collection, ServiceError> {
        let name = input.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        if let Some(parent) = input.parent {
            require_collection(&tx, parent)?;
        }
        let collection = Collection {
            id: CollectionId(Uuid::now_v7()),
            name,
            parent: input.parent,
            sort_order: input.sort_order,
        };
        tx.execute(
            "INSERT INTO collections(id,parent_id,name,sort_order) VALUES (?1,?2,?3,?4)",
            params![
                collection.id.0.to_string(),
                collection.parent.map(|p| p.0.to_string()),
                collection.name,
                collection.sort_order
            ],
        )
        .map_err(service_error)?;
        tx.commit().map_err(service_error)?;
        Ok(collection)
    }
    fn get(&self, id: CollectionId) -> Result<Option<Collection>, ServiceError> {
        get(self.connection, id)
    }
    fn list(&self) -> Result<Vec<Collection>, ServiceError> {
        collect(
            self.connection,
            "SELECT id,parent_id,name,sort_order FROM collections ORDER BY parent_id,sort_order,id",
            None,
        )
    }
    fn update(
        &mut self,
        id: CollectionId,
        input: CollectionInput,
    ) -> Result<Collection, ServiceError> {
        let name = input.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        require_collection(&tx, id)?;
        if let Some(parent) = input.parent {
            require_collection(&tx, parent)?;
            // UNION (not ALL) also terminates for malformed preexisting cycles.
            let cycle: bool = tx.query_row("WITH RECURSIVE descendants(id) AS (SELECT id FROM collections WHERE id=?1 UNION SELECT c.id FROM collections c JOIN descendants d ON c.parent_id=d.id) SELECT EXISTS(SELECT 1 FROM descendants WHERE id=?2)", params![id.0.to_string(), parent.0.to_string()], |r| r.get(0)).map_err(service_error)?;
            if cycle {
                return Err(ServiceError::InvalidInput("collection cycle".into()));
            }
        }
        tx.execute(
            "UPDATE collections SET parent_id=?1,name=?2,sort_order=?3 WHERE id=?4",
            params![
                input.parent.map(|p| p.0.to_string()),
                name,
                input.sort_order,
                id.0.to_string()
            ],
        )
        .map_err(service_error)?;
        tx.commit().map_err(service_error)?;
        Ok(Collection {
            id,
            parent: input.parent,
            name,
            sort_order: input.sort_order,
        })
    }
    fn delete(&mut self, id: CollectionId) -> Result<(), ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let current = require_collection(&tx, id)?;
        tx.execute(
            "UPDATE collections SET parent_id=?1 WHERE parent_id=?2",
            params![current.parent.map(|p| p.0.to_string()), id.0.to_string()],
        )
        .map_err(service_error)?;
        tx.execute("DELETE FROM collections WHERE id=?1", [id.0.to_string()])
            .map_err(service_error)?;
        tx.commit().map_err(service_error)
    }
    fn set_membership(
        &mut self,
        paper: PaperId,
        collection: CollectionId,
        present: bool,
    ) -> Result<(), ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        active_paper(&tx, paper)?;
        require_collection(&tx, collection)?;
        membership(&tx, paper, collection, present)?;
        tx.commit().map_err(service_error)
    }
    fn for_paper(&self, paper: PaperId) -> Result<Vec<Collection>, ServiceError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        active_paper(&tx, paper)?;
        let rows = collect(
            &tx,
            "SELECT c.id,c.parent_id,c.name,c.sort_order FROM collections c JOIN paper_collections pc ON pc.collection_id=c.id WHERE pc.paper_id=?1 ORDER BY c.parent_id,c.sort_order,c.id",
            Some(paper.0.to_string()),
        )?;
        tx.commit().map_err(service_error)?;
        Ok(rows)
    }
}
fn membership(
    db: &Connection,
    paper: PaperId,
    collection: CollectionId,
    present: bool,
) -> Result<(), ServiceError> {
    let sql = if present {
        "INSERT INTO paper_collections(paper_id,collection_id) VALUES (?1,?2) ON CONFLICT(paper_id,collection_id) DO NOTHING"
    } else {
        "DELETE FROM paper_collections WHERE paper_id=?1 AND collection_id=?2"
    };
    db.execute(sql, params![paper.0.to_string(), collection.0.to_string()])
        .map_err(service_error)?;
    Ok(())
}
/// These helpers intentionally do not begin/commit transactions. #33 can reuse
/// them inside the transaction that also creates/merges papers.
pub(crate) fn validate_destination(
    db: &Connection,
    destination: &ImportDestination,
) -> Result<(), ServiceError> {
    if destination.tags.len() > 100 {
        return Err(ServiceError::InvalidInput(
            "at most 100 destination tags".into(),
        ));
    }
    if let Some(id) = destination.collection {
        require_collection(db, id)?;
    }
    for tag in &destination.tags {
        super::tags::get(db, *tag)?.ok_or(ServiceError::NotFound)?;
    }
    Ok(())
}
pub(crate) fn apply_destination(
    db: &Transaction<'_>,
    paper: PaperId,
    destination: &ImportDestination,
) -> Result<(), ServiceError> {
    active_paper(db, paper)?;
    validate_destination(db, destination)?;
    if let Some(id) = destination.collection {
        membership(db, paper, id, true)?;
    }
    for tag in &destination.tags {
        super::tags::membership(db, paper, *tag, true)?;
    }
    Ok(())
}
impl DestinationLibrary for SqliteCollections<'_> {
    fn validate_destination(&self, destination: &ImportDestination) -> Result<(), ServiceError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        validate_destination(&tx, destination)?;
        tx.commit().map_err(service_error)
    }
    fn apply_destination(
        &mut self,
        paper: PaperId,
        destination: &ImportDestination,
    ) -> Result<(), ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        apply_destination(&tx, paper, destination)?;
        tx.commit().map_err(service_error)
    }
}
