//! #19 annotation rows. Quads are stored as JSON in unrotated PDF points; pages are 0-based.
use crate::error::service_error;
use cm_core::{
    ServiceError,
    annotations::{Anchor, Annotation, AnnotationId, AnnotationKind, AnnotationRepository, Source},
    geometry::Quad,
};
use cm_domain::{AttachmentId, PaperId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

pub struct SqliteAnnotations<'a> {
    pub(crate) connection: &'a Connection,
}
const COLUMNS: &str = "SELECT n.id,n.attachment_id,n.page_index,n.page_label,n.type,n.source,
    n.anchor_state,n.unresolved_reason,n.quote_text,n.context_before,n.context_after,n.quads_json,
    n.color,n.comment,n.confirmed_at,n.created_at,n.updated_at FROM annotations n";
const ORDER: &str = "ORDER BY n.page_index IS NULL, n.page_index, n.created_at, n.id";

fn decode(row: &Row<'_>) -> rusqlite::Result<Annotation> {
    let bad = |i: usize, m: &str| {
        rusqlite::Error::FromSqlConversionFailure(
            i,
            rusqlite::types::Type::Text,
            m.to_owned().into(),
        )
    };
    let uuid = |i: usize| -> rusqlite::Result<Uuid> {
        Uuid::parse_str(&row.get::<_, String>(i)?).map_err(|_| bad(i, "uuid"))
    };
    let anchor = match row.get::<_, String>(6)?.as_str() {
        "resolved" => {
            let quads: Vec<Quad> = match row.get::<_, Option<String>>(11)? {
                Some(json) => serde_json::from_str(&json).map_err(|_| bad(11, "quads"))?,
                None => vec![],
            };
            Anchor::Resolved {
                page_index: row.get(2)?,
                page_label: row.get(3)?,
                quads,
            }
        }
        "unresolved" => Anchor::Unresolved {
            reason: row.get(7)?,
        },
        _ => return Err(bad(6, "anchor_state")),
    };
    Ok(Annotation {
        id: AnnotationId(uuid(0)?),
        attachment_id: AttachmentId(uuid(1)?),
        kind: match row.get::<_, String>(4)?.as_str() {
            "highlight" => AnnotationKind::Highlight,
            "note" => AnnotationKind::Note,
            _ => return Err(bad(4, "type")),
        },
        source: match row.get::<_, String>(5)?.as_str() {
            "human" => Source::Human,
            "ai" => Source::Ai,
            "import" => Source::Import,
            _ => return Err(bad(5, "source")),
        },
        anchor,
        quote: row.get(8)?,
        context_before: row.get(9)?,
        context_after: row.get(10)?,
        color: row.get(12)?,
        comment: row.get(13)?,
        confirmed_at: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

type Columns = (
    Option<u32>,
    Option<String>,
    &'static str,
    Option<String>,
    Option<String>,
);
fn anchor_columns(a: &Annotation) -> Result<Columns, ServiceError> {
    Ok(match &a.anchor {
        Anchor::Resolved {
            page_index,
            page_label,
            quads,
        } => (
            Some(*page_index),
            page_label.clone(),
            "resolved",
            None,
            if quads.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_string(quads)
                        .map_err(|e| ServiceError::InvalidInput(e.to_string()))?,
                )
            },
        ),
        Anchor::Unresolved { reason } => (None, None, "unresolved", Some(reason.clone()), None),
    })
}
fn kind(k: AnnotationKind) -> &'static str {
    match k {
        AnnotationKind::Highlight => "highlight",
        AnnotationKind::Note => "note",
    }
}
fn source(s: Source) -> &'static str {
    match s {
        Source::Human => "human",
        Source::Ai => "ai",
        Source::Import => "import",
    }
}

fn insert(connection: &Connection, a: &Annotation) -> Result<(), ServiceError> {
    let (page, label, state, reason, quads) = anchor_columns(a)?;
    connection
        .execute(
            "INSERT INTO annotations(id,attachment_id,page_index,page_label,type,source,
             anchor_state,unresolved_reason,quote_text,context_before,context_after,quads_json,
             color,comment,confirmed_at,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                a.id.0.to_string(),
                a.attachment_id.0.to_string(),
                page,
                label,
                kind(a.kind),
                source(a.source),
                state,
                reason,
                a.quote,
                a.context_before,
                a.context_after,
                quads,
                a.color,
                a.comment,
                a.confirmed_at,
                a.created_at,
                a.updated_at
            ],
        )
        .map(|_| ())
        .map_err(service_error)
}

impl AnnotationRepository for SqliteAnnotations<'_> {
    fn now(&self) -> Result<String, ServiceError> {
        self.connection
            .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                r.get(0)
            })
            .map_err(service_error)
    }
    fn insert(&mut self, a: &Annotation) -> Result<(), ServiceError> {
        insert(self.connection, a)
    }
    fn insert_all(&mut self, rows: &[Annotation]) -> Result<(), ServiceError> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        for a in rows {
            insert(&transaction, a)?;
        }
        transaction.commit().map_err(service_error)
    }
    fn replace(&mut self, a: &Annotation) -> Result<(), ServiceError> {
        let (page, label, state, reason, quads) = anchor_columns(a)?;
        let changed = self
            .connection
            .execute(
                "UPDATE annotations SET page_index=?2,page_label=?3,anchor_state=?4,
                 unresolved_reason=?5,quads_json=?6,color=?7,comment=?8,confirmed_at=?9,
                 updated_at=?10 WHERE id=?1",
                params![
                    a.id.0.to_string(),
                    page,
                    label,
                    state,
                    reason,
                    quads,
                    a.color,
                    a.comment,
                    a.confirmed_at,
                    a.updated_at
                ],
            )
            .map_err(service_error)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }
    fn delete(&mut self, id: AnnotationId) -> Result<bool, ServiceError> {
        self.connection
            .execute("DELETE FROM annotations WHERE id=?1", [id.0.to_string()])
            .map(|n| n == 1)
            .map_err(service_error)
    }
    fn get(&self, id: AnnotationId) -> Result<Option<Annotation>, ServiceError> {
        self.connection
            .query_row(
                &format!("{COLUMNS} WHERE n.id=?1"),
                [id.0.to_string()],
                decode,
            )
            .optional()
            .map_err(service_error)
    }
    fn for_attachment(&self, id: AttachmentId) -> Result<Vec<Annotation>, ServiceError> {
        self.connection
            .prepare(&format!("{COLUMNS} WHERE n.attachment_id=?1 {ORDER}"))
            .map_err(service_error)?
            .query_map([id.0.to_string()], decode)
            .map_err(service_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(service_error)
    }
    fn for_paper(&self, id: PaperId) -> Result<Vec<Annotation>, ServiceError> {
        self.connection
            .prepare(&format!(
                "{COLUMNS} JOIN attachments a ON a.id=n.attachment_id WHERE a.paper_id=?1 {ORDER}"
            ))
            .map_err(service_error)?
            .query_map([id.0.to_string()], decode)
            .map_err(service_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(service_error)
    }
    fn attachment_exists(&self, id: AttachmentId) -> Result<bool, ServiceError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM attachments a JOIN papers p ON p.id=a.paper_id
                 WHERE a.id=?1 AND p.deleted_at IS NULL)",
                [id.0.to_string()],
                |r| r.get(0),
            )
            .map_err(service_error)
    }
}
