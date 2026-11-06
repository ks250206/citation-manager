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
        sql: "SELECT p.id FROM papers p WHERE 1=1".into(),
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
        b.and(if has { "EXISTS(SELECT 1 FROM attachments a WHERE a.paper_id=p.id AND a.kind='pdf' AND a.mime_type='application/pdf')" } else { "NOT EXISTS(SELECT 1 FROM attachments a WHERE a.paper_id=p.id AND a.kind='pdf' AND a.mime_type='application/pdf')" });
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
        }
    }
    let dir = match query.sort.direction {
        SortDirection::Asc => "ASC",
        SortDirection::Desc => "DESC",
    };
    let order = match query.sort.field {
        SortField::Id => format!("p.id {dir}"),
        SortField::Year => format!("(p.year IS NULL) ASC,p.year {dir},p.id {dir}"),
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
                row.get::<_, String>(0)
            })
            .map_err(service_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(service_error)?;
        let more = ids.len() > query.limit as usize;
        let mut papers = Vec::with_capacity(ids.len().min(query.limit as usize));
        for id in ids.into_iter().take(query.limit as usize) {
            let id = PaperId(Uuid::parse_str(&id).map_err(|_| ServiceError::Unavailable)?);
            papers.push(super::papers::read(&tx, id, true)?.ok_or(ServiceError::Unavailable)?);
        }
        let next = if more {
            let last = papers.last().ok_or(ServiceError::Unavailable)?;
            Some(LibraryCursor::from_position(
                query,
                last.id,
                sort_value(last, query.sort.field)?,
            )?)
        } else {
            None
        };
        tx.commit().map_err(service_error)?;
        Ok(LibraryPage { papers, next })
    }
}
