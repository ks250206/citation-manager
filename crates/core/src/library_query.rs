//! Common M1 bibliography query port. No PDF text, FTS syntax, relevance or
//! snippets are claimed here. #18 can implement this port and extend search via
//! a separate fulltext contract without changing organization/filter semantics.
use crate::{ServiceError, papers::PaperVisibility};
use cm_domain::{CollectionId, Paper, PaperId, TagId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TagMatch {
    #[default]
    All,
    Any,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryFilter {
    pub visibility: PaperVisibility,
    /// Direct membership only; selecting a parent does not select descendants.
    pub collection: Option<CollectionId>,
    pub tags: Vec<TagId>,
    /// Empty tag lists impose no predicate for either mode. Duplicate IDs are a set.
    pub tag_match: TagMatch,
    /// Whitespace-separated literal substrings, AND across tokens, OR across
    /// bibliographic fields/contributors/external IDs/tags. ASCII case-insensitive;
    /// other Unicode is literal, with no normalization or tokenization promises.
    pub keyword: Option<String>,
    /// Literal substring of author-role names (given/family or literal).
    pub author: Option<String>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// Exact case-sensitive persisted type.
    pub paper_type: Option<String>,
    /// At least one attachment with kind=pdf and an ASCII case-insensitive base
    /// MIME application/pdf (parameters and surrounding SP/HTAB ignored). This is
    /// metadata presence, not a claim about file existence or PDF validity.
    pub has_pdf: Option<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SortField {
    #[default]
    Id,
    Title,
    Year,
    CreatedAt,
    UpdatedAt,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibrarySort {
    pub field: SortField,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryQuery {
    pub filter: LibraryFilter,
    pub sort: LibrarySort,
    pub after: Option<LibraryCursor>,
    pub limit: u32,
}
impl Default for LibraryQuery {
    fn default() -> Self {
        Self {
            filter: LibraryFilter::default(),
            sort: LibrarySort::default(),
            after: None,
            limit: 50,
        }
    }
}
impl LibraryQuery {
    pub fn validate(&self) -> Result<(), ServiceError> {
        if !(1..=500).contains(&self.limit) {
            return Err(invalid("limit must be 1..=500"));
        }
        let f = &self.filter;
        if f.tags.len() > 100
            || f.tags.iter().any(|id| id.0.is_nil())
            || f.collection.is_some_and(|id| id.0.is_nil())
        {
            return Err(invalid("filter requires non-nil IDs and at most 100 tags"));
        }
        for text in [&f.keyword, &f.author, &f.paper_type].into_iter().flatten() {
            if text.len() > 1024 || text.trim().is_empty() || text.contains('\0') {
                return Err(invalid(
                    "filter text must be nonblank, at most 1024 bytes and contain no NUL",
                ));
            }
        }
        if f.keyword
            .as_ref()
            .is_some_and(|s| s.split_whitespace().count() > 16)
        {
            return Err(invalid("at most 16 keyword tokens"));
        }
        if let (Some(from), Some(to)) = (f.year_from, f.year_to)
            && from > to
        {
            return Err(invalid("inverted year range"));
        }
        if let Some(cursor) = &self.after {
            cursor.position_for(self)?;
        }
        Ok(())
    }
}
fn invalid(reason: &str) -> ServiceError {
    ServiceError::InvalidInput(reason.into())
}

/// Adapter-owned key, recorded from the returned row. UUID is always the final
/// tie-breaker in the requested direction. Years with no value sort LAST in both
/// directions; strings use SQLite BINARY order. Calls do not share a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortValue {
    Id,
    Text(String),
    Year(Option<i32>),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    version: u8,
    filter: LibraryFilter,
    sort: LibrarySort,
    id: PaperId,
    value: SortValue,
}
/// Opaque versioned JSON, not a credential or authorization mechanism. Tied to
/// exact filter+sort (including tag order), independent of page size. Its stored
/// key survives deleting the anchor. Metadata changes between pages may move rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryCursor(String);
impl LibraryCursor {
    pub fn from_encoded(value: String) -> Result<Self, ServiceError> {
        let cursor = Self(value);
        let p = cursor.decode()?;
        let q = LibraryQuery {
            filter: p.filter,
            sort: p.sort,
            after: None,
            limit: 1,
        };
        q.validate()?;
        cursor.position_for(&q)?;
        Ok(cursor)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn from_position(
        query: &LibraryQuery,
        id: PaperId,
        value: SortValue,
    ) -> Result<Self, ServiceError> {
        let payload = CursorPayload {
            version: 1,
            filter: query.filter.clone(),
            sort: query.sort,
            id,
            value,
        };
        let encoded = serde_json::to_string(&payload).map_err(|_| invalid("invalid cursor"))?;
        Self::from_encoded(encoded)
    }
    fn decode(&self) -> Result<CursorPayload, ServiceError> {
        if self.0.len() > 1_048_576 {
            return Err(invalid("cursor exceeds 1 MiB"));
        }
        serde_json::from_str(&self.0).map_err(|_| invalid("malformed cursor"))
    }
    pub fn position_for(&self, query: &LibraryQuery) -> Result<(PaperId, SortValue), ServiceError> {
        let p = self.decode()?;
        if p.version != 1 || p.id.0.is_nil() || p.filter != query.filter || p.sort != query.sort {
            return Err(invalid("cursor version, ID, filter or sort mismatch"));
        }
        if !matches!(
            (p.sort.field, &p.value),
            (SortField::Id, SortValue::Id)
                | (SortField::Year, SortValue::Year(_))
                | (
                    SortField::Title | SortField::CreatedAt | SortField::UpdatedAt,
                    SortValue::Text(_)
                )
        ) {
            return Err(invalid("cursor sort value mismatch"));
        }
        Ok((p.id, p.value))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryPage {
    pub papers: Vec<Paper>,
    pub next: Option<LibraryCursor>,
}
pub trait LibraryQueryRepository {
    /// One consistent read transaction per page; at most limit full Paper values,
    /// never lossy summary reconstructions. Filters are ANDed with each other.
    fn query(&self, query: &LibraryQuery) -> Result<LibraryPage, ServiceError>;
}
pub struct LibraryQueryService<R> {
    repository: R,
}
impl<R: LibraryQueryRepository> LibraryQueryService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
    pub fn query(&self, query: &LibraryQuery) -> Result<LibraryPage, ServiceError> {
        query.validate()?;
        self.repository.query(query)
    }
}
