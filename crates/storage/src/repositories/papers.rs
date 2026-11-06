use crate::error::service_error;
use cm_core::{
    ServiceError,
    papers::{NewPaper, PaperLibrary, PaperList, PaperVisibility, validate_metadata},
    ports::PaperRepository,
};
use cm_domain::{Bibliography, Contributor, ExternalId, Paper, PaperId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, named_params, params};
use uuid::Uuid;

pub struct SqlitePapers<'a> {
    pub(crate) connection: &'a mut Connection,
}

impl PaperRepository for SqlitePapers<'_> {
    fn get(&self, id: PaperId) -> Result<Option<Paper>, ServiceError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        let paper = read(&tx, id, false)?;
        tx.commit().map_err(service_error)?;
        Ok(paper)
    }
    fn save(&mut self, paper: &Paper) -> Result<(), ServiceError> {
        if paper.id.0.is_nil() {
            return Err(ServiceError::InvalidInput(
                "internal paper ID is required".into(),
            ));
        }
        self.insert(paper.clone()).map(|_| ())
    }
}

impl SqlitePapers<'_> {
    fn insert(&mut self, mut paper: Paper) -> Result<Paper, ServiceError> {
        validate_metadata(&mut paper)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let now = timestamp(&tx)?;
        paper.created_at = Some(now.clone());
        paper.updated_at = Some(now);
        paper.deleted_at = None;
        write_metadata(&tx, &paper, true)?;
        write_relations(&tx, &paper)?;
        tx.commit().map_err(service_error)?;
        Ok(paper)
    }
}
impl PaperLibrary for SqlitePapers<'_> {
    fn create(&mut self, input: NewPaper) -> Result<Paper, ServiceError> {
        self.insert(Paper {
            id: PaperId(Uuid::now_v7()),
            title: input.title,
            external_ids: input.external_ids,
            bibliography: input.bibliography,
            created_at: None,
            updated_at: None,
            deleted_at: None,
        })
    }
    fn update(&mut self, paper: &Paper) -> Result<Paper, ServiceError> {
        let mut paper = paper.clone();
        validate_metadata(&mut paper)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let current = read(&tx, paper.id, false)?.ok_or(ServiceError::NotFound)?;
        paper.created_at = current.created_at;
        paper.updated_at = Some(timestamp(&tx)?);
        paper.deleted_at = None;
        write_metadata(&tx, &paper, false)?;
        // Metadata-only edits preserve person identity and never touch attachments.
        if current.bibliography.authors != paper.bibliography.authors {
            replace_authors(&tx, &paper)?;
        }
        tx.execute(
            "DELETE FROM paper_external_ids WHERE paper_id=?1",
            [paper.id.0.to_string()],
        )
        .map_err(service_error)?;
        write_external_ids(&tx, &paper)?;
        tx.commit().map_err(service_error)?;
        Ok(paper)
    }
    fn list(&self, query: &PaperList) -> Result<Vec<Paper>, ServiceError> {
        query.validate()?;
        // One read transaction prevents mixed author/metadata generations across writers.
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        let visibility = match query.visibility {
            PaperVisibility::Active => 0,
            PaperVisibility::Deleted => 1,
            PaperVisibility::All => 2,
        };
        let ids = tx.prepare("SELECT id FROM papers WHERE (?1 = 2 OR (?1 = 0 AND deleted_at IS NULL) OR (?1 = 1 AND deleted_at IS NOT NULL))
            AND (?2 IS NULL OR id > ?2) ORDER BY id ASC LIMIT ?3").map_err(service_error)?
            .query_map(params![visibility, query.after.map(|id| id.0.to_string()), query.limit], |r| r.get::<_, String>(0)).map_err(service_error)?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(service_error)?;
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            result.push(
                read(
                    &tx,
                    PaperId(Uuid::parse_str(&id).map_err(|_| ServiceError::Unavailable)?),
                    true,
                )?
                .ok_or(ServiceError::Unavailable)?,
            );
        }
        tx.commit().map_err(service_error)?;
        Ok(result)
    }
    fn get_including_deleted(&self, id: PaperId) -> Result<Option<Paper>, ServiceError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        let paper = read(&tx, id, true)?;
        tx.commit().map_err(service_error)?;
        Ok(paper)
    }
    fn set_deleted(&mut self, id: PaperId, deleted: bool) -> Result<Paper, ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let mut paper = read(&tx, id, true)?.ok_or(ServiceError::NotFound)?;
        if paper.deleted_at.is_some() != deleted {
            let now = timestamp(&tx)?;
            paper.deleted_at = deleted.then(|| now.clone());
            paper.updated_at = Some(now);
            tx.execute(
                "UPDATE papers SET deleted_at=?1, updated_at=?2 WHERE id=?3",
                params![paper.deleted_at, paper.updated_at, id.0.to_string()],
            )
            .map_err(service_error)?;
        }
        tx.commit().map_err(service_error)?;
        Ok(paper)
    }
}

fn timestamp(db: &Connection) -> Result<String, ServiceError> {
    db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
        r.get(0)
    })
    .map_err(service_error)
}
fn write_metadata(db: &Connection, paper: &Paper, insert: bool) -> Result<(), ServiceError> {
    let b = &paper.bibliography;
    let sql = if insert {
        "INSERT INTO papers (id, type, title, abstract, year, publication_date, journal, volume, issue, pages,
         doi_normalized, url, language, citation_key, extra_json, created_at, updated_at)
         VALUES (:id,:type,:title,:abstract,:year,:date,:journal,:volume,:issue,:pages,:doi,:url,:language,:key,:extra,:created,:updated)"
    } else {
        "UPDATE papers SET type=:type,title=:title,abstract=:abstract,year=:year,publication_date=:date,
         journal=:journal,volume=:volume,issue=:issue,pages=:pages,doi_normalized=:doi,url=:url,language=:language,
         citation_key=:key,extra_json=:extra,created_at=:created,updated_at=:updated WHERE id=:id AND deleted_at IS NULL"
    };
    db.execute(sql, named_params! {
        ":id":paper.id.0.to_string(), ":type":b.paper_type, ":title":paper.title, ":abstract":b.abstract_text,
        ":year":b.year, ":date":b.publication_date, ":journal":b.journal, ":volume":b.volume, ":issue":b.issue,
        ":pages":b.pages, ":doi":b.doi, ":url":b.url, ":language":b.language, ":key":b.citation_key,
        ":extra":b.extra_json.as_ref().ok_or(ServiceError::Unavailable)?.to_string(),
        ":created":paper.created_at, ":updated":paper.updated_at,
    }).map_err(service_error)?;
    Ok(())
}
fn write_relations(db: &Connection, paper: &Paper) -> Result<(), ServiceError> {
    replace_authors(db, paper)?;
    write_external_ids(db, paper)
}
fn replace_authors(db: &Connection, paper: &Paper) -> Result<(), ServiceError> {
    let id = paper.id.0.to_string();
    let old_people = db
        .prepare("SELECT person_id FROM paper_people WHERE paper_id=?1")
        .map_err(service_error)?
        .query_map([&id], |r| r.get::<_, String>(0))
        .map_err(service_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(service_error)?;
    db.execute("DELETE FROM paper_people WHERE paper_id=?1", [&id])
        .map_err(service_error)?;
    // Remove only newly orphaned contributors from this record, not unrelated people.
    for person in old_people {
        db.execute("DELETE FROM people WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM paper_people WHERE person_id=?1)", [&person]).map_err(service_error)?;
    }
    for (order, author) in paper.bibliography.authors.iter().enumerate() {
        let person = Uuid::now_v7().to_string();
        db.execute("INSERT INTO people(id,given_name,family_name,literal_name,orcid) VALUES (?1,?2,?3,?4,?5)",
            params![person, author.given_name, author.family_name, author.literal_name, author.orcid]).map_err(service_error)?;
        db.execute(
            "INSERT INTO paper_people(paper_id,person_id,role,sort_order) VALUES (?1,?2,?3,?4)",
            params![id, person, author.role, order as i64],
        )
        .map_err(service_error)?;
    }
    Ok(())
}
fn write_external_ids(db: &Connection, paper: &Paper) -> Result<(), ServiceError> {
    for (order, external) in paper.external_ids.iter().enumerate() {
        db.execute("INSERT INTO paper_external_ids(paper_id,namespace,value,sort_order) VALUES (?1,?2,?3,?4)",
            params![paper.id.0.to_string(), external.namespace, external.value, order as i64]).map_err(service_error)?;
    }
    Ok(())
}

fn read(
    db: &Connection,
    id: PaperId,
    include_deleted: bool,
) -> Result<Option<Paper>, ServiceError> {
    let found = db.query_row("SELECT title,type,abstract,year,publication_date,journal,volume,issue,pages,doi_normalized,
        url,language,citation_key,extra_json,created_at,updated_at,deleted_at FROM papers
        WHERE id=?1 AND (?2 OR deleted_at IS NULL)", params![id.0.to_string(), include_deleted], |r| {
        let extra: String = r.get(13)?;
        let extra_json = serde_json::from_str(&extra).map_err(|e| rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(e)))?;
        Ok(Paper {
            id, title:r.get(0)?, external_ids:vec![],
            bibliography:Bibliography { paper_type:r.get(1)?, abstract_text:r.get(2)?, year:r.get(3)?, publication_date:r.get(4)?,
                journal:r.get(5)?, volume:r.get(6)?, issue:r.get(7)?, pages:r.get(8)?, doi:r.get(9)?, url:r.get(10)?,
                language:r.get(11)?, citation_key:r.get(12)?, authors:vec![], extra_json:Some(extra_json) },
            created_at:r.get(14)?, updated_at:r.get(15)?, deleted_at:r.get(16)?,
        })
    }).optional().map_err(service_error)?;
    let Some(mut paper) = found else {
        return Ok(None);
    };
    paper.bibliography.authors = db.prepare("SELECT given_name,family_name,literal_name,orcid,role
        FROM paper_people JOIN people ON people.id=paper_people.person_id WHERE paper_id=?1 ORDER BY sort_order").map_err(service_error)?
        .query_map([id.0.to_string()], |r| Ok(Contributor { given_name:r.get(0)?, family_name:r.get(1)?, literal_name:r.get(2)?, orcid:r.get(3)?, role:r.get(4)? })).map_err(service_error)?
        .collect::<rusqlite::Result<_>>().map_err(service_error)?;
    paper.external_ids = db
        .prepare(
            "SELECT namespace,value FROM paper_external_ids WHERE paper_id=?1 ORDER BY sort_order",
        )
        .map_err(service_error)?
        .query_map([id.0.to_string()], |r| {
            Ok(ExternalId {
                namespace: r.get(0)?,
                value: r.get(1)?,
            })
        })
        .map_err(service_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(service_error)?;
    Ok(Some(paper))
}
