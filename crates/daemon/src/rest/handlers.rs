//! Endpoint handlers (DESIGN §13.2; one per `ENDPOINTS` REST row; docs/openapi.json).
//!
//! Every handler takes `Ctx` first (authentication + the row's scopes), then parses its
//! inputs and runs daemon commands through `Ctx::run`. Write responses never echo stored
//! data the token could not read: a write-only token gets ids and timestamps, not records.
pub use super::ctx::App;
use super::{
    BASE_PATH,
    ctx::{Calls, Ctx, decode_cursor, encode_cursor, json_body, present, query, uuid, uuid_list},
    edge,
    error::ApiError,
};
use axum::{
    Json, Router,
    body::Body,
    extract::Path,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{MethodRouter, delete, get, patch, post},
};
use cm_core::{
    import::normalize::MAX_BATCH,
    papers::{PaperPatch, PaperVisibility},
    policy::Operation,
};
use cm_protocol::{
    annotations::{
        Anchor, Annotation, AnnotationKind, AnnotationPatch, AnnotationReply, AnnotationRequest,
        Edit, NewAnnotation, Source,
    },
    attachments::{PdfReply, RenderFailure},
    domain::{AttachmentId, CollectionId, Paper, PaperId, TagId},
    exchange::{ExportRequest, ExportSelection, Format, ImportReply, ImportRequestUi},
    geometry::Quad,
    import::{ImportAction, ImportCommit, ImportDecision, ImportDestination, ImportPreview},
    ipc::{Command, PdfCommand, Reply},
    jobs::{JobCommand, JobId, JobReply},
    library::{LibraryReply, LibraryRequest},
    library_query::{
        LibraryCursor, LibraryFilter, LibraryQuery, LibrarySort, SortDirection, SortField, TagMatch,
    },
    organization::{CollectionInput, OrganizationItem, OrganizationRequest, TagInput},
    search::{
        FulltextReply, FulltextRequest, PaperPageTextRequest, SearchCursor, SearchHit, SearchReply,
        SearchRequest, ShortTermScope, SourceSet,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Ids accepted in one bulk request.
pub const MAX_BULK_IDS: usize = 500;
pub const DEFAULT_PAGE_LIMIT: u32 = 50;
pub const DEFAULT_FULLTEXT_PAGES: u32 = 10;
pub const DEFAULT_FULLTEXT_CHARS: u32 = 100_000;
pub const MAX_QUERY_BYTES: usize = 4096;
pub const MAX_DOI_BYTES: usize = 512;
pub const MAX_FILE_NAME_BYTES: usize = 255;
/// Parse diagnostics returned by `POST /imports` (the rest are counted).
pub const MAX_IMPORT_DIAGNOSTICS: usize = 200;
/// Pixels of one REST page render (RGBA, base64 in JSON: about 21 MB at the cap).
pub const MAX_RENDER_PIXELS: u64 = 4_000_000;

/// Every route, keyed by its `ENDPOINTS` name. The router is built only from this list and
/// the contract tests compare it with `ENDPOINTS` and docs/openapi.json.
pub fn routes() -> Vec<(&'static str, MethodRouter<App>)> {
    vec![
        ("GET /health", get(health)),
        ("GET /papers", get(list_papers)),
        ("GET /papers/{id}", get(get_paper)),
        ("POST /papers", post(create_paper)),
        ("PATCH /papers/{id}", patch(patch_paper)),
        ("DELETE /papers/{id}", delete(trash_paper)),
        ("POST /papers/{id}/restore", post(restore_paper)),
        ("POST /papers/bulk-delete", post(bulk_delete)),
        ("POST /papers/resolve-doi", post(resolve_doi)),
        ("GET /search", get(search)),
        ("GET /papers/{id}/fulltext/status", get(fulltext_status)),
        ("GET /papers/{id}/fulltext", get(fulltext)),
        ("GET /attachments/{id}/pages/{page}", get(render_page)),
        ("GET /papers/{id}/annotations", get(list_annotations)),
        (
            "POST /attachments/{id}/annotations",
            post(create_annotation),
        ),
        ("PATCH /annotations/{id}", patch(patch_annotation)),
        ("DELETE /annotations/{id}", delete(delete_annotation)),
        ("GET /collections", get(list_collections)),
        ("POST /collections", post(create_collection)),
        ("PATCH /collections/{id}", patch(update_collection)),
        ("DELETE /collections/{id}", delete(delete_collection)),
        ("POST /collections/{id}/papers", post(collection_member)),
        ("POST /collections/{id}/papers/bulk-move", post(bulk_move)),
        ("GET /tags", get(list_tags)),
        ("POST /tags", post(create_tag)),
        ("DELETE /tags/{id}", delete(delete_tag)),
        ("POST /imports", post(import)),
        ("POST /exports", post(export)),
        ("GET /jobs/{id}", get(get_job)),
        ("POST /jobs/{id}/cancel", post(cancel_job)),
    ]
}

pub fn router(app: App) -> Router {
    let mut router = Router::new();
    for (name, method_router) in routes() {
        let (_, path) = name.split_once(' ').expect("METHOD /path");
        router = router.route(&format!("{BASE_PATH}{path}"), method_router);
    }
    router
        .fallback(fallback)
        .layer(middleware::from_fn_with_state(app.clone(), edge::edge))
        .with_state(app)
}

async fn fallback() -> ApiError {
    ApiError::for_status(StatusCode::NOT_FOUND)
}

fn ok(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}
fn created(location: String, value: Value) -> Response {
    let mut r = (StatusCode::CREATED, Json(value)).into_response();
    if let Ok(v) = HeaderValue::from_str(&location) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}
fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}
fn unexpected<T>(_: Reply) -> Result<T, ApiError> {
    Err(ApiError::internal())
}
fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn paper_id(text: &str) -> Result<PaperId, ApiError> {
    uuid(text, "paper id").map(PaperId)
}
fn collection_id(text: &str) -> Result<CollectionId, ApiError> {
    uuid(text, "collection id").map(CollectionId)
}

// ---------------------------------------------------------------- health

async fn health(_ctx: Ctx) -> Response {
    ok(json!({ "status": "ok" }))
}

// ---------------------------------------------------------------- papers

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Visibility {
    Active,
    Deleted,
    All,
}
impl From<Visibility> for PaperVisibility {
    fn from(v: Visibility) -> Self {
        match v {
            Visibility::Active => Self::Active,
            Visibility::Deleted => Self::Deleted,
            Visibility::All => Self::All,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListPapersQuery {
    limit: Option<u32>,
    cursor: Option<String>,
    visibility: Option<Visibility>,
    collection: Option<String>,
    tags: Option<String>,
    tag_match: Option<String>,
    keyword: Option<String>,
    author: Option<String>,
    year_from: Option<i32>,
    year_to: Option<i32>,
    #[serde(rename = "type")]
    paper_type: Option<String>,
    has_pdf: Option<bool>,
    sort: Option<String>,
    direction: Option<String>,
}

async fn list_papers(ctx: Ctx, uri: Uri) -> Result<Response, ApiError> {
    let q: ListPapersQuery = query(&uri)?;
    let filter = LibraryFilter {
        visibility: q.visibility.map(Into::into).unwrap_or_default(),
        collection: q.collection.as_deref().map(collection_id).transpose()?,
        tags: q
            .tags
            .as_deref()
            .map(|t| uuid_list(t, "tag id", 100))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(TagId)
            .collect(),
        tag_match: match q.tag_match.as_deref() {
            None | Some("all") => TagMatch::All,
            Some("any") => TagMatch::Any,
            Some(_) => return Err(ApiError::invalid("tag_match must be all or any")),
        },
        keyword: q.keyword,
        author: q.author,
        year_from: q.year_from,
        year_to: q.year_to,
        paper_type: q.paper_type,
        has_pdf: q.has_pdf,
    };
    let sort = LibrarySort {
        field: match q.sort.as_deref() {
            None | Some("id") => SortField::Id,
            Some("title") => SortField::Title,
            Some("year") => SortField::Year,
            Some("created_at") => SortField::CreatedAt,
            Some("updated_at") => SortField::UpdatedAt,
            Some(_) => {
                return Err(ApiError::invalid(
                    "sort must be id, title, year, created_at or updated_at",
                ));
            }
        },
        direction: match q.direction.as_deref() {
            None | Some("asc") => SortDirection::Asc,
            Some("desc") => SortDirection::Desc,
            Some(_) => return Err(ApiError::invalid("direction must be asc or desc")),
        },
    };
    let after = match q.cursor {
        Some(c) => Some(LibraryCursor::from_encoded(decode_cursor(&c)?).map_err(ApiError::from)?),
        None => None,
    };
    let request = LibraryQuery {
        filter,
        sort,
        after,
        limit: q.limit.unwrap_or(DEFAULT_PAGE_LIMIT),
    };
    let page = ctx
        .run(move |c| match c.call(Command::Query(request))? {
            Reply::Page(page) => Ok(page),
            other => unexpected(other),
        })
        .await?;
    Ok(ok(json!({
        "papers": page.papers,
        "next_cursor": page.next.map(|c| encode_cursor(c.as_str())),
    })))
}

async fn get_paper(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = paper_id(&id)?;
    let (paper, organization, attachments) = ctx
        .run(move |c| {
            let paper = match c.call(Command::Library(LibraryRequest::Get(id)))? {
                Reply::Library(LibraryReply::Paper(p)) => *p,
                other => return unexpected(other),
            };
            let organization =
                match c.call(Command::Organization(OrganizationRequest::ForPaper(id)))? {
                    Reply::Organization(o) => o,
                    other => return unexpected(other),
                };
            let attachments = match c.call(Command::Pdf(PdfCommand::List(id)))? {
                Reply::Pdf(PdfReply::Attachments(a)) => a,
                other => return unexpected(other),
            };
            Ok((paper, organization, attachments))
        })
        .await?;
    let attachments: Vec<Value> = attachments
        .into_iter()
        .map(|(a, status)| {
            json!({
                "id": a.id,
                "original_name": a.original_name,
                "media_type": a.media_type,
                "byte_size": a.byte_size,
                "sha256": a.sha256.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "status": to_value(&status),
            })
        })
        .collect();
    Ok(ok(json!({
        "paper": paper,
        "collections": organization.collections,
        "tags": organization.tags,
        "attachments": attachments,
    })))
}

async fn create_paper(ctx: Ctx, headers: HeaderMap, body: Body) -> Result<Response, ApiError> {
    let patch: PaperPatch = json_body(&headers, body, ctx.limits().json_body).await?;
    let input = patch.into_new_paper().map_err(ApiError::from)?;
    let paper = ctx
        .run(
            move |c| match c.call(Command::Library(LibraryRequest::Create(Box::new(input))))? {
                Reply::Library(LibraryReply::Paper(p)) => Ok(*p),
                other => unexpected(other),
            },
        )
        .await?;
    Ok(created(
        format!("{BASE_PATH}/papers/{}", paper.id.0),
        json!({ "paper": paper }),
    ))
}

/// `{id, updated_at}` plus the record when the token can read it.
fn paper_write_reply(ctx: &Ctx, paper: &Paper) -> Value {
    let mut reply = json!({ "id": paper.id, "updated_at": paper.updated_at });
    if ctx.may(Operation::PaperGet) {
        reply["paper"] = to_value(paper);
    }
    reply
}

async fn patch_paper(
    ctx: Ctx,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let id = paper_id(&id)?;
    let patch: PaperPatch = json_body(&headers, body, ctx.limits().json_body).await?;
    if patch.is_empty() {
        return Err(ApiError::invalid("the patch changes nothing"));
    }
    let paper = ctx
        .run(move |c| {
            match c.call(Command::Library(LibraryRequest::Patch(id, Box::new(patch))))? {
                Reply::Library(LibraryReply::Paper(p)) => Ok(*p),
                other => unexpected(other),
            }
        })
        .await?;
    Ok(ok(paper_write_reply(&ctx, &paper)))
}

async fn lifecycle(ctx: &Ctx, id: PaperId, deleted: bool) -> Result<Response, ApiError> {
    let paper = ctx
        .run(
            move |c| match c.call(Command::Lifecycle { paper: id, deleted })? {
                Reply::Paper(p) => Ok(*p),
                other => unexpected(other),
            },
        )
        .await?;
    Ok(ok(
        json!({ "id": paper.id, "deleted": paper.deleted_at.is_some() }),
    ))
}
async fn trash_paper(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = paper_id(&id)?;
    lifecycle(&ctx, id, true).await
}
async fn restore_paper(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = paper_id(&id)?;
    lifecycle(&ctx, id, false).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BulkDeleteBody {
    paper_ids: Vec<PaperId>,
}
fn bulk_ids(ids: &[PaperId]) -> Result<(), ApiError> {
    let unique: std::collections::BTreeSet<_> = ids.iter().map(|p| p.0).collect();
    if ids.is_empty()
        || ids.len() > MAX_BULK_IDS
        || unique.len() != ids.len()
        || unique.contains(&uuid::Uuid::nil())
    {
        return Err(ApiError::invalid(format!(
            "paper_ids must hold 1..={MAX_BULK_IDS} distinct non-nil ids"
        )));
    }
    Ok(())
}
/// Per-item outcome. A denial (revoked token, elevation ended) stops the batch.
/// Runs a batch one item at a time. A denial (token revoked, elevation ended), the request
/// deadline or a client disconnect stops it; the response still lists every id (later ones
/// as `not_attempted`) and `stopped` names the reason, so a caller sees what was done.
fn run_batch(
    c: &mut Calls,
    ids: Vec<PaperId>,
    mut item: impl FnMut(&mut Calls, PaperId) -> Result<(), ApiError>,
) -> Value {
    let mut results = Vec::with_capacity(ids.len());
    let mut stopped: Option<&'static str> = None;
    for id in ids {
        if stopped.is_some() {
            results.push(json!({ "id": id, "ok": false, "error": "not_attempted" }));
            continue;
        }
        match item(c, id) {
            Ok(()) => results.push(json!({ "id": id, "ok": true })),
            Err(e) => {
                if matches!(
                    e.status,
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::GATEWAY_TIMEOUT
                ) || e.code == "cancelled"
                {
                    stopped = Some(e.code);
                }
                results.push(json!({ "id": id, "ok": false, "error": e.code }));
            }
        }
    }
    json!({ "results": results, "stopped": stopped })
}

async fn bulk_delete(ctx: Ctx, headers: HeaderMap, body: Body) -> Result<Response, ApiError> {
    let body: BulkDeleteBody = json_body(&headers, body, ctx.limits().json_body).await?;
    bulk_ids(&body.paper_ids)?;
    let reply = ctx
        .run(move |c| {
            Ok(run_batch(c, body.paper_ids, |c, id| {
                c.call(Command::Lifecycle {
                    paper: id,
                    deleted: true,
                })
                .map(|_| ())
            }))
        })
        .await?;
    Ok(ok(reply))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveBody {
    doi: String,
}
async fn resolve_doi(ctx: Ctx, headers: HeaderMap, body: Body) -> Result<Response, ApiError> {
    let body: ResolveBody = json_body(&headers, body, ctx.limits().json_body).await?;
    if body.doi.trim().is_empty() || body.doi.len() > MAX_DOI_BYTES {
        return Err(ApiError::invalid(format!(
            "doi must be 1..={MAX_DOI_BYTES} bytes"
        )));
    }
    let resolution = ctx
        .run(move |c| match c.call(Command::Resolve { doi: body.doi })? {
            Reply::Resolution(r) => Ok(*r),
            other => unexpected(other),
        })
        .await?;
    Ok(ok(json!({ "resolution": resolution })))
}

// ---------------------------------------------------------------- search & full text

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQuery {
    q: String,
    limit: Option<u32>,
    cursor: Option<String>,
    visibility: Option<Visibility>,
    collection: Option<String>,
    papers: Option<String>,
    sources: Option<String>,
    short_term_scope: Option<String>,
    candidate_limit: Option<u32>,
    snippet_chars: Option<u32>,
    timeout_ms: Option<u32>,
}

fn search_cursor(cursor: &SearchCursor) -> String {
    encode_cursor(&format!("{}.{}", cursor.scan_after, cursor.offset))
}
fn parse_search_cursor(text: &str) -> Result<SearchCursor, ApiError> {
    let raw = decode_cursor(text)?;
    let (scan, offset) = raw
        .split_once('.')
        .ok_or_else(|| ApiError::invalid("invalid cursor"))?;
    Ok(SearchCursor {
        scan_after: scan
            .parse()
            .map_err(|_| ApiError::invalid("invalid cursor"))?,
        offset: offset
            .parse()
            .map_err(|_| ApiError::invalid("invalid cursor"))?,
    })
}

fn hit_value(hit: SearchHit) -> Value {
    json!({
        "paper_id": hit.paper_id,
        "title": hit.paper_title,
        "doi": hit.doi,
        "deleted": hit.deleted,
        "source": to_value(&hit.source),
        "attachment_id": hit.attachment_id,
        "annotation_id": hit.annotation_id,
        "page_index": hit.page_index,
        "page_label": hit.page_label,
        "matched_fields": to_value(&hit.matched_fields),
        "snippet_field": to_value(&hit.snippet_field),
        "snippet": hit.snippet,
        "highlights": hit.highlights.iter().map(|h| json!({"start": h.start, "end": h.end})).collect::<Vec<_>>(),
        "snippet_truncated": hit.snippet_truncated,
        "score": hit.score,
    })
}

async fn search(ctx: Ctx, uri: Uri) -> Result<Response, ApiError> {
    let q: SearchQuery = query(&uri)?;
    if q.q.len() > MAX_QUERY_BYTES {
        return Err(ApiError::invalid(format!(
            "q must be at most {MAX_QUERY_BYTES} bytes"
        )));
    }
    let mut request = SearchRequest::new(q.q);
    if let Some(v) = q.visibility {
        request.visibility = v.into();
    }
    request.collection = q.collection.as_deref().map(collection_id).transpose()?;
    if let Some(papers) = &q.papers {
        request.papers = uuid_list(papers, "paper id", cm_core::search::types::MAX_SCOPE_PAPERS)?
            .into_iter()
            .map(PaperId)
            .collect();
    }
    if let Some(sources) = &q.sources {
        let mut set = SourceSet {
            bibliographic: false,
            annotations: false,
            pages: false,
        };
        for s in sources.split(',').filter(|s| !s.is_empty()) {
            match s {
                "bibliographic" => set.bibliographic = true,
                "annotations" => set.annotations = true,
                "pages" => set.pages = true,
                _ => {
                    return Err(ApiError::invalid(
                        "sources must list bibliographic, annotations or pages",
                    ));
                }
            }
        }
        request.sources = set;
    }
    request.short_term_scope = match q.short_term_scope.as_deref() {
        None | Some("bounded") => ShortTermScope::Bounded,
        Some("all_pages") => ShortTermScope::AllPages,
        Some(_) => {
            return Err(ApiError::invalid(
                "short_term_scope must be bounded or all_pages",
            ));
        }
    };
    if let Some(limit) = q.limit {
        request.limit = limit;
    }
    if let Some(n) = q.candidate_limit {
        request.candidate_limit = n;
    }
    if let Some(n) = q.snippet_chars {
        request.snippet_chars = n;
    }
    if let Some(n) = q.timeout_ms {
        request.timeout_ms = n;
    }
    request.cursor = q.cursor.as_deref().map(parse_search_cursor).transpose()?;
    request.validate().map_err(ApiError::from)?;
    let reply = ctx
        .run(move |c| match c.call(Command::Search(Box::new(request)))? {
            Reply::Search(reply) => Ok(reply),
            other => unexpected(other),
        })
        .await?;
    let page = match reply {
        SearchReply::Results(page) => *page,
        SearchReply::Invalid(errors) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                "the search query did not parse",
            )
            .with_details(to_value(&errors)));
        }
    };
    let coverage = &page.coverage;
    Ok(ok(json!({
        "results": page.hits.into_iter().map(hit_value).collect::<Vec<_>>(),
        "next_cursor": page.next_page.as_ref().map(search_cursor),
        "total_hits": page.total_hits,
        "coverage": {
            "complete": coverage.complete,
            "fts_truncated": coverage.fts_truncated,
            "timed_out": coverage.timed_out,
            "short_terms": coverage.short_terms,
            "scan": to_value(&coverage.scan),
            "scanned_documents": coverage.scanned_documents,
            "unsearched": to_value(&coverage.unsearched),
            "continuation": coverage.continuation.as_ref().map(search_cursor),
            "unresolved": to_value(&coverage.unresolved),
        },
    })))
}

async fn fulltext_status(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = paper_id(&id)?;
    let status = ctx
        .run(
            move |c| match c.call(Command::Fulltext(FulltextRequest::Status(id)))? {
                Reply::Fulltext(FulltextReply::Status(s)) => Ok(s),
                other => unexpected(other),
            },
        )
        .await?;
    Ok(ok(json!({ "attachments": status })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FulltextQuery {
    attachment: Option<String>,
    from_page: Option<u32>,
    max_pages: Option<u32>,
    max_chars: Option<u32>,
}
async fn fulltext(ctx: Ctx, Path(id): Path<String>, uri: Uri) -> Result<Response, ApiError> {
    let paper = paper_id(&id)?;
    let q: FulltextQuery = query(&uri)?;
    let request = PaperPageTextRequest {
        paper,
        attachment: q
            .attachment
            .as_deref()
            .map(|a| uuid(a, "attachment id").map(AttachmentId))
            .transpose()?,
        from_page: q.from_page.unwrap_or(0),
        max_pages: q.max_pages.unwrap_or(DEFAULT_FULLTEXT_PAGES),
        max_chars: q.max_chars.unwrap_or(DEFAULT_FULLTEXT_CHARS),
    };
    let read = ctx
        .run(
            move |c| match c.call(Command::Fulltext(FulltextRequest::PaperPages(request)))? {
                Reply::Fulltext(FulltextReply::Pages(read)) => Ok(*read),
                other => unexpected(other),
            },
        )
        .await?;
    Ok(ok(to_value(&read)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderQuery {
    zoom: Option<f32>,
}
async fn render_page(
    ctx: Ctx,
    Path((id, page)): Path<(String, String)>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let attachment = AttachmentId(uuid(&id, "attachment id")?);
    let page: u32 = page
        .parse()
        .map_err(|_| ApiError::invalid("page must be a 0-based page index"))?;
    let q: RenderQuery = query(&uri)?;
    let zoom = q.zoom.unwrap_or(1.0);
    if !zoom.is_finite()
        || !(cm_protocol::attachments::MIN_RENDER_ZOOM..=cm_protocol::attachments::MAX_RENDER_ZOOM)
            .contains(&zoom)
    {
        return Err(ApiError::invalid("zoom must be 0.1..=8"));
    }
    let rendered = ctx
        .run(move |c| {
            match c.call(Command::Pdf(PdfCommand::Render {
                attachment,
                page,
                zoom,
            }))? {
                Reply::Pdf(PdfReply::Page(page)) => Ok(page),
                other => unexpected(other),
            }
        })
        .await?;
    let page = rendered.map_err(|failure| {
        let (status, code) = match &failure {
            RenderFailure::RuntimeUnavailable(_) => {
                (StatusCode::SERVICE_UNAVAILABLE, "pdf_runtime_unavailable")
            }
            RenderFailure::Busy | RenderFailure::Cancelled => {
                (StatusCode::SERVICE_UNAVAILABLE, "busy")
            }
            RenderFailure::MissingOrCorrupt => (StatusCode::CONFLICT, "attachment_unavailable"),
            RenderFailure::PageOutOfRange => (StatusCode::NOT_FOUND, "page_out_of_range"),
            RenderFailure::TooLarge => (StatusCode::BAD_REQUEST, "too_large"),
            RenderFailure::Encrypted | RenderFailure::Damaged => {
                (StatusCode::UNPROCESSABLE_ENTITY, "pdf_unreadable")
            }
            RenderFailure::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        // Fixed messages only: renderer details may name local paths.
        let message = match failure {
            RenderFailure::RuntimeUnavailable(_) | RenderFailure::Other(_) => {
                "the PDF renderer is not available".to_owned()
            }
            other => other.to_string(),
        };
        ApiError::new(status, code, message)
    })?;
    if u64::from(page.width) * u64::from(page.height) > MAX_RENDER_PIXELS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "too_large",
            format!("the page exceeds {MAX_RENDER_PIXELS} pixels at this zoom; lower the zoom"),
        ));
    }
    Ok(ok(json!({
        "attachment_id": page.attachment,
        "page": page.page,
        "page_count": page.page_count,
        "label": page.label,
        "rotation": to_value(&page.rotation),
        "zoom": page.zoom,
        "width": page.width,
        "height": page.height,
        "has_text": page.has_text,
        "format": "rgba8",
        "data": cm_protocol::ipc::b64::encode(&page.rgba),
    })))
}

// ---------------------------------------------------------------- annotations

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AnchorView {
    Resolved {
        page_index: u32,
        #[serde(default)]
        page_label: Option<String>,
        #[serde(default)]
        quads: Vec<Quad>,
    },
    Unresolved {
        reason: String,
    },
}
impl From<Anchor> for AnchorView {
    fn from(a: Anchor) -> Self {
        match a {
            Anchor::Resolved {
                page_index,
                page_label,
                quads,
            } => Self::Resolved {
                page_index,
                page_label,
                quads,
            },
            Anchor::Unresolved { reason } => Self::Unresolved { reason },
        }
    }
}
impl From<AnchorView> for Anchor {
    fn from(a: AnchorView) -> Self {
        match a {
            AnchorView::Resolved {
                page_index,
                page_label,
                quads,
            } => Self::Resolved {
                page_index,
                page_label,
                quads,
            },
            AnchorView::Unresolved { reason } => Self::Unresolved { reason },
        }
    }
}
fn kind_name(kind: AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Highlight => "highlight",
        AnnotationKind::Note => "note",
    }
}
fn source_name(source: Source) -> &'static str {
    match source {
        Source::Human => "human",
        Source::Ai => "ai",
        Source::Import => "import",
    }
}
fn annotation_value(a: Annotation) -> Value {
    json!({
        "id": a.id.0,
        "attachment_id": a.attachment_id,
        "kind": kind_name(a.kind),
        "source": source_name(a.source),
        "anchor": to_value(&AnchorView::from(a.anchor)),
        "quote": a.quote,
        "context_before": a.context_before,
        "context_after": a.context_after,
        "color": a.color,
        "comment": a.comment,
        "confirmed_at": a.confirmed_at,
        "created_at": a.created_at,
        "updated_at": a.updated_at,
    })
}

async fn list_annotations(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = paper_id(&id)?;
    let list = ctx
        .run(
            move |c| match c.call(Command::Annotations(AnnotationRequest::ForPaper(id)))? {
                Reply::Annotations(AnnotationReply::List(list)) => Ok(list),
                other => unexpected(other),
            },
        )
        .await?;
    Ok(ok(json!({
        "annotations": list.into_iter().map(annotation_value).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAnnotationBody {
    kind: String,
    #[serde(default)]
    source: Option<String>,
    anchor: AnchorView,
    quote: String,
    #[serde(default)]
    context_before: Option<String>,
    #[serde(default)]
    context_after: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    comment: Option<String>,
}

async fn create_annotation(
    ctx: Ctx,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let attachment = AttachmentId(uuid(&id, "attachment id")?);
    let body: NewAnnotationBody = json_body(&headers, body, ctx.limits().json_body).await?;
    let kind = match body.kind.as_str() {
        "highlight" => AnnotationKind::Highlight,
        "note" => AnnotationKind::Note,
        _ => return Err(ApiError::invalid("kind must be highlight or note")),
    };
    // API writes are not made by the person at the GUI: always unreviewed `ai` (a `human`
    // source would skip the GUI review; the daemon refuses it for every token as well).
    let source = match body.source.as_deref() {
        None | Some("ai") => Source::Ai,
        Some(_) => return Err(ApiError::invalid("source must be ai")),
    };
    let input = NewAnnotation {
        attachment_id: attachment,
        kind,
        source,
        anchor: body.anchor.into(),
        quote: body.quote,
        context_before: body.context_before,
        context_after: body.context_after,
        color: body.color,
        comment: body.comment,
    };
    let annotation = ctx
        .run(move |c| {
            match c.call(Command::Annotations(AnnotationRequest::Create(Box::new(
                input,
            ))))? {
                Reply::Annotations(AnnotationReply::Edit(edit)) => match *edit {
                    Edit::Created(a) => Ok(a),
                    _ => Err(ApiError::internal()),
                },
                other => unexpected(other),
            }
        })
        .await?;
    Ok(created(
        format!("{BASE_PATH}/annotations/{}", annotation.id.0),
        json!({ "annotation": annotation_value(annotation) }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnnotationPatchBody {
    #[serde(default, deserialize_with = "present")]
    color: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    comment: Option<Option<String>>,
}
fn annotation_id(text: &str) -> Result<cm_protocol::annotations::AnnotationId, ApiError> {
    uuid(text, "annotation id").map(cm_protocol::annotations::AnnotationId)
}

async fn patch_annotation(
    ctx: Ctx,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let id = annotation_id(&id)?;
    let body: AnnotationPatchBody = json_body(&headers, body, ctx.limits().json_body).await?;
    if body.color.is_none() && body.comment.is_none() {
        return Err(ApiError::invalid("the patch changes nothing"));
    }
    let patch = AnnotationPatch {
        color: body.color,
        comment: body.comment,
    };
    let after = ctx
        .run(
            move |c| match c.call(Command::Annotations(AnnotationRequest::Update(id, patch)))? {
                Reply::Annotations(AnnotationReply::Edit(edit)) => match *edit {
                    Edit::Updated { after, .. } => Ok(after),
                    _ => Err(ApiError::internal()),
                },
                other => unexpected(other),
            },
        )
        .await?;
    let mut reply = json!({ "id": after.id.0, "updated_at": after.updated_at });
    // Quote/context are annotation text: only for tokens that can read annotations.
    if ctx.may(Operation::AnnotationRead) {
        reply["annotation"] = annotation_value(after);
    }
    Ok(ok(reply))
}

async fn delete_annotation(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = annotation_id(&id)?;
    ctx.run(
        move |c| match c.call(Command::Annotations(AnnotationRequest::Delete(id)))? {
            Reply::Annotations(_) => Ok(()),
            other => unexpected(other),
        },
    )
    .await?;
    Ok(no_content())
}

// ---------------------------------------------------------------- collections & tags

async fn organization(
    ctx: &Ctx,
    request: OrganizationRequest,
) -> Result<cm_protocol::organization::Organization, ApiError> {
    ctx.run(move |c| match c.call(Command::Organization(request))? {
        Reply::Organization(o) => Ok(o),
        other => unexpected(other),
    })
    .await
}

async fn list_collections(ctx: Ctx) -> Result<Response, ApiError> {
    let o = organization(&ctx, OrganizationRequest::List).await?;
    Ok(ok(json!({ "collections": o.collections })))
}
async fn list_tags(ctx: Ctx) -> Result<Response, ApiError> {
    let o = organization(&ctx, OrganizationRequest::List).await?;
    Ok(ok(json!({ "tags": o.tags })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewCollectionBody {
    name: String,
    #[serde(default)]
    parent: Option<CollectionId>,
    #[serde(default)]
    sort_order: i64,
}
async fn create_collection(ctx: Ctx, headers: HeaderMap, body: Body) -> Result<Response, ApiError> {
    let body: NewCollectionBody = json_body(&headers, body, ctx.limits().json_body).await?;
    let o = organization(
        &ctx,
        OrganizationRequest::CreateCollection(CollectionInput {
            name: body.name,
            parent: body.parent,
            sort_order: body.sort_order,
        }),
    )
    .await?;
    match o.item {
        Some(OrganizationItem::Collection(c)) => Ok(created(
            format!("{BASE_PATH}/collections/{}", c.id.0),
            json!({ "collection": c }),
        )),
        _ => Err(ApiError::internal()),
    }
}

/// Replaces name, parent and order (all required: no read of the current values).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionBody {
    name: String,
    #[serde(default, deserialize_with = "present")]
    parent: Option<Option<CollectionId>>,
    sort_order: i64,
}
async fn update_collection(
    ctx: Ctx,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let id = collection_id(&id)?;
    let body: CollectionBody = json_body(&headers, body, ctx.limits().json_body).await?;
    let Some(parent) = body.parent else {
        return Err(ApiError::invalid(
            "parent is required (null for a top-level collection)",
        ));
    };
    let o = organization(
        &ctx,
        OrganizationRequest::UpdateCollection(
            id,
            CollectionInput {
                name: body.name,
                parent,
                sort_order: body.sort_order,
            },
        ),
    )
    .await?;
    match o.item {
        Some(OrganizationItem::Collection(c)) => Ok(ok(json!({ "collection": c }))),
        _ => Err(ApiError::internal()),
    }
}
async fn delete_collection(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = collection_id(&id)?;
    organization(&ctx, OrganizationRequest::DeleteCollection(id)).await?;
    Ok(no_content())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberBody {
    paper_id: PaperId,
    #[serde(default = "yes")]
    member: bool,
}
fn yes() -> bool {
    true
}
async fn collection_member(
    ctx: Ctx,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let id = collection_id(&id)?;
    let body: MemberBody = json_body(&headers, body, ctx.limits().json_body).await?;
    organization(
        &ctx,
        OrganizationRequest::CollectionMember(body.paper_id, id, body.member),
    )
    .await?;
    Ok(ok(json!({
        "collection_id": id,
        "paper_id": body.paper_id,
        "member": body.member,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BulkMoveBody {
    paper_ids: Vec<PaperId>,
    /// Also remove each paper from this collection (a move rather than an add).
    #[serde(default)]
    from: Option<CollectionId>,
}
async fn bulk_move(
    ctx: Ctx,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let id = collection_id(&id)?;
    let body: BulkMoveBody = json_body(&headers, body, ctx.limits().json_body).await?;
    bulk_ids(&body.paper_ids)?;
    if body.from == Some(id) {
        return Err(ApiError::invalid(
            "from must differ from the target collection",
        ));
    }
    let from = body.from;
    let reply = ctx
        .run(move |c| {
            Ok(run_batch(c, body.paper_ids, |c, paper| {
                c.call(Command::Organization(
                    OrganizationRequest::CollectionMember(paper, id, true),
                ))?;
                if let Some(from) = from {
                    c.call(Command::Organization(
                        OrganizationRequest::CollectionMember(paper, from, false),
                    ))?;
                }
                Ok(())
            }))
        })
        .await?;
    Ok(ok(reply))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTagBody {
    name: String,
    #[serde(default)]
    color: Option<String>,
}
async fn create_tag(ctx: Ctx, headers: HeaderMap, body: Body) -> Result<Response, ApiError> {
    let body: NewTagBody = json_body(&headers, body, ctx.limits().json_body).await?;
    let o = organization(
        &ctx,
        OrganizationRequest::CreateTag(TagInput {
            name: body.name,
            color: body.color,
        }),
    )
    .await?;
    match o.item {
        Some(OrganizationItem::Tag(t)) => Ok(created(
            format!("{BASE_PATH}/tags/{}", t.id.0),
            json!({ "tag": t }),
        )),
        _ => Err(ApiError::internal()),
    }
}
async fn delete_tag(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = TagId(uuid(&id, "tag id")?);
    organization(&ctx, OrganizationRequest::DeleteTag(id)).await?;
    Ok(no_content())
}

// ---------------------------------------------------------------- import / export

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FormatName {
    Bibtex,
    Ris,
}
impl From<FormatName> for Format {
    fn from(f: FormatName) -> Self {
        match f {
            FormatName::Bibtex => Format::Bibtex,
            FormatName::Ris => Format::Ris,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationBody {
    #[serde(default)]
    collection: Option<CollectionId>,
    #[serde(default)]
    tags: Vec<TagId>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportBody {
    format: FormatName,
    content: String,
    #[serde(default)]
    start: usize,
    #[serde(default)]
    destination: DestinationBody,
    #[serde(default)]
    dry_run: bool,
}

/// Exact duplicates (visible to this token) and later rows duplicating an earlier row of
/// the batch are skipped; everything else is created. Merges stay a GUI decision.
fn decisions(preview: &ImportPreview) -> Vec<ImportDecision> {
    preview
        .candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let duplicate = c.existing_match.is_some()
                || c.matches.iter().any(|m| m.kind.exact())
                || c.batch_matches.iter().any(|j| *j < i);
            ImportDecision {
                candidate_index: i,
                action: if duplicate {
                    ImportAction::Skip
                } else {
                    ImportAction::Create
                },
            }
        })
        .collect()
}

async fn import(ctx: Ctx, headers: HeaderMap, body: Body) -> Result<Response, ApiError> {
    let body: ImportBody = json_body(&headers, body, ctx.limits().import_body).await?;
    if body.content.len() > ctx.limits().import_text {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "content must be at most {} bytes; split the file",
                ctx.limits().import_text
            ),
        ));
    }
    let result = ctx
        .run(move |c| {
            let parsed = match body.format {
                FormatName::Bibtex => cm_metadata::bibtex::parse(body.content.as_bytes()),
                FormatName::Ris => cm_metadata::ris::parse(body.content.as_bytes()),
            }
            .map_err(ApiError::from)?;
            let total = parsed.candidates.len();
            let diagnostics: Vec<Value> = parsed
                .diagnostics
                .iter()
                .take(MAX_IMPORT_DIAGNOSTICS)
                .map(|d| json!({"line": d.line, "message": d.message, "error": d.error}))
                .collect();
            let mut reply = json!({
                "dry_run": body.dry_run,
                "total_records": total,
                "start": body.start,
                "next_start": Value::Null,
                "diagnostics": diagnostics,
                "diagnostics_omitted": parsed.diagnostics.len().saturating_sub(MAX_IMPORT_DIAGNOSTICS),
                "migration_notice": parsed.migration_notice,
            });
            if total == 0 {
                return Ok(reply);
            }
            if body.start >= total {
                return Err(ApiError::invalid("start is past the last record"));
            }
            let count = MAX_BATCH.min(total - body.start);
            if body.start + count < total {
                reply["next_start"] = json!(body.start + count);
            }
            let destination = ImportDestination {
                collection: body.destination.collection,
                tags: body.destination.tags,
            };
            let request = parsed
                .batch(body.start, count, destination)
                .map_err(ApiError::from)?;
            let preview = match c.call(Command::Import(ImportRequestUi::Preview(request)))? {
                Reply::Import(ImportReply::Preview(p)) => p,
                other => return unexpected(other),
            };
            let cancel = |c: &mut super::ctx::Calls, preview: &ImportPreview| {
                let _ = c.call(Command::Import(ImportRequestUi::Cancel(preview.id.clone())));
            };
            if body.dry_run {
                // Duplicate matches are already redacted by the daemon for tokens without
                // papers:read; only ids and kinds are returned here, never stored records.
                reply["candidates"] = preview
                    .candidates
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        json!({
                            "index": i,
                            "title": c.draft.title,
                            "existing_match": c.existing_match,
                            "matches": c.matches.iter().map(|m| json!({
                                "paper_id": m.existing.id,
                                "kind": to_value(&m.kind),
                            })).collect::<Vec<_>>(),
                            "batch_matches": c.batch_matches,
                            "diagnostics": to_value(&c.diagnostics),
                        })
                    })
                    .collect::<Vec<_>>()
                    .into();
                reply["warnings"] = json!(preview.warnings);
                cancel(c, &preview);
                return Ok(reply);
            }
            let commit = ImportCommit {
                preview_id: preview.id.clone(),
                decisions: decisions(&preview),
            };
            let outcome = match c.call(Command::Import(ImportRequestUi::Commit(commit))) {
                Ok(Reply::Import(ImportReply::Committed(o))) => o,
                Ok(other) => return unexpected(other),
                Err(e) => {
                    cancel(c, &preview);
                    return Err(e);
                }
            };
            reply["created"] = json!(outcome.created);
            reply["merged"] = json!(outcome.merged);
            reply["skipped"] = json!(outcome.skipped);
            reply["rows"] = outcome
                .rows
                .iter()
                .map(|r| {
                    json!({
                        "candidate_index": r.candidate_index,
                        "paper_id": r.paper,
                        "diagnostics": to_value(&r.diagnostics),
                    })
                })
                .collect::<Vec<_>>()
                .into();
            Ok(reply)
        })
        .await?;
    let status = if result["dry_run"] == json!(false) && result.get("created").is_some() {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(result)).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SelectionBody {
    All,
    Ids { ids: Vec<PaperId> },
    Collection { id: CollectionId },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportBody {
    format: FormatName,
    #[serde(default)]
    selection: Option<SelectionBody>,
    #[serde(default)]
    folder: Option<String>,
    file_name: String,
}

/// A single plain file name: no separators, no leading dot, no control characters.
fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_BYTES
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
        && std::path::Path::new(name)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

async fn export(ctx: Ctx, headers: HeaderMap, body: Body) -> Result<Response, ApiError> {
    let body: ExportBody = json_body(&headers, body, ctx.limits().json_body).await?;
    if !plain_file_name(&body.file_name) {
        return Err(ApiError::invalid(
            "file_name must be a plain file name (no path separators or leading dot)",
        ));
    }
    // Only the owner's configured export folders; never a client-chosen path.
    let folders = ctx.app.gateway.api().export_dirs();
    let folder = match &body.folder {
        Some(wanted) => folders
            .iter()
            .find(|f| f.as_os_str() == std::ffi::OsStr::new(wanted))
            .cloned()
            .ok_or_else(|| ApiError::forbidden(Operation::Export))?,
        None => match folders.as_slice() {
            [only] => only.clone(),
            [] => return Err(ApiError::forbidden(Operation::Export)),
            _ => {
                return Err(ApiError::invalid(
                    "folder is required when several export folders are configured",
                ));
            }
        },
    };
    let selection = match body.selection.unwrap_or(SelectionBody::All) {
        SelectionBody::All => ExportSelection::All,
        SelectionBody::Ids { ids } => ExportSelection::Ids(ids),
        SelectionBody::Collection { id } => ExportSelection::Collection(id),
    };
    let request = ExportRequest {
        selection,
        format: body.format.into(),
        path: folder.join(&body.file_name),
    };
    let exported = ctx
        .run(move |c| match c.call(Command::Export(request))? {
            Reply::Exported(e) => Ok(e),
            other => unexpected(other),
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "folder": folder,
            "file_name": body.file_name,
            "records": exported.records,
            "diagnostics": exported.diagnostics,
            "migration_notice": exported.migration_notice,
        })),
    )
        .into_response())
}

// ---------------------------------------------------------------- jobs

fn job_id(text: &str) -> Result<JobId, ApiError> {
    uuid(text, "job id").map(JobId)
}
async fn job(ctx: &Ctx, command: JobCommand) -> Result<Response, ApiError> {
    let job = ctx
        .run(move |c| match c.call(Command::Jobs(command))? {
            Reply::Jobs(JobReply::Job(job)) => Ok(*job),
            other => unexpected(other),
        })
        .await?;
    Ok(ok(json!({ "job": job })))
}
async fn get_job(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = job_id(&id)?;
    job(&ctx, JobCommand::Get(id)).await
}
async fn cancel_job(ctx: Ctx, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = job_id(&id)?;
    job(&ctx, JobCommand::Cancel(id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_file_names_are_plain() {
        for ok in ["refs.bib", "My refs 2026.ris", "a"] {
            assert!(plain_file_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".bib",
            "../a.bib",
            "a/b.bib",
            "a\\b.bib",
            "/etc/passwd",
            "..",
            ".",
            "a\nb",
        ] {
            assert!(!plain_file_name(bad), "{bad:?}");
        }
        assert!(!plain_file_name(&"a".repeat(256)));
    }
}
