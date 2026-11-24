//! `SearchIndex` over SQLite. Every value is bound; FTS input is one quoted phrase.
use super::{PDF_ATTACHMENT, reader::Deadline};
use crate::error::{SqlFault, service_error, sql_fault};
use cm_core::{
    ServiceError,
    papers::PaperVisibility,
    search::{
        service::{
            Candidate, ExtractionGaps, IndexedDocument, ResolvedFilter, ScanRow, ScanWindow,
            SearchIndex, Tokenizer,
        },
        types::{DocumentSource, SearchField, Weights},
    },
};
use cm_domain::{AttachmentId, CollectionId, PaperId, TagId, TagKey};
use rusqlite::{Connection, OptionalExtension, Row, params_from_iter, types::Value};
use std::{cell::Cell, time::Instant};
use uuid::Uuid;

pub struct SqliteSearch<'a> {
    pub(crate) connection: &'a Connection,
    /// Interrupt deadline of a `SearchReader` connection (`None`: not interruptible).
    pub(crate) deadline: Option<&'a Deadline>,
    /// Classification of the last SQLite error (damage vs. contention vs. interrupt).
    pub(crate) fault: Cell<Option<SqlFault>>,
}
impl<'a> SqliteSearch<'a> {
    pub(crate) fn new(connection: &'a Connection, deadline: Option<&'a Deadline>) -> Self {
        Self {
            connection,
            deadline,
            fault: Cell::new(None),
        }
    }
    /// What the last failing SQLite call reported. Callers use it to tell real index damage
    /// (`Corrupt`, worth a verify/rebuild) from transient lock contention (`Busy`) and the
    /// request deadline (`Interrupted`); the port itself only returns `ServiceError`.
    pub fn fault(&self) -> Option<SqlFault> {
        self.fault.get()
    }
    pub(crate) fn err(&self, e: rusqlite::Error) -> ServiceError {
        self.fault.set(Some(sql_fault(&e)));
        service_error(e)
    }
}

/// Rows examined per scan statement; the deadline is checked between chunks.
const SCAN_CHUNK: u32 = 256;
const COLUMNS: [&str; 8] = [
    "title",
    "authors",
    "abstract",
    "journal",
    "tags",
    "identifiers",
    "notes",
    "body",
];

#[derive(Default)]
pub(crate) struct Sql {
    pub sql: String,
    pub values: Vec<Value>,
}
impl Sql {
    pub fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.values.len())
    }
    pub fn and(&mut self, clause: &str) {
        self.sql.push_str(" AND (");
        self.sql.push_str(clause);
        self.sql.push(')');
    }
}

/// Paper predicates against alias `p` (papers).
pub(crate) fn filter(b: &mut Sql, f: &ResolvedFilter) {
    match f.visibility {
        PaperVisibility::Active => b.and("p.deleted_at IS NULL"),
        PaperVisibility::Deleted => b.and("p.deleted_at IS NOT NULL"),
        PaperVisibility::All => {}
    }
    for doi in &f.dois {
        let v = b.bind(doi.clone());
        b.and(&format!("p.doi_normalized={v}"));
    }
    for tag in &f.tags {
        let v = b.bind(tag.0.to_string());
        b.and(&format!(
            "EXISTS(SELECT 1 FROM paper_tags pt WHERE pt.paper_id=p.id AND pt.tag_id={v})"
        ));
    }
    for collection in &f.collections {
        let v = b.bind(collection.0.to_string());
        b.and(&format!("EXISTS(SELECT 1 FROM paper_collections pc WHERE pc.paper_id=p.id AND pc.collection_id={v})"));
    }
    for (from, to) in &f.years {
        let from = b.bind(i64::from(*from));
        let to = b.bind(i64::from(*to));
        b.and(&format!("p.year BETWEEN {from} AND {to}"));
    }
    if f.has_pdf {
        b.and(&format!(
            "EXISTS(SELECT 1 FROM attachments a WHERE a.paper_id=p.id AND {PDF_ATTACHMENT})"
        ));
    }
    if !f.papers.is_empty() {
        let list = f
            .papers
            .iter()
            .map(|p| b.bind(p.0.to_string()))
            .collect::<Vec<_>>()
            .join(",");
        b.and(&format!("p.id IN ({list})"));
    }
}

fn sources(b: &mut Sql, sources: &[DocumentSource]) {
    let list = sources
        .iter()
        .map(|s| format!("'{}'", source_name(*s)))
        .collect::<Vec<_>>()
        .join(",");
    b.and(&format!("d.source IN ({list})"));
}

pub(crate) fn source_name(source: DocumentSource) -> &'static str {
    match source {
        DocumentSource::Paper => "paper",
        DocumentSource::Annotation => "annotation",
        DocumentSource::Page => "page",
    }
}
fn parse_source(value: &str) -> rusqlite::Result<DocumentSource> {
    match value {
        "paper" => Ok(DocumentSource::Paper),
        "annotation" => Ok(DocumentSource::Annotation),
        "page" => Ok(DocumentSource::Page),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn uuid(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let value: String = row.get(index)?;
    Uuid::parse_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}
/// One FTS5 phrase: the whole text in double quotes, embedded quotes doubled.
pub(crate) fn phrase(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

impl SqliteSearch<'_> {
    fn candidates(&self, b: &Sql) -> Result<Vec<Candidate>, ServiceError> {
        self.connection
            .prepare(&b.sql)
            .map_err(|e| self.err(e))?
            .query_map(params_from_iter(b.values.iter()), |row| {
                Ok(Candidate {
                    id: row.get(0)?,
                    paper_id: PaperId(uuid(row, 1)?),
                    source: parse_source(&row.get::<_, String>(2)?)?,
                    score: row.get(3)?,
                })
            })
            .map_err(|e| self.err(e))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| self.err(e))
    }
}

impl SearchIndex for SqliteSearch<'_> {
    fn set_deadline(&self, deadline: Option<Instant>) {
        if let Some(cell) = self.deadline {
            cell.set(deadline);
        }
    }
    fn interrupted(&self) -> bool {
        self.fault.get() == Some(SqlFault::Interrupted)
    }
    fn resolve_tag(&self, key: &TagKey) -> Result<Option<TagId>, ServiceError> {
        self.connection
            .query_row(
                "SELECT id FROM tags WHERE normalized_name=?1",
                [key.as_str()],
                |r| uuid(r, 0),
            )
            .optional()
            .map(|id| id.map(TagId))
            .map_err(|e| self.err(e))
    }
    fn resolve_collection(&self, name: &str) -> Result<Vec<CollectionId>, ServiceError> {
        self.connection
            .prepare("SELECT id FROM collections WHERE name=?1 ORDER BY id LIMIT 16")
            .map_err(|e| self.err(e))?
            .query_map([name], |r| uuid(r, 0).map(CollectionId))
            .map_err(|e| self.err(e))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| self.err(e))
    }
    fn fts(
        &self,
        tokenizer: Tokenizer,
        text: &str,
        f: &ResolvedFilter,
        source_set: &[DocumentSource],
        weights: &Weights,
        max: u32,
    ) -> Result<Vec<Candidate>, ServiceError> {
        if source_set.is_empty() {
            return Ok(Vec::new());
        }
        let (table, fields): (&str, Vec<SearchField>) = match tokenizer {
            Tokenizer::Words => ("search_fts_words", SearchField::ALL.to_vec()),
            Tokenizer::Trigram => (
                "search_fts_trigram",
                SearchField::ALL
                    .into_iter()
                    .filter(|f| *f != SearchField::Identifiers)
                    .collect(),
            ),
        };
        let mut b = Sql::default();
        let w = fields
            .iter()
            .map(|field| b.bind(weights.get(*field)))
            .collect::<Vec<_>>()
            .join(",");
        let m = b.bind(phrase(text));
        b.sql = format!(
            "SELECT d.id, d.paper_id, d.source, -bm25({table},{w}) AS score
             FROM {table} JOIN search_documents d ON d.id={table}.rowid
             JOIN papers p ON p.id=d.paper_id
             WHERE {table} MATCH {m}"
        );
        sources(&mut b, source_set);
        filter(&mut b, f);
        let limit = b.bind(i64::from(max));
        b.sql
            .push_str(&format!(" ORDER BY score DESC, d.id LIMIT {limit}"));
        self.candidates(&b)
    }
    fn filtered(&self, f: &ResolvedFilter, max: u32) -> Result<Vec<Candidate>, ServiceError> {
        let mut b = Sql {
            sql: "SELECT d.id, d.paper_id, d.source, 0.0 FROM search_documents d
                  JOIN papers p ON p.id=d.paper_id WHERE d.source='paper'"
                .into(),
            values: Vec::new(),
        };
        filter(&mut b, f);
        let limit = b.bind(i64::from(max));
        b.sql.push_str(&format!(" ORDER BY d.id LIMIT {limit}"));
        self.candidates(&b)
    }
    fn scan(
        &self,
        needles: &[String],
        f: &ResolvedFilter,
        source_set: &[DocumentSource],
        after: i64,
        max_rows: u32,
        deadline: Instant,
    ) -> Result<ScanWindow, ServiceError> {
        let mut window = ScanWindow::default();
        if needles.is_empty() || source_set.is_empty() {
            window.exhausted = true;
            return Ok(window);
        }
        let mut last = after;
        loop {
            let chunk = SCAN_CHUNK.min(max_rows - window.examined);
            let mut b = Sql::default();
            let masks = needles
                .iter()
                .map(|n| {
                    let v = b.bind(n.to_ascii_lowercase());
                    COLUMNS
                        .iter()
                        .enumerate()
                        .map(|(i, c)| format!("((instr(lower(coalesce(d.{c},'')),{v})>0)<<{i})"))
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .collect::<Vec<_>>();
            let from = b.bind(last);
            b.sql = format!(
                "SELECT d.id, d.paper_id, d.source, {} FROM search_documents d
                 JOIN papers p ON p.id=d.paper_id WHERE d.id>{from}",
                masks.join(", ")
            );
            sources(&mut b, source_set);
            filter(&mut b, f);
            let limit = b.bind(i64::from(chunk));
            b.sql.push_str(&format!(" ORDER BY d.id LIMIT {limit}"));
            let rows = self.connection.prepare(&b.sql).and_then(|mut statement| {
                statement
                    .query_map(params_from_iter(b.values.iter()), |row| {
                        let masks = (0..needles.len())
                            .map(|i| row.get::<_, i64>(3 + i).map(|m| m as u8))
                            .collect::<rusqlite::Result<Vec<u8>>>()?;
                        Ok(ScanRow {
                            id: row.get(0)?,
                            paper_id: PaperId(uuid(row, 1)?),
                            source: parse_source(&row.get::<_, String>(2)?)?,
                            masks,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
            });
            let rows = match rows {
                Ok(rows) => rows,
                // The request deadline interrupted this chunk: the window ends after the
                // last fully examined chunk and is reported as timed out (continuation).
                Err(e) if sql_fault(&e) == SqlFault::Interrupted => {
                    window.timed_out = true;
                    return Ok(window);
                }
                Err(e) => return Err(self.err(e)),
            };
            let count = rows.len() as u32;
            window.examined += count;
            if let Some(row) = rows.last() {
                last = row.id;
                window.last_id = Some(row.id);
            }
            window
                .rows
                .extend(rows.into_iter().filter(|r| r.masks.iter().any(|m| *m != 0)));
            if count < chunk {
                window.exhausted = true;
                return Ok(window);
            }
            if window.examined >= max_rows {
                break;
            }
            if Instant::now() >= deadline {
                window.timed_out = true;
                break;
            }
        }
        // Full window: exhausted only if no further document is in scope.
        let mut b = Sql::default();
        let from = b.bind(last);
        b.sql = format!(
            "SELECT EXISTS(SELECT 1 FROM search_documents d JOIN papers p ON p.id=d.paper_id WHERE d.id>{from}"
        );
        sources(&mut b, source_set);
        filter(&mut b, f);
        b.sql.push(')');
        let more: bool =
            match self
                .connection
                .query_row(&b.sql, params_from_iter(b.values.iter()), |r| r.get(0))
            {
                Ok(more) => more,
                Err(e) if sql_fault(&e) == SqlFault::Interrupted => {
                    window.timed_out = true;
                    return Ok(window);
                }
                Err(e) => return Err(self.err(e)),
            };
        window.exhausted = !more;
        if window.exhausted {
            window.timed_out = false;
        }
        Ok(window)
    }
    fn documents(&self, ids: &[i64]) -> Result<Vec<IndexedDocument>, ServiceError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut b = Sql::default();
        let list = ids
            .iter()
            .map(|id| b.bind(*id))
            .collect::<Vec<_>>()
            .join(",");
        b.sql = format!(
            "SELECT d.id, d.source, d.source_key, d.paper_id, d.attachment_id, d.page_index, d.page_label,
                    p.title, p.doi_normalized, p.deleted_at IS NOT NULL, {}
             FROM search_documents d JOIN papers p ON p.id=d.paper_id WHERE d.id IN ({list})",
            COLUMNS.map(|c| format!("d.{c}")).join(",")
        );
        self.connection
            .prepare(&b.sql)
            .map_err(|e| self.err(e))?
            .query_map(params_from_iter(b.values.iter()), |row| {
                let attachment: Option<String> = row.get(4)?;
                let mut fields: [Option<String>; 8] = Default::default();
                for (i, field) in fields.iter_mut().enumerate() {
                    *field = row.get(10 + i)?;
                }
                Ok(IndexedDocument {
                    id: row.get(0)?,
                    source: parse_source(&row.get::<_, String>(1)?)?,
                    source_key: row.get(2)?,
                    paper_id: PaperId(uuid(row, 3)?),
                    attachment_id: attachment
                        .map(|a| Uuid::parse_str(&a).map(AttachmentId))
                        .transpose()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    page_index: row
                        .get::<_, Option<i64>>(5)?
                        .map(|i| u32::try_from(i).map_err(|_| rusqlite::Error::InvalidQuery))
                        .transpose()?,
                    page_label: row.get(6)?,
                    paper_title: row.get(7)?,
                    doi: row.get(8)?,
                    deleted: row.get(9)?,
                    fields,
                })
            })
            .map_err(|e| self.err(e))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| self.err(e))
    }
    fn extraction_gaps(&self, f: &ResolvedFilter) -> Result<ExtractionGaps, ServiceError> {
        let mut b = Sql {
            sql: format!(
                "SELECT coalesce(sum(e.attachment_id IS NULL OR e.status='extracting'),0),
                        coalesce(sum(e.status IN ('no_text','encrypted','malformed','too_large','failed')),0)
                 FROM attachments a JOIN papers p ON p.id=a.paper_id
                 LEFT JOIN fulltext_extractions e ON e.attachment_id=a.id
                 WHERE {PDF_ATTACHMENT}"
            ),
            values: Vec::new(),
        };
        filter(&mut b, f);
        self.connection
            .query_row(&b.sql, params_from_iter(b.values.iter()), |r| {
                Ok(ExtractionGaps {
                    pending: r.get::<_, i64>(0)? as u32,
                    without_text: r.get::<_, i64>(1)? as u32,
                })
            })
            .map_err(|e| self.err(e))
    }
}
