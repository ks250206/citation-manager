use crate::error::service_error;
use cm_core::{
    ServiceError,
    library_query::{
        LibraryCursor, LibraryPage, LibraryQuery, LibraryQueryRepository, SortDirection, SortField,
        SortValue, TagMatch,
    },
    papers::PaperVisibility,
};
use cm_domain::{Paper, PaperId};
use rusqlite::{Connection, params_from_iter, types::Value};
use uuid::Uuid;

pub struct SqliteLibraryQuery<'a> {
    pub(crate) connection: &'a Connection,
}
struct Predicate {
    sql: String,
    values: Vec<Value>,
}
impl Predicate {
    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.values.len())
    }
    fn and(&mut self, sql: &str) {
        self.sql.push_str(" AND (");
        self.sql.push_str(sql);
        self.sql.push(')');
    }
}
/// Metadata predicate only: compare the ASCII base media type, ignoring parameters and
/// SP/HTAB around it. Managed ingestion preserves the input. Shared by the `has_pdf`
/// filter and the page's `with_pdf` marks so the column and the filter always agree.
const PDF: &str = "EXISTS(SELECT 1 FROM attachments a WHERE a.paper_id=p.id AND a.kind='pdf' AND lower(trim(substr(a.mime_type,1,instr(a.mime_type||';',';')-1),char(9)||' '))='application/pdf')";
fn has_pdf(tx: &Connection, id: PaperId) -> Result<bool, ServiceError> {
    tx.query_row(
        &format!("SELECT {PDF} FROM papers p WHERE p.id=?1"),
        [id.0.to_string()],
        |row| row.get(0),
    )
    .map_err(service_error)
}
fn contains(expression: &str, parameter: &str) -> String {
    // instr treats %, _ and backslash literally. Builtin lower is ASCII-only.
    format!("instr(lower(coalesce({expression},'')),lower({parameter}))>0")
}
fn contributor_match(parameter: &str, authors_only: bool) -> String {
    let fields = [
        "person.given_name",
        "person.family_name",
        "person.literal_name",
        "coalesce(person.given_name,'') || ' ' || coalesce(person.family_name,'')",
        "coalesce(person.family_name,'') || ' ' || coalesce(person.given_name,'')",
    ];
    let any = fields
        .into_iter()
        .map(|f| contains(f, parameter))
        .collect::<Vec<_>>()
        .join(" OR ");
    let role = if authors_only {
        "AND pp.role='author'"
    } else {
        ""
    };
    format!(
        "EXISTS(SELECT 1 FROM paper_people pp JOIN people person ON person.id=pp.person_id WHERE pp.paper_id=p.id {role} AND ({any}))"
    )
}
fn predicate(query: &LibraryQuery) -> Result<Predicate, ServiceError> {
    let mut b = Predicate {
        sql: format!(
            "SELECT p.id,{} FROM papers p WHERE 1=1",
            optional_key(query.sort.field).unwrap_or("NULL")
        ),
        values: vec![],
    };
    let f = &query.filter;
    match f.visibility {
        PaperVisibility::Active => b.and("p.deleted_at IS NULL"),
        PaperVisibility::Deleted => b.and("p.deleted_at IS NOT NULL"),
        PaperVisibility::All => (),
    }
    if let Some(id) = f.collection {
        let v = b.bind(id.0.to_string());
        b.and(&format!("EXISTS(SELECT 1 FROM paper_collections pc WHERE pc.paper_id=p.id AND pc.collection_id={v})"));
    }
    if !f.tags.is_empty() {
        let terms = f
            .tags
            .iter()
            .map(|id| {
                let v = b.bind(id.0.to_string());
                format!(
                    "EXISTS(SELECT 1 FROM paper_tags pt WHERE pt.paper_id=p.id AND pt.tag_id={v})"
                )
            })
            .collect::<Vec<_>>();
        b.and(&terms.join(match f.tag_match {
            TagMatch::All => " AND ",
            TagMatch::Any => " OR ",
        }));
    }
    if let Some(author) = &f.author {
        let v = b.bind(author.clone());
        b.and(&contributor_match(&v, true));
    }
    if let Some(year) = f.year_from {
        let v = b.bind(year);
        b.and(&format!("p.year>={v}"));
    }
    if let Some(year) = f.year_to {
        let v = b.bind(year);
        b.and(&format!("p.year<={v}"));
    }
    if let Some(kind) = &f.paper_type {
        let v = b.bind(kind.clone());
        b.and(&format!("p.type={v}"));
    }
    if let Some(has) = f.has_pdf {
        b.and(&format!("{}{PDF}", if has { "" } else { "NOT " }));
    }
    if let Some(keyword) = &f.keyword {
        for token in keyword.split_whitespace() {
            let v = b.bind(token.to_string());
            let mut terms = [
                "p.title",
                "p.abstract",
                "p.journal",
                "p.doi_normalized",
                "p.url",
                "p.citation_key",
                "p.type",
                "p.publication_date",
                "p.language",
                "p.volume",
                "p.issue",
                "p.pages",
                "CAST(p.year AS TEXT)",
            ]
            .into_iter()
            .map(|field| contains(field, &v))
            .collect::<Vec<_>>();
            terms.push(contributor_match(&v, false));
            terms.push(format!(
                "EXISTS(SELECT 1 FROM paper_external_ids e WHERE e.paper_id=p.id AND ({} OR {}))",
                contains("e.namespace", &v),
                contains("e.value", &v)
            ));
            terms.push(format!("EXISTS(SELECT 1 FROM paper_tags pt JOIN tags t ON t.id=pt.tag_id WHERE pt.paper_id=p.id AND ({} OR {}))", contains("t.name", &v), contains("t.normalized_name", &v)));
            b.and(&terms.join(" OR "));
        }
    }
    let op = match query.sort.direction {
        SortDirection::Asc => ">",
        SortDirection::Desc => "<",
    };
    if let Some(cursor) = &query.after {
        let (id, value) = cursor.position_for(query)?;
        let id = b.bind(id.0.to_string());
        match value {
            SortValue::Id => b.and(&format!("p.id {op} {id}")),
            SortValue::Text(text) => {
                let key = b.bind(text);
                let field = sort_column(query.sort.field);
                b.and(&format!(
                    "({field} {op} {key} OR ({field}={key} AND p.id {op} {id}))"
                ));
            }
            SortValue::Year(year) => {
                let null = b.bind(i32::from(year.is_none()));
                let key = b.bind(year.unwrap_or(0));
                b.and(&format!("((p.year IS NULL)>{null} OR ((p.year IS NULL)={null} AND (coalesce(p.year,0) {op} {key} OR (coalesce(p.year,0)={key} AND p.id {op} {id}))))"));
            }
            SortValue::OptionalText(text) => {
                let field = optional_key(query.sort.field).ok_or(ServiceError::Unavailable)?;
                let null = b.bind(i32::from(text.is_none()));
                let key = b.bind(text.unwrap_or_default());
                b.and(&format!("((({field}) IS NULL)>{null} OR ((({field}) IS NULL)={null} AND (coalesce({field},'') {op} {key} OR (coalesce({field},'')={key} AND p.id {op} {id}))))"));
            }
        }
    }
    let dir = match query.sort.direction {
        SortDirection::Asc => "ASC",
        SortDirection::Desc => "DESC",
    };
    let order = match query.sort.field {
        SortField::Id => format!("p.id {dir}"),
        SortField::Year => format!("(p.year IS NULL) ASC,p.year {dir},p.id {dir}"),
        SortField::Author | SortField::Journal => {
            // Missing values sort last in both directions, like years.
            let key = optional_key(query.sort.field).ok_or(ServiceError::Unavailable)?;
            format!("(({key}) IS NULL) ASC,{key} {dir},p.id {dir}")
        }
        field => format!("{} {dir},p.id {dir}", sort_column(field)),
    };
    let limit = b.bind(i64::from(query.limit) + 1);
    b.sql.push_str(&format!(" ORDER BY {order} LIMIT {limit}"));
    Ok(b)
}
fn sort_column(field: SortField) -> &'static str {
    match field {
        SortField::Id => "p.id",
        SortField::Title => "p.title",
        SortField::Year => "p.year",
        SortField::CreatedAt => "p.created_at",
        SortField::UpdatedAt => "p.updated_at",
        SortField::Author | SortField::Journal => "p.id",
    }
}
/// SQL key of the optional-text sorts. The page records the key SQLite computed for the
/// last row, so the cursor never depends on re-deriving it from the Paper value.
fn optional_key(field: SortField) -> Option<&'static str> {
    match field {
        SortField::Journal => Some("nullif(p.journal,'')"),
        SortField::Author => Some(
            "(SELECT coalesce(nullif(person.family_name,''),nullif(person.literal_name,''),nullif(person.given_name,'')) FROM paper_people pp JOIN people person ON person.id=pp.person_id WHERE pp.paper_id=p.id AND pp.role='author' ORDER BY pp.sort_order LIMIT 1)",
        ),
        _ => None,
    }
}
fn sort_value(p: &Paper, field: SortField) -> Result<SortValue, ServiceError> {
    Ok(match field {
        SortField::Id => SortValue::Id,
        SortField::Title => SortValue::Text(p.title.clone()),
        SortField::Year => SortValue::Year(p.bibliography.year),
        SortField::CreatedAt => {
            SortValue::Text(p.created_at.clone().ok_or(ServiceError::Unavailable)?)
        }
        SortField::UpdatedAt => {
            SortValue::Text(p.updated_at.clone().ok_or(ServiceError::Unavailable)?)
        }
        // Read from the query row instead (see `optional_key`).
        SortField::Author | SortField::Journal => return Err(ServiceError::Unavailable),
    })
}
impl LibraryQueryRepository for SqliteLibraryQuery<'_> {
    fn query(&self, query: &LibraryQuery) -> Result<LibraryPage, ServiceError> {
        query.validate()?;
        let b = predicate(query)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(service_error)?;
        let ids = tx
            .prepare(&b.sql)
            .map_err(service_error)?
            .query_map(params_from_iter(b.values.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(service_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(service_error)?;
        let more = ids.len() > query.limit as usize;
        let mut papers = Vec::with_capacity(ids.len().min(query.limit as usize));
        let mut with_pdf = Vec::new();
        let mut last_key = None;
        for (id, key) in ids.into_iter().take(query.limit as usize) {
            let id = PaperId(Uuid::parse_str(&id).map_err(|_| ServiceError::Unavailable)?);
            papers.push(super::papers::read(&tx, id, true)?.ok_or(ServiceError::Unavailable)?);
            if has_pdf(&tx, id)? {
                with_pdf.push(id);
            }
            last_key = key;
        }
        let next = if more {
            let last = papers.last().ok_or(ServiceError::Unavailable)?;
            let value = if optional_key(query.sort.field).is_some() {
                SortValue::OptionalText(last_key)
            } else {
                sort_value(last, query.sort.field)?
            };
            Some(LibraryCursor::from_position(query, last.id, value)?)
        } else {
            None
        };
        tx.commit().map_err(service_error)?;
        Ok(LibraryPage {
            papers,
            next,
            with_pdf,
        })
    }
}
