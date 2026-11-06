//! Real SQLite import adapter. No files/parser/network integration.
use super::{
    collections::{apply_destination, validate_destination},
    papers::{insert_in_transaction, read, update_in_transaction},
};
use crate::{LibraryDb, error::service_error};
use cm_core::{
    ServiceError,
    import::{normalize::*, *},
};
use cm_domain::{Paper, PaperId};
use rusqlite::{Connection, TransactionBehavior};
use uuid::Uuid;

pub struct SqliteImports<'a> {
    connection: &'a mut Connection,
    instance: u64,
}
impl LibraryDb {
    pub fn imports(&mut self) -> SqliteImports<'_> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        SqliteImports {
            instance: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            connection: &mut self.connection,
        }
    }
}
fn snapshot(
    db: &Connection,
    destination: &ImportDestination,
    instance: u64,
) -> Result<ImportSnapshot, ServiceError> {
    validate_destination(db, destination)?;
    // data_version is comparable only on this same connection; total_changes also
    // invalidates local writes (including rolled-back attempts, conservatively).
    let version: i64 = db
        .pragma_query_value(None, "data_version", |r| r.get(0))
        .map_err(service_error)?;
    let revision = format!("{instance}:{version}:{}", db.total_changes());
    let count: i64 = db
        .query_row("SELECT count(*) FROM papers", [], |r| r.get(0))
        .map_err(service_error)?;
    if count > MAX_LIBRARY_PAPERS as i64 {
        return Err(invalid("library inventory exceeds import paper budget"));
    }
    // Reject before loading unbounded legacy metadata into memory. Contributor and
    // external-ID bytes are included, even for trash records.
    let relations: i64 = db.query_row("SELECT (SELECT count(*) FROM paper_people) + (SELECT count(*) FROM paper_external_ids)", [], |r| r.get(0)).map_err(service_error)?;
    if relations > 100_000 {
        return Err(invalid("library relation inventory exceeds import budget"));
    }
    let bytes: i64 = db.query_row(
        "SELECT
         (SELECT coalesce(sum(
          length(CAST(title AS BLOB)) + length(CAST(coalesce(abstract,'') AS BLOB)) +
          length(CAST(extra_json AS BLOB)) + length(CAST(coalesce(journal,'') AS BLOB)) +
          length(CAST(coalesce(url,'') AS BLOB)) + length(CAST(coalesce(citation_key,'') AS BLOB)) +
          length(CAST(coalesce(publication_date,'') AS BLOB)) + length(CAST(coalesce(volume,'') AS BLOB)) +
          length(CAST(coalesce(issue,'') AS BLOB)) + length(CAST(coalesce(pages,'') AS BLOB)) +
          length(CAST(coalesce(language,'') AS BLOB)) + length(CAST(type AS BLOB)) +
          length(CAST(coalesce(doi_normalized,'') AS BLOB))),0) FROM papers) +
         (SELECT coalesce(sum(length(CAST(coalesce(given_name,'') AS BLOB)) +
          length(CAST(coalesce(family_name,'') AS BLOB)) + length(CAST(coalesce(literal_name,'') AS BLOB)) +
          length(CAST(coalesce(orcid,'') AS BLOB)) + length(CAST(role AS BLOB))),0)
          FROM paper_people JOIN people ON people.id=paper_people.person_id) +
         (SELECT coalesce(sum(length(CAST(namespace AS BLOB)) + length(CAST(value AS BLOB))),0)
          FROM paper_external_ids)", [], |r| r.get(0)).map_err(service_error)?;
    if bytes > MAX_INVENTORY_BYTES as i64 {
        return Err(invalid("library metadata exceeds import byte budget"));
    }
    let ids = db
        .prepare("SELECT id FROM papers ORDER BY id")
        .map_err(service_error)?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(service_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(service_error)?;
    let mut papers = Vec::with_capacity(ids.len());
    for id in ids {
        let id = PaperId(Uuid::parse_str(&id).map_err(|_| ServiceError::Unavailable)?);
        papers.push(read(db, id, true)?.ok_or(ServiceError::Unavailable)?);
    }
    let snapshot = ImportSnapshot { revision, papers };
    check_snapshot(&snapshot)?;
    Ok(snapshot)
}
impl ImportBackend for SqliteImports<'_> {
    fn snapshot(
        &mut self,
        destination: &ImportDestination,
    ) -> Result<ImportSnapshot, ServiceError> {
        let tx = self.connection.transaction().map_err(service_error)?;
        let snapshot = snapshot(&tx, destination, self.instance)?;
        tx.commit().map_err(service_error)?;
        Ok(snapshot)
    }
    fn apply(&mut self, approved: &PreparedImport) -> Result<ImportOutcome, ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let current = snapshot(&tx, approved.destination(), self.instance)?;
        let writes = approved.writes(&current)?;
        let mut outcome = ImportOutcome {
            skipped: approved.preview().candidates.len() - writes.len(),
            ..Default::default()
        };
        for write in writes {
            let (index, paper) = match write {
                ImportWrite::Create { index, draft } => {
                    let paper = insert_in_transaction(
                        &tx,
                        Paper {
                            id: PaperId(Uuid::now_v7()),
                            title: draft.title,
                            external_ids: draft.external_ids,
                            bibliography: draft.bibliography,
                            created_at: None,
                            updated_at: None,
                            deleted_at: None,
                        },
                    )?;
                    outcome.created.push(paper.id);
                    (index, paper)
                }
                ImportWrite::Merge { index, paper } => {
                    let paper = update_in_transaction(&tx, &paper)?;
                    outcome.merged.push(paper.id);
                    (index, paper)
                }
            };
            apply_destination(&tx, paper.id, approved.destination())?;
            let row = &approved.preview().candidates[index];
            outcome.rows.push(ImportRowOutcome {
                candidate_index: index,
                paper: Some(paper.id),
                source: row.draft.source.clone(),
                diagnostics: row.diagnostics.clone(),
            });
        }
        for (index, row) in approved.preview().candidates.iter().enumerate() {
            if !outcome.rows.iter().any(|r| r.candidate_index == index) {
                outcome.rows.push(ImportRowOutcome {
                    candidate_index: index,
                    paper: None,
                    source: row.draft.source.clone(),
                    diagnostics: row.diagnostics.clone(),
                });
            }
        }
        outcome.rows.sort_by_key(|r| r.candidate_index);
        tx.commit().map_err(service_error)?;
        Ok(outcome)
    }
}
