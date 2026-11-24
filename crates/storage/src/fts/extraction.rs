//! Page text and per-attachment extraction state. Each `store` is one IMMEDIATE
//! transaction; the projection/FTS triggers update the index in that same transaction.
use super::PDF_ATTACHMENT;
use crate::{LibraryDirectory, attachments::files::managed_path, error::service_error};
use cm_core::{
    ServiceError,
    search::{
        extraction::{
            AttachmentExtraction, ExtractScope, ExtractTarget, ExtractionState, FulltextStore,
            StoreBatch,
        },
        types::{PageText, PageTextRead, PageTextRequest},
    },
};
use cm_domain::{AttachmentId, PaperId};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

pub struct SqliteFulltext<'a> {
    pub(crate) connection: &'a mut Connection,
    pub(crate) directory: LibraryDirectory,
}
pub struct FulltextReader<'a> {
    pub(crate) connection: &'a Connection,
}

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

fn uuid(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let value: String = row.get(index)?;
    Uuid::parse_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn count(row: &Row<'_>, index: usize) -> rusqlite::Result<u32> {
    u32::try_from(row.get::<_, i64>(index)?)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, -1))
}

impl FulltextStore for SqliteFulltext<'_> {
    fn targets(
        &mut self,
        scope: ExtractScope,
        force: bool,
        limit: u32,
    ) -> Result<Vec<ExtractTarget>, ServiceError> {
        let (predicate, key) = match scope {
            ExtractScope::Pending => ("1=1", None),
            ExtractScope::Attachment(id) => ("a.id=?2", Some(id.0.to_string())),
            ExtractScope::Paper(id) => ("a.paper_id=?2", Some(id.0.to_string())),
        };
        // Unfinished = never started, interrupted, or recorded for other bytes.
        let unfinished =
            "(e.attachment_id IS NULL OR e.status='extracting' OR e.file_hash<>a.file_hash)";
        let wanted = if force { "1=1" } else { unfinished };
        let sql = format!(
            "SELECT a.id, a.paper_id, a.file_hash, a.relative_path, e.status, e.next_page,
                    e.file_hash = a.file_hash
             FROM attachments a JOIN papers p ON p.id=a.paper_id
             LEFT JOIN fulltext_extractions e ON e.attachment_id=a.id
             WHERE {PDF_ATTACHMENT} AND {predicate} AND {wanted}
             ORDER BY p.deleted_at IS NOT NULL, a.added_at, a.id LIMIT ?1"
        );
        let mut statement = self.connection.prepare(&sql).map_err(service_error)?;
        let map = |row: &Row<'_>| {
            let hash: Vec<u8> = row.get(2)?;
            Ok((
                AttachmentId(uuid(row, 0)?),
                PaperId(uuid(row, 1)?),
                <[u8; 32]>::try_from(hash).map_err(|_| rusqlite::Error::InvalidQuery)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<bool>>(6)?,
            ))
        };
        let rows = match key {
            Some(key) => statement.query_map(params![i64::from(limit), key], map),
            None => statement.query_map(params![i64::from(limit)], map),
        }
        .map_err(service_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(service_error)?;
        drop(statement);
        let mut targets = Vec::with_capacity(rows.len());
        for (attachment_id, paper_id, sha256, key, status, next, same_hash) in rows {
            let path = managed_path(&self.directory, &key, false)
                .map_err(|_| ServiceError::InvalidInput("unsafe managed attachment path".into()))?;
            let resumable =
                !force && status.as_deref() == Some("extracting") && same_hash == Some(true);
            targets.push(ExtractTarget {
                attachment_id,
                paper_id,
                sha256,
                path,
                resume_from: if resumable {
                    next.unwrap_or(0).max(0) as u32
                } else {
                    0
                },
                reset: status.is_some() && !resumable,
            });
        }
        Ok(targets)
    }
    fn reset(&mut self, attachment: AttachmentId) -> Result<(), ServiceError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let id = attachment.0.to_string();
        tx.execute("DELETE FROM fulltext_pages WHERE attachment_id=?1", [&id])
            .map_err(service_error)?;
        tx.execute(
            "DELETE FROM fulltext_extractions WHERE attachment_id=?1",
            [&id],
        )
        .map_err(service_error)?;
        tx.commit().map_err(service_error)
    }
    fn store(&mut self, batch: StoreBatch<'_>) -> Result<ExtractionState, ServiceError> {
        let target = batch.target;
        let id = target.attachment_id.0.to_string();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(service_error)?;
        let truncated = batch.pages.iter().filter(|p| p.truncated).count() as i64;
        tx.execute(
            &format!(
                "INSERT INTO fulltext_extractions(attachment_id,file_hash,status,page_count,next_page,truncated_pages,updated_at)
                 VALUES (?1,?2,'extracting',?3,?4,?5,{NOW})
                 ON CONFLICT(attachment_id) DO UPDATE SET file_hash=excluded.file_hash, status='extracting',
                    page_count=excluded.page_count, next_page=excluded.next_page,
                    truncated_pages=truncated_pages+excluded.truncated_pages, detail=NULL,
                    updated_at=excluded.updated_at"
            ),
            params![
                id,
                target.sha256.as_slice(),
                i64::from(batch.page_count),
                i64::from(batch.next_page),
                truncated
            ],
        )
        .map_err(service_error)?;
        {
            let mut insert = tx
                .prepare(&format!(
                    "INSERT INTO fulltext_pages(attachment_id,page_index,page_label,text,extraction_status,updated_at)
                     VALUES (?1,?2,?3,?4,?5,{NOW})
                     ON CONFLICT(attachment_id,page_index) DO UPDATE SET page_label=excluded.page_label,
                        text=excluded.text, extraction_status=excluded.extraction_status,
                        updated_at=excluded.updated_at"
                ))
                .map_err(service_error)?;
            for page in batch.pages {
                let has_text = !page.text.trim().is_empty();
                insert
                    .execute(params![
                        id,
                        i64::from(page.page_index),
                        page.page_label,
                        if has_text { page.text.as_str() } else { "" },
                        if has_text { "complete" } else { "unavailable" },
                    ])
                    .map_err(service_error)?;
            }
        }
        let state = match batch.finish {
            None => ExtractionState::Extracting,
            Some(page_limit_hit) => {
                let (with_text, truncated): (i64, i64) = tx
                    .query_row(
                        "SELECT (SELECT count(*) FROM fulltext_pages WHERE attachment_id=?1 AND extraction_status='complete'),
                                truncated_pages FROM fulltext_extractions WHERE attachment_id=?1",
                        [&id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(service_error)?;
                let (state, detail) = if with_text == 0 {
                    (
                        ExtractionState::NoText,
                        Some(
                            "no extractable text (image-only or empty pages); OCR is not supported",
                        ),
                    )
                } else if page_limit_hit {
                    (ExtractionState::Partial, Some("page limit reached"))
                } else if truncated > 0 {
                    (
                        ExtractionState::Partial,
                        Some("some pages exceeded the per-page text limit"),
                    )
                } else {
                    (ExtractionState::Complete, None)
                };
                tx.execute(
                    "UPDATE fulltext_extractions SET status=?2, detail=?3,
                        pages_with_text=(SELECT count(*) FROM fulltext_pages WHERE attachment_id=?1 AND extraction_status='complete')
                     WHERE attachment_id=?1",
                    params![id, state.as_str(), detail],
                )
                .map_err(service_error)?;
                state
            }
        };
        tx.commit().map_err(service_error)?;
        Ok(state)
    }
    fn fail(
        &mut self,
        target: &ExtractTarget,
        state: ExtractionState,
        detail: &str,
    ) -> Result<(), ServiceError> {
        if !state.lacks_text() {
            return Err(ServiceError::InvalidInput("not a failure state".into()));
        }
        let id = target.attachment_id.0.to_string();
        self.connection
            .execute(
                &format!(
                    "INSERT INTO fulltext_extractions(attachment_id,file_hash,status,detail,updated_at)
                     VALUES (?1,?2,?3,?4,{NOW})
                     ON CONFLICT(attachment_id) DO UPDATE SET file_hash=excluded.file_hash,
                        status=excluded.status, detail=excluded.detail, updated_at=excluded.updated_at,
                        pages_with_text=(SELECT count(*) FROM fulltext_pages WHERE attachment_id=?1 AND extraction_status='complete')"
                ),
                params![id, target.sha256.as_slice(), state.as_str(), detail],
            )
            .map_err(service_error)?;
        Ok(())
    }
}

const STATUS: &str =
    "SELECT a.id, a.paper_id, coalesce(e.status,'pending'), coalesce(e.page_count,0),
        coalesce(e.next_page,0), coalesce(e.pages_with_text,0), coalesce(e.truncated_pages,0),
        e.detail, e.updated_at
    FROM attachments a JOIN papers p ON p.id=a.paper_id
    LEFT JOIN fulltext_extractions e ON e.attachment_id=a.id";

fn status_row(row: &Row<'_>) -> rusqlite::Result<AttachmentExtraction> {
    Ok(AttachmentExtraction {
        attachment_id: AttachmentId(uuid(row, 0)?),
        paper_id: PaperId(uuid(row, 1)?),
        state: ExtractionState::parse(&row.get::<_, String>(2)?)
            .ok_or(rusqlite::Error::InvalidQuery)?,
        page_count: count(row, 3)?,
        pages_done: count(row, 4)?,
        pages_with_text: count(row, 5)?,
        truncated_pages: count(row, 6)?,
        detail: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

impl FulltextReader<'_> {
    /// Extraction state of each PDF attachment of a visible (not trashed) paper.
    pub fn for_paper(&self, paper: PaperId) -> Result<Vec<AttachmentExtraction>, ServiceError> {
        self.connection
            .prepare(&format!(
                "{STATUS} WHERE p.deleted_at IS NULL AND a.paper_id=?1 AND {PDF_ATTACHMENT} ORDER BY a.added_at, a.id"
            ))
            .map_err(service_error)?
            .query_map([paper.0.to_string()], status_row)
            .map_err(service_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(service_error)
    }
    pub fn for_attachment(
        &self,
        attachment: AttachmentId,
    ) -> Result<Option<AttachmentExtraction>, ServiceError> {
        self.connection
            .query_row(
                &format!("{STATUS} WHERE p.deleted_at IS NULL AND a.id=?1 AND {PDF_ATTACHMENT}"),
                [attachment.0.to_string()],
                status_row,
            )
            .optional()
            .map_err(service_error)
    }
    /// PDF attachments whose extraction has not finished (never started or interrupted),
    /// the first such attachment ID and the pages committed for them so far. Together they
    /// form the automatic job idempotency key: it changes whenever work was done.
    pub fn unfinished(&self) -> Result<(u32, Option<AttachmentId>, u64), ServiceError> {
        self.connection
            .query_row(
                &format!(
                    "SELECT count(*), min(a.id), coalesce(sum(e.next_page),0) FROM attachments a
                     LEFT JOIN fulltext_extractions e ON e.attachment_id=a.id
                     WHERE {PDF_ATTACHMENT}
                       AND (e.attachment_id IS NULL OR e.status='extracting' OR e.file_hash<>a.file_hash)"
                ),
                [],
                |r| {
                    let first: Option<String> = r.get(1)?;
                    Ok((
                        count(r, 0)?,
                        first
                            .map(|f| Uuid::parse_str(&f).map(AttachmentId))
                            .transpose()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        r.get::<_, i64>(2)?.max(0) as u64,
                    ))
                },
            )
            .map_err(service_error)
    }
    /// Bounded page text read in page order; `max_chars` counts Unicode scalars overall.
    pub fn pages(&self, request: &PageTextRequest) -> Result<PageTextRead, ServiceError> {
        request.validate()?;
        let extraction = self
            .for_attachment(request.attachment)?
            .ok_or(ServiceError::NotFound)?;
        let rows = self
            .connection
            .prepare(
                "SELECT page_index, page_label, text FROM fulltext_pages
                 WHERE attachment_id=?1 AND page_index>=?2 ORDER BY page_index LIMIT ?3",
            )
            .map_err(service_error)?
            .query_map(
                params![
                    request.attachment.0.to_string(),
                    i64::from(request.from_page),
                    i64::from(request.max_pages) + 1
                ],
                |r| {
                    Ok((
                        count(r, 0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(service_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(service_error)?;
        let mut budget = request.max_chars as usize;
        let mut pages = Vec::new();
        let mut next_page = None;
        for (i, (page_index, page_label, text)) in rows.into_iter().enumerate() {
            if i == request.max_pages as usize || budget == 0 {
                next_page = Some(page_index);
                break;
            }
            let chars = text.chars().count();
            let (text, truncated) = if chars > budget {
                (text.chars().take(budget).collect(), true)
            } else {
                (text, false)
            };
            budget -= chars.min(budget);
            pages.push(PageText {
                page_index,
                page_label,
                text,
                truncated,
            });
        }
        Ok(PageTextRead {
            attachment_id: extraction.attachment_id,
            paper_id: extraction.paper_id,
            extraction,
            pages,
            next_page,
        })
    }
}
