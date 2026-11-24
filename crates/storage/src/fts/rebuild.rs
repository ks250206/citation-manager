//! Recovery for a damaged/out-of-sync search index: verify, then rebuild the projection
//! and both FTS indexes from the authoritative source tables in one transaction.
//! Source tables (papers, contributors, tags, annotations, fulltext_pages) are only read.
use super::SqliteSearch;
use crate::{StorageError, error::service_error};
use cm_core::ServiceError;
use rusqlite::TransactionBehavior;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexHealth {
    /// FTS5 `integrity-check` (index vs. external content) passed for both indexes.
    pub fts_ok: bool,
    /// The projection equals what the source tables produce now.
    pub projection_ok: bool,
    /// Fixed diagnostic (never document text).
    pub detail: Option<String>,
}
impl IndexHealth {
    pub fn ok(&self) -> bool {
        self.fts_ok && self.projection_ok
    }
}

/// The same projection the migration backfills and the triggers maintain.
const REFILL: &str = "
INSERT INTO search_documents(source, source_key, paper_id, title, authors, abstract, journal, tags, identifiers)
SELECT 'paper', paper_id, paper_id, title, authors, abstract, journal, tags, identifiers FROM search_paper_projection;
INSERT INTO search_documents(source, source_key, paper_id, attachment_id, page_index, page_label, notes)
SELECT 'annotation', annotation_id, paper_id, attachment_id, page_index, page_label, notes FROM search_annotation_projection;
INSERT INTO search_documents(source, source_key, paper_id, attachment_id, page_index, page_label, body)
SELECT 'page', CAST(page_id AS TEXT), paper_id, attachment_id, page_index, page_label, body FROM search_page_projection;
";

/// Expected projection rows (source, key, paper, attachment, page, label, 8 fields).
const EXPECTED: &str = "
SELECT 'paper', paper_id, paper_id, NULL, NULL, NULL, title, authors, abstract, journal, tags, identifiers, NULL, NULL FROM search_paper_projection
UNION ALL
SELECT 'annotation', annotation_id, paper_id, attachment_id, page_index, page_label, NULL, NULL, NULL, NULL, NULL, NULL, notes, NULL FROM search_annotation_projection
UNION ALL
SELECT 'page', CAST(page_id AS TEXT), paper_id, attachment_id, page_index, page_label, NULL, NULL, NULL, NULL, NULL, NULL, NULL, body FROM search_page_projection";
const ACTUAL: &str = "SELECT source, source_key, paper_id, attachment_id, page_index, page_label, title, authors, abstract, journal, tags, identifiers, notes, body FROM search_documents";

impl SqliteSearch<'_> {
    /// Checks both FTS indexes against their content and the projection against sources.
    pub fn verify(&self) -> Result<IndexHealth, ServiceError> {
        let mut fts_ok = true;
        let mut detail = None;
        for table in ["search_fts_words", "search_fts_trigram"] {
            // rank=1: also compare the index with the external content table.
            if let Err(e) = self.connection.execute(
                &format!("INSERT INTO {table}({table}, rank) VALUES ('integrity-check', 1)"),
                [],
            ) {
                fts_ok = false;
                detail = Some(format!("{table} integrity-check failed: {}", code(&e)));
            }
        }
        let differences = |a: &str, b: &str| -> Result<i64, ServiceError> {
            self.connection
                .query_row(
                    &format!(
                        "SELECT count(*) FROM (SELECT * FROM ({a}) EXCEPT SELECT * FROM ({b}))"
                    ),
                    [],
                    |r| r.get(0),
                )
                .map_err(service_error)
        };
        let projection_ok = differences(EXPECTED, ACTUAL)? == 0
            && differences(ACTUAL, EXPECTED)? == 0
            && self
                .connection
                .query_row(
                    &format!("SELECT (SELECT count(*) FROM ({EXPECTED})) = (SELECT count(*) FROM search_documents)"),
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(service_error)?;
        if !projection_ok && detail.is_none() {
            detail = Some("search projection differs from source tables".into());
        }
        Ok(IndexHealth {
            fts_ok,
            projection_ok,
            detail,
        })
    }
    /// Rebuilds the projection and both FTS indexes (one IMMEDIATE transaction). The
    /// projection triggers are suspended so clearing it issues no per-row FTS deletes
    /// against a possibly damaged index. Returns the number of indexed documents.
    pub fn rebuild(&self) -> Result<u64, StorageError> {
        let tx =
            rusqlite::Transaction::new_unchecked(self.connection, TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "UPDATE search_index_state SET suspended=1 WHERE id=1; DELETE FROM search_documents;",
        )?;
        tx.execute_batch(REFILL)?;
        tx.execute_batch(
            "UPDATE search_index_state SET suspended=0, generation=generation+1,
                 rebuilt_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=1;
             INSERT INTO search_fts_words(search_fts_words) VALUES ('rebuild');
             INSERT INTO search_fts_trigram(search_fts_trigram) VALUES ('rebuild');",
        )?;
        let count: i64 = tx.query_row("SELECT count(*) FROM search_documents", [], |r| r.get(0))?;
        tx.commit()?;
        Ok(count as u64)
    }
    /// Allocated bytes per search object (projection, its indexes and FTS shadow tables),
    /// from the bundled SQLite's DBSTAT virtual table. Diagnostics/benchmarks (#30) only.
    pub fn index_bytes(&self) -> Result<Vec<(String, u64)>, ServiceError> {
        self.connection
            .prepare(
                "SELECT name, sum(pgsize) FROM dbstat
                 WHERE name GLOB 'search_*' OR name IN ('fulltext_pages','sqlite_autoindex_search_documents_1')
                 GROUP BY name ORDER BY name",
            )
            .map_err(service_error)?
            .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))
            .map_err(service_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(service_error)
    }
    pub fn generation(&self) -> Result<u64, ServiceError> {
        self.connection
            .query_row(
                "SELECT generation FROM search_index_state WHERE id=1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|g| g as u64)
            .map_err(service_error)
    }
}

fn code(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => format!("sqlite code {}", f.extended_code),
        _ => "error".into(),
    }
}
