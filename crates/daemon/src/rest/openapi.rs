//! OpenAPI 3.1 contract of the REST API, generated from the route table and the shared
//! `ENDPOINTS` scope rows. `docs/openapi.json` is this document; a test fails when they
//! differ (regenerate with `CM_UPDATE_OPENAPI=1 cargo test -p cm-daemon openapi`).
use super::{
    BASE_PATH, Limits,
    handlers::{
        DEFAULT_FULLTEXT_CHARS, DEFAULT_FULLTEXT_PAGES, DEFAULT_PAGE_LIMIT, MAX_BULK_IDS,
        MAX_DOI_BYTES, MAX_FILE_NAME_BYTES, MAX_QUERY_BYTES,
    },
};
use cm_core::policy::scopes::{Access, Surface, endpoint};
use serde_json::{Map, Value, json};

struct Param {
    name: &'static str,
    location: &'static str,
    schema: fn() -> Value,
    required: bool,
    description: &'static str,
}
struct Op {
    name: &'static str,
    id: &'static str,
    summary: &'static str,
    params: &'static [Param],
    body: Option<&'static str>,
    ok: u16,
    /// Another success status with the same body (`POST /imports` dry run: 200).
    ok_also: Option<u16>,
    response: Option<&'static str>,
    /// Errors besides the common ones (400/401/403/413/415/503/504).
    extra: &'static [u16],
}

fn s() -> Value {
    json!({"type": "string"})
}
fn ns() -> Value {
    json!({"type": ["string", "null"]})
}
fn int() -> Value {
    json!({"type": "integer"})
}
fn nint() -> Value {
    json!({"type": ["integer", "null"]})
}
fn boolean() -> Value {
    json!({"type": "boolean"})
}
fn num() -> Value {
    json!({"type": "number"})
}
fn id() -> Value {
    json!({"type": "string", "format": "uuid"})
}
fn nid() -> Value {
    json!({"type": ["string", "null"], "format": "uuid"})
}
fn arr(items: Value) -> Value {
    json!({"type": "array", "items": items})
}
fn r(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}
fn any() -> Value {
    json!({})
}
fn obj(required: &[&str], props: Vec<(&str, Value)>) -> Value {
    let props: Map<String, Value> = props.into_iter().map(|(k, v)| (k.into(), v)).collect();
    json!({"type": "object", "required": required, "properties": props})
}
/// Request bodies reject unknown fields.
fn strict(required: &[&str], props: Vec<(&str, Value)>) -> Value {
    let mut o = obj(required, props);
    o["additionalProperties"] = json!(false);
    o
}
fn enumeration(values: &[&str]) -> Value {
    json!({"type": "string", "enum": values})
}

const ID: Param = Param {
    name: "id",
    location: "path",
    schema: id,
    required: true,
    description: "Resource UUID",
};
const LIMIT: Param = Param {
    name: "limit",
    location: "query",
    schema: int,
    required: false,
    description: "Page size: papers 1..=500 (default 50); search 1..=100 (default 20)",
};
const CURSOR: Param = Param {
    name: "cursor",
    location: "query",
    schema: s,
    required: false,
    description: "Opaque next_cursor from the previous page (repeat the same filters)",
};
const VISIBILITY: Param = Param {
    name: "visibility",
    location: "query",
    schema: || enumeration(&["active", "deleted", "all"]),
    required: false,
    description: "Trash filter (default active)",
};
const COLLECTION: Param = Param {
    name: "collection",
    location: "query",
    schema: id,
    required: false,
    description: "Direct members of this collection",
};
macro_rules! q {
    ($name:literal, $schema:expr, $desc:literal) => {
        Param {
            name: $name,
            location: "query",
            schema: $schema,
            required: false,
            description: $desc,
        }
    };
}

const OPS: &[Op] = &[
    Op {
        name: "GET /health",
        id: "health",
        summary: "Service status only (no authentication, no library data)",
        params: &[],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("Health"),
        extra: &[],
    },
    Op {
        name: "GET /papers",
        id: "listPapers",
        summary: "List papers with filters and cursor paging",
        params: &[
            LIMIT,
            CURSOR,
            VISIBILITY,
            COLLECTION,
            q!("tags", s, "Comma-separated tag UUIDs (at most 100)"),
            q!(
                "tag_match",
                || enumeration(&["all", "any"]),
                "Tag combination (default all)"
            ),
            q!(
                "keyword",
                s,
                "Literal substrings over bibliographic fields (no full text or annotations)"
            ),
            q!("author", s, "Literal substring of author names"),
            q!("year_from", int, "Lowest year"),
            q!("year_to", int, "Highest year"),
            q!("type", s, "Exact paper type"),
            q!("has_pdf", boolean, "With/without a PDF attachment"),
            q!(
                "sort",
                || enumeration(&["id", "title", "year", "created_at", "updated_at"]),
                "Sort field (default id)"
            ),
            q!(
                "direction",
                || enumeration(&["asc", "desc"]),
                "Sort direction (default asc)"
            ),
        ],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("PaperPage"),
        extra: &[],
    },
    Op {
        name: "GET /papers/{id}",
        id: "getPaper",
        summary: "One paper with its collections, tags and attachments",
        params: &[ID],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("PaperDetail"),
        extra: &[404],
    },
    Op {
        name: "POST /papers",
        id: "createPaper",
        summary: "Create a paper",
        params: &[],
        body: Some("NewPaper"),
        ok: 201,
        ok_also: None,
        response: Some("PaperEnvelope"),
        extra: &[],
    },
    Op {
        name: "PATCH /papers/{id}",
        id: "updatePaper",
        summary: "Merge-patch bibliographic fields (null clears a field)",
        params: &[ID],
        body: Some("PaperPatch"),
        ok: 200,
        ok_also: None,
        response: Some("PaperWrite"),
        extra: &[404, 409],
    },
    Op {
        name: "DELETE /papers/{id}",
        id: "trashPaper",
        summary: "Move a paper to the trash (soft delete)",
        params: &[ID],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("PaperLifecycle"),
        extra: &[404],
    },
    Op {
        name: "POST /papers/{id}/restore",
        id: "restorePaper",
        summary: "Restore a paper from the trash",
        params: &[ID],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("PaperLifecycle"),
        extra: &[404],
    },
    Op {
        name: "POST /papers/bulk-delete",
        id: "bulkTrashPapers",
        summary: "Trash several papers (needs a time-limited elevation)",
        params: &[],
        body: Some("BulkIds"),
        ok: 200,
        ok_also: None,
        response: Some("BulkResult"),
        extra: &[],
    },
    Op {
        name: "POST /papers/resolve-doi",
        id: "resolveDoi",
        summary: "Resolve a DOI to a draft for preview (nothing is saved)",
        params: &[],
        body: Some("ResolveDoi"),
        ok: 200,
        ok_also: None,
        response: Some("Resolution"),
        extra: &[],
    },
    Op {
        name: "GET /search",
        id: "search",
        summary: "Full-text search over bibliographic records, annotations and PDF pages",
        params: &[
            q!(
                "q",
                s,
                "Search DSL (docs/ADR/search-dsl.md), at most 4096 bytes"
            ),
            LIMIT,
            CURSOR,
            VISIBILITY,
            COLLECTION,
            q!(
                "papers",
                s,
                "Comma-separated paper UUIDs to search within (at most 500)"
            ),
            q!(
                "sources",
                s,
                "Comma-separated subset of bibliographic,annotations,pages"
            ),
            q!(
                "short_term_scope",
                || enumeration(&["bounded", "all_pages"]),
                "Where 1-2 character terms are scanned"
            ),
            q!(
                "candidate_limit",
                int,
                "Documents per short-term scan window, 1..=20000"
            ),
            q!(
                "snippet_chars",
                int,
                "Snippet length in characters, 32..=1000 (default 160)"
            ),
            q!(
                "timeout_ms",
                int,
                "Search time budget, 1..=10000 ms (default 2000)"
            ),
        ],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("SearchResults"),
        extra: &[],
    },
    Op {
        name: "GET /papers/{id}/fulltext/status",
        id: "getFulltextStatus",
        summary: "Text extraction state of the paper's PDF attachments",
        params: &[ID],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("FulltextStatus"),
        extra: &[404],
    },
    Op {
        name: "GET /papers/{id}/fulltext",
        id: "getFulltext",
        summary: "Bounded extracted page text of the paper's PDF",
        params: &[
            ID,
            q!(
                "attachment",
                id,
                "PDF attachment of this paper (default: its first PDF)"
            ),
            q!("from_page", int, "0-based first page (default 0)"),
            q!("max_pages", int, "Pages to return, 1..=50 (default 10)"),
            q!(
                "max_chars",
                int,
                "Characters across pages, 1..=400000 (default 100000)"
            ),
        ],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("PageText"),
        extra: &[404],
    },
    Op {
        name: "GET /attachments/{id}/pages/{page}",
        id: "renderPage",
        summary: "Render one PDF page as RGBA pixels",
        params: &[
            ID,
            Param {
                name: "page",
                location: "path",
                schema: int,
                required: true,
                description: "0-based physical page index",
            },
            q!("zoom", num, "Pixels per PDF point, 0.1..=8 (default 1)"),
        ],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("RenderedPage"),
        extra: &[404, 409, 422],
    },
    Op {
        name: "GET /papers/{id}/annotations",
        id: "listAnnotations",
        summary: "Highlights, comments and notes of the paper",
        params: &[ID],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("AnnotationList"),
        extra: &[404],
    },
    Op {
        name: "POST /attachments/{id}/annotations",
        id: "createAnnotation",
        summary: "Create an annotation on a PDF attachment",
        params: &[ID],
        body: Some("NewAnnotation"),
        ok: 201,
        ok_also: None,
        response: Some("AnnotationEnvelope"),
        extra: &[404],
    },
    Op {
        name: "PATCH /annotations/{id}",
        id: "updateAnnotation",
        summary: "Change an annotation's color or comment",
        params: &[ID],
        body: Some("AnnotationPatch"),
        ok: 200,
        ok_also: None,
        response: Some("AnnotationWrite"),
        extra: &[404],
    },
    Op {
        name: "DELETE /annotations/{id}",
        id: "deleteAnnotation",
        summary: "Delete an annotation",
        params: &[ID],
        body: None,
        ok: 204,
        ok_also: None,
        response: None,
        extra: &[404],
    },
    Op {
        name: "GET /collections",
        id: "listCollections",
        summary: "All collections",
        params: &[],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("CollectionList"),
        extra: &[],
    },
    Op {
        name: "POST /collections",
        id: "createCollection",
        summary: "Create a collection",
        params: &[],
        body: Some("NewCollection"),
        ok: 201,
        ok_also: None,
        response: Some("CollectionEnvelope"),
        extra: &[404],
    },
    Op {
        name: "PATCH /collections/{id}",
        id: "updateCollection",
        summary: "Replace a collection's name, parent and order",
        params: &[ID],
        body: Some("CollectionUpdate"),
        ok: 200,
        ok_also: None,
        response: Some("CollectionEnvelope"),
        extra: &[404],
    },
    Op {
        name: "DELETE /collections/{id}",
        id: "deleteCollection",
        summary: "Delete a collection (children move up; papers stay)",
        params: &[ID],
        body: None,
        ok: 204,
        ok_also: None,
        response: None,
        extra: &[404],
    },
    Op {
        name: "POST /collections/{id}/papers",
        id: "setCollectionMember",
        summary: "Add a paper to (or remove it from) a collection",
        params: &[ID],
        body: Some("Membership"),
        ok: 200,
        ok_also: None,
        response: Some("MembershipResult"),
        extra: &[404],
    },
    Op {
        name: "POST /collections/{id}/papers/bulk-move",
        id: "bulkMoveToCollection",
        summary: "Add/move several papers into a collection (needs a time-limited elevation)",
        params: &[ID],
        body: Some("BulkMove"),
        ok: 200,
        ok_also: None,
        response: Some("BulkResult"),
        extra: &[],
    },
    Op {
        name: "GET /tags",
        id: "listTags",
        summary: "All tags",
        params: &[],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("TagList"),
        extra: &[],
    },
    Op {
        name: "POST /tags",
        id: "createTag",
        summary: "Create a tag",
        params: &[],
        body: Some("NewTag"),
        ok: 201,
        ok_also: None,
        response: Some("TagEnvelope"),
        extra: &[409],
    },
    Op {
        name: "DELETE /tags/{id}",
        id: "deleteTag",
        summary: "Delete a tag (papers stay)",
        params: &[ID],
        body: None,
        ok: 204,
        ok_also: None,
        response: None,
        extra: &[404],
    },
    Op {
        name: "POST /imports",
        id: "importBibliography",
        summary: "Import BibTeX/RIS text (up to 100 records per request; dry_run previews)",
        params: &[],
        body: Some("ImportRequest"),
        ok: 201,
        ok_also: Some(200),
        response: Some("ImportResult"),
        extra: &[409],
    },
    Op {
        name: "POST /exports",
        id: "exportBibliography",
        summary: "Write BibTeX/RIS into an allowed export folder",
        params: &[],
        body: Some("ExportRequest"),
        ok: 201,
        ok_also: None,
        response: Some("ExportResult"),
        extra: &[404, 409],
    },
    Op {
        name: "GET /jobs/{id}",
        id: "getJob",
        summary: "Status of one of this token's jobs",
        params: &[ID],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("JobEnvelope"),
        extra: &[404],
    },
    Op {
        name: "POST /jobs/{id}/cancel",
        id: "cancelJob",
        summary: "Cancel one of this token's jobs",
        params: &[ID],
        body: None,
        ok: 200,
        ok_also: None,
        response: Some("JobEnvelope"),
        extra: &[404],
    },
];

/// `ENDPOINTS` names documented here (contract tests compare with the router).
pub fn operation_names() -> Vec<&'static str> {
    OPS.iter().map(|o| o.name).collect()
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No content",
        400 => "Invalid input, JSON, query or Host header (code says which)",
        401 => "Missing, unknown, revoked or expired token",
        403 => "Scope missing, browser origin refused, elevation or export folder required",
        404 => "Not found",
        409 => "Conflict with existing data or state",
        413 => "Body or result too large",
        415 => "Body is not application/json",
        422 => "PDF cannot be read",
        503 => "Busy or unavailable (Retry-After)",
        504 => "Request time limit reached",
        _ => "Error",
    }
}

fn operation(op: &Op) -> Value {
    let row = endpoint(Surface::Rest, op.name).expect("documented endpoint is in ENDPOINTS");
    let mut params: Vec<Value> = op
        .params
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "in": p.location,
                "required": p.required || p.location == "path",
                "description": p.description,
                "schema": (p.schema)(),
            })
        })
        .collect();
    if op.name == "GET /search" {
        params[0]["required"] = json!(true);
    }
    let mut responses = Map::new();
    let ok = match op.response {
        Some(schema) => json!({
            "description": status_text(op.ok),
            "content": {"application/json": {"schema": r(schema)}},
        }),
        None => json!({"description": status_text(op.ok)}),
    };
    if let Some(also) = op.ok_also {
        let mut alt = ok.clone();
        alt["description"] = json!(status_text(also));
        responses.insert(also.to_string(), alt);
    }
    responses.insert(op.ok.to_string(), ok);
    let mut errors: Vec<u16> = vec![400, 503, 504];
    let scopes: Vec<&str> = match row.access {
        Access::Scoped(required) => {
            errors.extend([401, 403]);
            required.iter().map(|s| s.name()).collect()
        }
        _ => {
            errors.push(403);
            vec![]
        }
    };
    if op.body.is_some() {
        errors.extend([413, 415]);
    }
    errors.extend(op.extra);
    errors.sort_unstable();
    errors.dedup();
    for code in errors {
        responses.insert(
            code.to_string(),
            json!({
                "description": status_text(code),
                "content": {"application/json": {"schema": r("Error")}},
            }),
        );
    }
    let mut operation = json!({
        "operationId": op.id,
        "summary": op.summary,
        "parameters": params,
        "responses": responses,
        "x-required-scopes": scopes,
        "x-daemon-operations": row.operations.iter().map(|o| format!("{o:?}")).collect::<Vec<_>>(),
    });
    match row.access {
        Access::Public => operation["security"] = json!([]),
        _ => operation["security"] = json!([{"bearer": scopes}]),
    }
    if let Some(body) = op.body {
        operation["requestBody"] = json!({
            "required": true,
            "content": {"application/json": {"schema": r(body)}},
        });
    }
    operation
}

fn paths() -> Value {
    let mut paths = Map::new();
    for op in OPS {
        let (method, path) = op.name.split_once(' ').expect("METHOD /path");
        let entry = paths.entry(path.to_owned()).or_insert_with(|| json!({}));
        entry[method.to_ascii_lowercase()] = operation(op);
    }
    Value::Object(paths)
}

fn paper_fields(nullable: bool) -> Vec<(&'static str, Value)> {
    let text = if nullable { ns } else { s };
    vec![
        ("title", s()),
        ("external_ids", arr(r("ExternalId"))),
        ("type", text()),
        ("abstract", text()),
        ("year", if nullable { nint() } else { int() }),
        ("publication_date", text()),
        ("journal", text()),
        ("volume", text()),
        ("issue", text()),
        ("pages", text()),
        ("doi", text()),
        ("url", text()),
        ("language", text()),
        ("citation_key", text()),
        ("authors", arr(r("Contributor"))),
        ("extra_json", any()),
    ]
}

fn schemas(limits: &Limits) -> Value {
    let mut paper = paper_fields(false);
    paper.extend([
        ("id", id()),
        ("created_at", s()),
        ("updated_at", s()),
        ("deleted_at", s()),
    ]);
    let bulk_ids = json!({"type": "array", "items": id(), "minItems": 1, "maxItems": MAX_BULK_IDS, "uniqueItems": true});
    let anchor = json!({"oneOf": [
        strict(&["state", "page_index"], vec![
            ("state", json!({"const": "resolved"})),
            ("page_index", int()),
            ("page_label", ns()),
            ("quads", arr(arr(obj(&["x", "y"], vec![("x", num()), ("y", num())])))),
        ]),
        strict(&["state", "reason"], vec![
            ("state", json!({"const": "unresolved"})),
            ("reason", s()),
        ]),
    ]});
    let annotation = obj(
        &[
            "id",
            "attachment_id",
            "kind",
            "source",
            "anchor",
            "quote",
            "created_at",
            "updated_at",
        ],
        vec![
            ("id", id()),
            ("attachment_id", id()),
            ("kind", enumeration(&["highlight", "note"])),
            ("source", enumeration(&["human", "ai", "import"])),
            ("anchor", r("Anchor")),
            ("quote", s()),
            ("context_before", ns()),
            ("context_after", ns()),
            ("color", ns()),
            ("comment", ns()),
            ("confirmed_at", ns()),
            ("created_at", s()),
            ("updated_at", s()),
        ],
    );
    let extraction = obj(
        &[
            "attachment_id",
            "paper_id",
            "state",
            "page_count",
            "pages_done",
        ],
        vec![
            ("attachment_id", id()),
            ("paper_id", id()),
            ("state", s()),
            ("page_count", int()),
            ("pages_done", int()),
            ("pages_with_text", int()),
            ("truncated_pages", int()),
            ("detail", ns()),
            ("updated_at", ns()),
        ],
    );
    let hit = obj(
        &[
            "paper_id",
            "title",
            "source",
            "snippet",
            "score",
            "deleted",
            "highlights",
            "matched_fields",
        ],
        vec![
            ("paper_id", id()),
            ("title", s()),
            ("doi", ns()),
            ("deleted", boolean()),
            ("source", enumeration(&["paper", "annotation", "page"])),
            ("attachment_id", nid()),
            ("annotation_id", ns()),
            ("page_index", nint()),
            ("page_label", ns()),
            ("matched_fields", arr(s())),
            ("snippet_field", ns()),
            ("snippet", s()),
            (
                "highlights",
                arr(obj(
                    &["start", "end"],
                    vec![("start", int()), ("end", int())],
                )),
            ),
            ("snippet_truncated", boolean()),
            ("score", num()),
        ],
    );
    let coverage = obj(
        &["complete", "timed_out", "scan", "unsearched"],
        vec![
            ("complete", boolean()),
            ("fts_truncated", boolean()),
            ("timed_out", boolean()),
            ("short_terms", arr(s())),
            ("scan", enumeration(&["not_needed", "complete", "partial"])),
            ("scanned_documents", int()),
            ("unsearched", arr(any())),
            ("continuation", ns()),
            ("unresolved", arr(any())),
        ],
    );
    let job = obj(
        &[
            "id",
            "kind",
            "state",
            "attempts",
            "max_attempts",
            "created_at_ms",
            "updated_at_ms",
        ],
        vec![
            ("id", id()),
            ("kind", s()),
            ("idempotency_key", s()),
            (
                "state",
                enumeration(&[
                    "queued",
                    "running",
                    "retry_wait",
                    "succeeded",
                    "failed",
                    "cancelled",
                ]),
            ),
            ("attempts", int()),
            ("max_attempts", int()),
            (
                "progress",
                obj(
                    &["done"],
                    vec![("done", int()), ("total", nint()), ("message", ns())],
                ),
            ),
            ("error", ns()),
            ("result", any()),
            ("detected_committed", boolean()),
            ("cancel_requested", boolean()),
            ("next_attempt_at_ms", nint()),
            ("deadline_at_ms", nint()),
            ("created_at_ms", int()),
            ("updated_at_ms", int()),
        ],
    );
    let format = enumeration(&["bibtex", "ris"]);
    let mut new_paper = strict(&["title"], paper_fields(true));
    new_paper["properties"]["title"] = json!({"type": "string", "minLength": 1});
    let entries: Vec<(&str, Value)> = vec![
        (
            "Error",
            obj(
                &["error"],
                vec![(
                    "error",
                    obj(
                        &["code", "message", "request_id"],
                        vec![
                            ("code", s()),
                            ("message", s()),
                            ("details", any()),
                            ("request_id", s()),
                        ],
                    ),
                )],
            ),
        ),
        (
            "Health",
            obj(&["status"], vec![("status", json!({"const": "ok"}))]),
        ),
        (
            "ExternalId",
            obj(
                &["namespace", "value"],
                vec![("namespace", s()), ("value", s())],
            ),
        ),
        (
            "Contributor",
            obj(
                &["role"],
                vec![
                    ("given_name", ns()),
                    ("family_name", ns()),
                    ("literal_name", ns()),
                    ("orcid", ns()),
                    ("role", s()),
                ],
            ),
        ),
        ("Paper", obj(&["id", "title", "external_ids"], paper)),
        ("NewPaper", new_paper),
        ("PaperPatch", strict(&[], paper_fields(true))),
        (
            "PaperPage",
            obj(
                &["papers", "next_cursor"],
                vec![("papers", arr(r("Paper"))), ("next_cursor", ns())],
            ),
        ),
        (
            "PaperEnvelope",
            obj(&["paper"], vec![("paper", r("Paper"))]),
        ),
        (
            "PaperWrite",
            obj(
                &["id"],
                vec![
                    ("id", id()),
                    ("updated_at", ns()),
                    (
                        "paper",
                        json!({"$ref": "#/components/schemas/Paper", "description": "Only for tokens that also hold papers:read"}),
                    ),
                ],
            ),
        ),
        (
            "PaperLifecycle",
            obj(
                &["id", "deleted"],
                vec![("id", id()), ("deleted", boolean())],
            ),
        ),
        (
            "Collection",
            obj(
                &["id", "name", "sort_order"],
                vec![
                    ("id", id()),
                    ("parent", nid()),
                    ("name", s()),
                    ("sort_order", int()),
                ],
            ),
        ),
        (
            "Tag",
            obj(
                &["id", "name", "key"],
                vec![("id", id()), ("name", s()), ("key", s()), ("color", ns())],
            ),
        ),
        (
            "AttachmentSummary",
            obj(
                &[
                    "id",
                    "original_name",
                    "media_type",
                    "byte_size",
                    "sha256",
                    "status",
                ],
                vec![
                    ("id", id()),
                    ("original_name", s()),
                    ("media_type", s()),
                    ("byte_size", int()),
                    ("sha256", s()),
                    ("status", enumeration(&["Present", "Missing", "Corrupt"])),
                ],
            ),
        ),
        (
            "PaperDetail",
            obj(
                &["paper", "collections", "tags", "attachments"],
                vec![
                    ("paper", r("Paper")),
                    ("collections", arr(r("Collection"))),
                    ("tags", arr(r("Tag"))),
                    ("attachments", arr(r("AttachmentSummary"))),
                ],
            ),
        ),
        (
            "BulkIds",
            strict(&["paper_ids"], vec![("paper_ids", bulk_ids.clone())]),
        ),
        (
            "BulkMove",
            strict(
                &["paper_ids"],
                vec![("paper_ids", bulk_ids), ("from", nid())],
            ),
        ),
        (
            "BulkResult",
            obj(
                &["results"],
                vec![(
                    "results",
                    arr(obj(
                        &["id", "ok"],
                        vec![("id", id()), ("ok", boolean()), ("error", s())],
                    )),
                )],
            ),
        ),
        (
            "ResolveDoi",
            strict(
                &["doi"],
                vec![(
                    "doi",
                    json!({"type": "string", "minLength": 1, "maxLength": MAX_DOI_BYTES}),
                )],
            ),
        ),
        (
            "Resolution",
            obj(
                &["resolution"],
                vec![(
                    "resolution",
                    obj(
                        &["canonical_doi", "draft", "manual_add"],
                        vec![
                            ("canonical_doi", s()),
                            ("draft", any()),
                            ("manual_add", boolean()),
                            ("outcomes", arr(any())),
                            ("fields", arr(any())),
                            ("pdf_candidates", arr(any())),
                        ],
                    ),
                )],
            ),
        ),
        ("SearchHit", hit),
        ("SearchCoverage", coverage),
        (
            "SearchResults",
            obj(
                &["results", "next_cursor", "total_hits", "coverage"],
                vec![
                    ("results", arr(r("SearchHit"))),
                    ("next_cursor", ns()),
                    ("total_hits", int()),
                    ("coverage", r("SearchCoverage")),
                ],
            ),
        ),
        ("AttachmentExtraction", extraction),
        (
            "FulltextStatus",
            obj(
                &["attachments"],
                vec![("attachments", arr(r("AttachmentExtraction")))],
            ),
        ),
        (
            "PageText",
            obj(
                &[
                    "paper_id",
                    "attachment_id",
                    "extraction",
                    "pages",
                    "next_page",
                ],
                vec![
                    ("paper_id", id()),
                    ("attachment_id", id()),
                    ("extraction", r("AttachmentExtraction")),
                    (
                        "pages",
                        arr(obj(
                            &["page_index", "text", "truncated"],
                            vec![
                                ("page_index", int()),
                                ("page_label", ns()),
                                ("text", s()),
                                ("truncated", boolean()),
                            ],
                        )),
                    ),
                    ("next_page", nint()),
                ],
            ),
        ),
        (
            "RenderedPage",
            obj(
                &[
                    "attachment_id",
                    "page",
                    "page_count",
                    "width",
                    "height",
                    "format",
                    "data",
                ],
                vec![
                    ("attachment_id", id()),
                    ("page", int()),
                    ("page_count", int()),
                    ("label", ns()),
                    ("rotation", s()),
                    ("zoom", num()),
                    ("width", int()),
                    ("height", int()),
                    ("has_text", boolean()),
                    ("format", json!({"const": "rgba8"})),
                    (
                        "data",
                        json!({"type": "string", "contentEncoding": "base64"}),
                    ),
                ],
            ),
        ),
        ("Anchor", anchor),
        ("Annotation", annotation),
        (
            "AnnotationList",
            obj(
                &["annotations"],
                vec![("annotations", arr(r("Annotation")))],
            ),
        ),
        (
            "AnnotationEnvelope",
            obj(&["annotation"], vec![("annotation", r("Annotation"))]),
        ),
        (
            "NewAnnotation",
            strict(
                &["kind", "anchor", "quote"],
                vec![
                    ("kind", enumeration(&["highlight", "note"])),
                    ("source", enumeration(&["ai", "human"])),
                    ("anchor", r("Anchor")),
                    ("quote", s()),
                    ("context_before", ns()),
                    ("context_after", ns()),
                    ("color", ns()),
                    ("comment", ns()),
                ],
            ),
        ),
        (
            "AnnotationPatch",
            strict(&[], vec![("color", ns()), ("comment", ns())]),
        ),
        (
            "AnnotationWrite",
            obj(
                &["id", "updated_at"],
                vec![
                    ("id", id()),
                    ("updated_at", s()),
                    (
                        "annotation",
                        json!({"$ref": "#/components/schemas/Annotation", "description": "Only for tokens that also hold annotations:read"}),
                    ),
                ],
            ),
        ),
        (
            "CollectionList",
            obj(
                &["collections"],
                vec![("collections", arr(r("Collection")))],
            ),
        ),
        ("TagList", obj(&["tags"], vec![("tags", arr(r("Tag")))])),
        (
            "NewCollection",
            strict(
                &["name"],
                vec![("name", s()), ("parent", nid()), ("sort_order", int())],
            ),
        ),
        (
            "CollectionUpdate",
            strict(
                &["name", "parent", "sort_order"],
                vec![("name", s()), ("parent", nid()), ("sort_order", int())],
            ),
        ),
        (
            "CollectionEnvelope",
            obj(&["collection"], vec![("collection", r("Collection"))]),
        ),
        (
            "Membership",
            strict(
                &["paper_id"],
                vec![("paper_id", id()), ("member", boolean())],
            ),
        ),
        (
            "MembershipResult",
            obj(
                &["collection_id", "paper_id", "member"],
                vec![
                    ("collection_id", id()),
                    ("paper_id", id()),
                    ("member", boolean()),
                ],
            ),
        ),
        (
            "NewTag",
            strict(&["name"], vec![("name", s()), ("color", ns())]),
        ),
        ("TagEnvelope", obj(&["tag"], vec![("tag", r("Tag"))])),
        (
            "ImportRequest",
            strict(
                &["format", "content"],
                vec![
                    ("format", format.clone()),
                    (
                        "content",
                        json!({"type": "string", "maxLength": limits.import_text, "description": "BibTeX/RIS text (at most this many bytes)"}),
                    ),
                    (
                        "start",
                        json!({"type": "integer", "minimum": 0, "description": "First record of this batch (see next_start)"}),
                    ),
                    (
                        "destination",
                        strict(&[], vec![("collection", nid()), ("tags", arr(id()))]),
                    ),
                    ("dry_run", boolean()),
                ],
            ),
        ),
        (
            "ImportResult",
            obj(
                &[
                    "dry_run",
                    "total_records",
                    "start",
                    "next_start",
                    "diagnostics",
                ],
                vec![
                    ("dry_run", boolean()),
                    ("total_records", int()),
                    ("start", int()),
                    ("next_start", nint()),
                    (
                        "diagnostics",
                        arr(obj(
                            &["line", "message", "error"],
                            vec![("line", int()), ("message", s()), ("error", boolean())],
                        )),
                    ),
                    ("diagnostics_omitted", int()),
                    ("migration_notice", s()),
                    ("candidates", arr(any())),
                    ("warnings", arr(s())),
                    ("created", arr(id())),
                    ("merged", arr(id())),
                    ("skipped", int()),
                    ("rows", arr(any())),
                ],
            ),
        ),
        (
            "ExportRequest",
            strict(
                &["format", "file_name"],
                vec![
                    ("format", format),
                    (
                        "selection",
                        json!({"oneOf": [
                            strict(&["kind"], vec![("kind", json!({"const": "all"}))]),
                            strict(&["kind", "ids"], vec![("kind", json!({"const": "ids"})), ("ids", arr(id()))]),
                            strict(&["kind", "id"], vec![("kind", json!({"const": "collection"})), ("id", id())]),
                        ]}),
                    ),
                    (
                        "folder",
                        json!({"type": "string", "description": "One of the export folders configured by the owner (optional when exactly one exists)"}),
                    ),
                    (
                        "file_name",
                        json!({"type": "string", "minLength": 1, "maxLength": MAX_FILE_NAME_BYTES, "description": "Plain file name: no path separators, no leading dot"}),
                    ),
                ],
            ),
        ),
        (
            "ExportResult",
            obj(
                &["folder", "file_name", "records", "diagnostics"],
                vec![
                    ("folder", s()),
                    ("file_name", s()),
                    ("records", int()),
                    ("diagnostics", arr(s())),
                    ("migration_notice", s()),
                ],
            ),
        ),
        ("JobStatus", job),
        ("JobEnvelope", obj(&["job"], vec![("job", r("JobStatus"))])),
    ];
    Value::Object(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )
}

pub fn document() -> Value {
    let limits = Limits::default();
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "citation-manager local REST API",
            "version": "1",
            "description": format!(
                "Local-only HTTP API served by papersd (DESIGN §13, docs/ADR/rest-api.md). \
                 Disabled by default; binds 127.0.0.1 or ::1 only. Every endpoint except \
                 /health needs `Authorization: Bearer <token>` with the scopes listed in \
                 x-required-scopes (the same daemon decision as IPC, CLI and MCP). Requests \
                 with a Host other than 127.0.0.1/localhost/[::1] plus the API port, or with \
                 a browser Origin that is not configured, are refused. Limits: JSON bodies \
                 {} bytes, import bodies {} bytes, URI {} bytes, {} s per request ({} s for \
                 imports, exports, DOI lookups and bulk operations), {} concurrent requests. \
                 Errors: {{\"error\":{{\"code\",\"message\",\"details\",\"request_id\"}}}}. \
                 Paging: papers default limit {}, full text default {} pages / {} characters, \
                 search query at most {} bytes.",
                limits.json_body,
                limits.import_body,
                limits.max_uri,
                limits.request_timeout.as_secs(),
                limits.long_timeout.as_secs(),
                limits.max_in_flight,
                DEFAULT_PAGE_LIMIT,
                DEFAULT_FULLTEXT_PAGES,
                DEFAULT_FULLTEXT_CHARS,
                MAX_QUERY_BYTES,
            ),
        },
        "servers": [{
            "url": format!("http://127.0.0.1:{{port}}{BASE_PATH}"),
            "variables": {"port": {"default": cm_core::policy::api::DEFAULT_PORT.to_string()}},
        }],
        "security": [{"bearer": []}],
        "paths": paths(),
        "components": {
            "securitySchemes": {
                "bearer": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "API token issued in the desktop app (API & integrations) or with `papersd token issue`",
                },
            },
            "schemas": schemas(&limits),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_core::policy::scopes::ENDPOINTS;
    use std::collections::BTreeSet;

    fn committed_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/openapi.json")
    }

    /// docs/openapi.json is exactly the generated document.
    #[test]
    fn openapi_document_is_committed() {
        let generated = serde_json::to_string_pretty(&document()).unwrap() + "\n";
        if std::env::var_os("CM_UPDATE_OPENAPI").is_some() {
            std::fs::write(committed_path(), &generated).unwrap();
        }
        let committed = std::fs::read_to_string(committed_path()).unwrap_or_default();
        assert!(
            committed == generated,
            "docs/openapi.json is stale; run CM_UPDATE_OPENAPI=1 cargo test -p cm-daemon openapi"
        );
    }

    /// Router routes == OpenAPI operations == ENDPOINTS REST rows (in both directions).
    #[test]
    fn router_openapi_and_scope_table_list_the_same_endpoints() {
        let router: BTreeSet<&str> = crate::rest::handlers::routes()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let documented: BTreeSet<&str> = operation_names().into_iter().collect();
        let table: BTreeSet<&str> = ENDPOINTS
            .iter()
            .filter(|e| e.surface == Surface::Rest)
            .map(|e| e.name)
            .collect();
        assert_eq!(router, table, "router vs ENDPOINTS");
        assert_eq!(documented, table, "openapi vs ENDPOINTS");
        let doc = document();
        let mut in_paths = BTreeSet::new();
        for (path, item) in doc["paths"].as_object().unwrap() {
            for method in item.as_object().unwrap().keys() {
                in_paths.insert(format!("{} {path}", method.to_ascii_uppercase()));
            }
        }
        let table: BTreeSet<String> = table.into_iter().map(str::to_owned).collect();
        assert_eq!(in_paths, table);
    }

    /// Every operation documents exactly its ENDPOINTS scopes, and every non-public one
    /// documents 401 and 403.
    #[test]
    fn documented_scopes_match_the_shared_table() {
        let doc = document();
        for row in ENDPOINTS.iter().filter(|e| e.surface == Surface::Rest) {
            let (method, path) = row.name.split_once(' ').unwrap();
            let op = &doc["paths"][path][method.to_ascii_lowercase()];
            let scopes: Vec<String> = op["x-required-scopes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
            match row.access {
                Access::Scoped(required) => {
                    let expected: Vec<String> =
                        required.iter().map(|s| s.name().to_owned()).collect();
                    assert_eq!(scopes, expected, "{}", row.name);
                    for code in ["401", "403"] {
                        assert!(op["responses"].get(code).is_some(), "{} {code}", row.name);
                    }
                }
                Access::Public => {
                    assert!(scopes.is_empty());
                    assert_eq!(op["security"], json!([]));
                }
                Access::OwnerOnly => panic!("owner-only rows are not served over HTTP"),
            }
        }
    }

    /// Every `$ref` resolves.
    #[test]
    fn schema_references_resolve() {
        let doc = document();
        fn walk(v: &Value, doc: &Value) {
            match v {
                Value::Object(m) => {
                    if let Some(Value::String(target)) = m.get("$ref") {
                        let name = target.strip_prefix("#/components/schemas/").unwrap();
                        assert!(doc["components"]["schemas"].get(name).is_some(), "{target}");
                    }
                    m.values().for_each(|v| walk(v, doc));
                }
                Value::Array(a) => a.iter().for_each(|v| walk(v, doc)),
                _ => {}
            }
        }
        walk(&doc, &doc);
    }
}
