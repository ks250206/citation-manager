use super::organization::{active_paper, require_id};
use crate::error::service_error;
use cm_core::{
    ServiceError,
    tags::{TagInput, TagLibrary},
};
use cm_domain::{PaperId, Tag, TagId, TagKey};
use rusqlite::{Connection, TransactionBehavior, params};
use uuid::Uuid;

pub struct SqliteTags<'a> {
    pub(crate) connection: &'a mut Connection,
}
fn collect(db: &Connection, sql: &str, value: Option<&str>) -> Result<Vec<Tag>, ServiceError> {
    let mut statement = db.prepare(sql).map_err(service_error)?;
    let mut rows = if let Some(value) = value {
        statement.query([value]).map_err(service_error)?
    } else {
        statement.query([]).map_err(service_error)?
    };
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(service_error)? {
        let id: String = row.get(0).map_err(service_error)?;
        let key: String = row.get(2).map_err(service_error)?;
        let parsed = TagKey::new(&key).map_err(|_| ServiceError::Unavailable)?;
        if parsed.as_str() != key {
            return Err(ServiceError::Unavailable);
        }
        result.push(Tag {
            id: TagId(Uuid::parse_str(&id).map_err(|_| ServiceError::Unavailable)?),
            name: row.get(1).map_err(service_error)?,
            key: parsed,
            color: row.get(3).map_err(service_error)?,
        });
    }
    Ok(result)
}
pub(crate) fn get(db: &Connection, id: TagId) -> Result<Option<Tag>, ServiceError> {
    require_id(id.0)?;
    Ok(collect(
        db,
        "SELECT id,name,normalized_name,color FROM tags WHERE id=?1",
        Some(&id.0.to_string()),
    )?
    .pop())
}
pub(crate) fn membership(
    db: &Connection,
    paper: PaperId,
    tag: TagId,
    present: bool,
) -> Result<(), ServiceError> {
    let sql = if present {
        "INSERT INTO paper_tags(paper_id,tag_id) VALUES (?1,?2) ON CONFLICT(paper_id,tag_id) DO NOTHING"
    } else {
        "DELETE FROM paper_tags WHERE paper_id=?1 AND tag_id=?2"
    };
    db.execute(sql, params![paper.0.to_string(), tag.0.to_string()])
        .map_err(service_error)?;
    Ok(())
}
impl TagLibrary for SqliteTags<'_> {
    fn create(&mut self, input: TagInput) -> Result<Tag, ServiceError> {
        let (name, key) = input.validate()?;
        let tag = Tag {
            id: TagId(Uuid::now_v7()),
            name,
            key,
            color: input.color,
        };
        self.connection
            .execute(
                "INSERT INTO tags(id,name,normalized_name,color) VALUES (?1,?2,?3,?4)",
                params![tag.id.0.to_string(), tag.name, tag.key.as_str(), tag.color],
            )
            .map_err(service_error)?;
        Ok(tag)
    }
    fn get(&self, id: TagId) -> Result<Option<Tag>, ServiceError> {
        get(self.connection, id)
    }
    fn find(&self, key: &TagKey) -> Result<Option<Tag>, ServiceError> {
        Ok(collect(
            self.connection,
            "SELECT id,name,normalized_name,color FROM tags WHERE normalized_name=?1",
            Some(key.as_str()),
        )?
        .pop())
    }
    fn list(&self) -> Result<Vec<Tag>, ServiceError> {
        collect(
            self.connection,
            "SELECT id,name,normalized_name,color FROM tags ORDER BY normalized_name,id",
            None,
        )
    }
    fn update(&mut self, id: TagId, input: TagInput) -> Result<Tag, ServiceError> {
        require_id(id.0)?;
        let (name, key) = input.validate()?;
        let count = self
            .connection
            .execute(
                "UPDATE tags SET name=?1,normalized_name=?2,color=?3 WHERE id=?4",
                params![name, key.as_str(), input.color, id.0.to_string()],
            )
            .map_err(service_error)?;
        if count == 0 {
            return Err(ServiceError::NotFound);
        }
        Ok(Tag {
            id,
            name,
            key,
            color: input.color,
        })
    }
    fn delete(&mut self, id: TagId) -> Result<(), ServiceError> {
        require_id(id.0)?;
        if self
            .connection
            .execute("DELETE FROM tags WHERE id=?1", [id.0.to_string()])
            .map_err(service_error)?
            == 0
        {
            return Err(ServiceError::NotFound);
        }
        Ok(())
    }
    fn set_membership(
        &mut self,
        paper: PaperId,
        tag: TagId,
        present: bool,
    ) -> Result<(), ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        active_paper(&tx, paper)?;
        get(&tx, tag)?.ok_or(ServiceError::NotFound)?;
        membership(&tx, paper, tag, present)?;
        tx.commit().map_err(service_error)
    }
    fn for_paper(&self, paper: PaperId) -> Result<Vec<Tag>, ServiceError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        active_paper(&tx, paper)?;
        let tags = collect(
            &tx,
            "SELECT t.id,t.name,t.normalized_name,t.color FROM tags t JOIN paper_tags pt ON pt.tag_id=t.id WHERE pt.paper_id=?1 ORDER BY t.normalized_name,t.id",
            Some(&paper.0.to_string()),
        )?;
        tx.commit().map_err(service_error)?;
        Ok(tags)
    }
}
