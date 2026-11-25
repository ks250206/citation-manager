//! Read tools: search_papers, get_paper, get_paper_fulltext, get_paper_annotations.
//! All need only the read-only MCP grant (LibraryRead).
use super::{
    Args, Ctx, ToolError,
    fragments::{object, paper_id, uuid},
    source::{Papers, clip, get_paper as read_paper, partial_source, source, unexpected, uri},
};
use cm_protocol::{
    annotations::{Anchor, Annotation, AnnotationKind, AnnotationReply, AnnotationRequest, Source},
    attachments::{AttachmentStatus, PdfReply},
    domain::{AttachmentId, CollectionId, Paper, PaperId},
    ipc::{Command, PdfCommand, Reply},
    organization::{Organization, OrganizationRequest},
    search::{
        AttachmentExtraction, DocumentSource, ExtractionState, FulltextReply, FulltextRequest,
        PageTextRequest, SearchCursor, SearchReply, SearchRequest, SourceSet,
    },
};
use serde_json::{Value, json};

// ---------------------------------------------------------------- search_papers

pub const SEARCH_DESCRIPTION: &str = "Search the local library: bibliographic records, \
annotations and extracted PDF page text. Filter by keyword query, tags, publication year \
range and collection. Each result carries a `source` block (paper_id, doi, title, authors, \
year, attachment_id, page_index, page_label, uri) and the matching `text` snippet; cite the \
source instead of guessing page numbers. Bounded: limit 1-50 results, snippet_chars 32-1000. \
Read-only.";

pub fn search_schema() -> Value {
    object(
        json!({
            "query": {"type": "string", "minLength": 1, "maxLength": 1000,
                "description": "Keywords (all must match). Also accepts the library search \
                    syntax: doi:, tag:, year:YYYY..YYYY, collection:, has:pdf; quote phrases."},
            "tags": {"type": "array", "maxItems": 10, "uniqueItems": true,
                "items": {"type": "string", "minLength": 1, "maxLength": 256},
                "description": "Only papers carrying every one of these tags."},
            "collection": {"type": "string", "minLength": 1, "maxLength": 256,
                "description": "Exact collection name."},
            "collection_id": uuid("Collection ID (alternative to the name)."),
            "year_from": {"type": "integer", "minimum": 1, "maximum": 9999},
            "year_to": {"type": "integer", "minimum": 1, "maximum": 9999},
            "sources": {"type": "array", "minItems": 1, "maxItems": 3, "uniqueItems": true,
                "items": {"type": "string", "enum": ["bibliographic", "annotations", "pages"]},
                "description": "What to search (default: all)."},
            "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 10},
            "snippet_chars": {"type": "integer", "minimum": 32, "maximum": 1000, "default": 300},
            "cursor": {"type": "object", "additionalProperties": false,
                "required": ["scan_after", "offset"],
                "description": "next_cursor from a previous call with the same arguments.",
                "properties": {
                    "scan_after": {"type": "integer", "minimum": 0, "maximum": 9007199254740991i64},
                    "offset": {"type": "integer", "minimum": 0, "maximum": 30000}
                }}
        }),
        &[],
    )
}

/// A DSL string value: always quoted, with the two DSL escapes.
fn quoted(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "\"{}\"",
        collapsed.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

pub(crate) fn search_query(args: &Args) -> Result<String, ToolError> {
    let mut parts = Vec::new();
    if let Some(query) = args.str("query").map(str::trim).filter(|q| !q.is_empty()) {
        parts.push(query.to_owned());
    }
    for tag in args.strings("tags") {
        parts.push(format!("tag:{}", quoted(&tag)));
    }
    if let Some(collection) = args.str("collection") {
        parts.push(format!("collection:{}", quoted(collection)));
    }
    let (from, to) = (args.i32("year_from"), args.i32("year_to"));
    if from.is_some() || to.is_some() {
        let (from, to) = (from.unwrap_or(1), to.unwrap_or(9999));
        if from > to {
            return Err(ToolError::invalid("year_from must not be after year_to"));
        }
        parts.push(format!("year:{from:04}..{to:04}"));
    }
    if parts.is_empty() {
        return Err(ToolError::invalid(
            "give a query or at least one filter (tags, collection, year_from/year_to)",
        ));
    }
    Ok(parts.join(" "))
}

fn source_kind(source: DocumentSource) -> &'static str {
    match source {
        DocumentSource::Paper => "paper",
        DocumentSource::Annotation => "annotation",
        DocumentSource::Page => "page",
    }
}

pub async fn search_papers(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let query = search_query(args)?;
    let mut request = SearchRequest::new(query.clone());
    request.limit = args.u32("limit").unwrap_or(10);
    request.snippet_chars = args.u32("snippet_chars").unwrap_or(300);
    request.collection = args.uuid("collection_id").map(CollectionId);
    if args.has("sources") {
        let sources = args.strings("sources");
        request.sources = SourceSet {
            bibliographic: sources.iter().any(|s| s == "bibliographic"),
            annotations: sources.iter().any(|s| s == "annotations"),
            pages: sources.iter().any(|s| s == "pages"),
        };
    }
    if let Some(cursor) = args.value("cursor") {
        request.cursor = Some(SearchCursor {
            scan_after: cursor["scan_after"].as_f64().unwrap_or(0.0) as i64,
            offset: cursor["offset"].as_f64().unwrap_or(0.0) as u32,
        });
    }
    let page = match ctx.call(Command::Search(Box::new(request))).await? {
        Reply::Search(SearchReply::Results(page)) => page,
        Reply::Search(SearchReply::Invalid(errors)) => {
            let first = errors
                .diagnostics
                .first()
                .map(|d| d.message.clone())
                .unwrap_or_else(|| "invalid query".into());
            return Err(ToolError::invalid(format!("invalid search query: {first}")).with(
                "diagnostics",
                Value::Array(
                    errors
                        .diagnostics
                        .iter()
                        .map(|d| {
                            json!({"code": d.code, "message": d.message,
                                   "start": d.start, "end": d.end,
                                   "corrections": d.corrections.iter()
                                       .map(|c| c.replacement.clone()).collect::<Vec<_>>()})
                        })
                        .collect(),
                ),
            ));
        }
        _ => return Err(unexpected()),
    };
    let mut papers = Papers::default();
    let mut results = Vec::with_capacity(page.hits.len());
    for hit in &page.hits {
        let label = hit.page_label.as_deref();
        let source = match papers.get(ctx, hit.paper_id).await? {
            Some(paper) => source(paper, hit.attachment_id, hit.page_index, label),
            None => partial_source(
                hit.paper_id,
                &hit.paper_title,
                hit.doi.as_deref(),
                hit.attachment_id,
                hit.page_index,
                label,
            ),
        };
        results.push(json!({
            "kind": source_kind(hit.source),
            "source": source,
            "text": hit.snippet,
            "text_truncated": hit.snippet_truncated,
            "matched_fields": hit.matched_fields,
            "annotation_id": hit.annotation_id,
            "score": hit.score,
            "uri": uri(hit.paper_id, hit.attachment_id, hit.page_index, hit.annotation_id.as_deref()),
        }));
    }
    Ok(json!({
        "query": query,
        "results": results,
        "returned": results.len(),
        "total_hits": page.total_hits,
        "next_cursor": page.next_page,
        "coverage": page.coverage,
    }))
}

// ---------------------------------------------------------------- get_paper

pub const GET_PAPER_DESCRIPTION: &str = "Bibliographic record of one paper with its \
attachments (PDF text-extraction state), collections and tags. The `source` block is what \
to cite. Abstract capped at abstract_chars (default 4000, max 20000). Read-only.";

pub fn get_paper_schema() -> Value {
    object(
        json!({
            "paper_id": paper_id(),
            "abstract_chars": {"type": "integer", "minimum": 0, "maximum": 20000, "default": 4000}
        }),
        &["paper_id"],
    )
}

pub(crate) async fn attachments(
    ctx: &Ctx<'_>,
    paper: PaperId,
) -> Result<Vec<(cm_protocol::domain::Attachment, AttachmentStatus)>, ToolError> {
    match ctx.call(Command::Pdf(PdfCommand::List(paper))).await? {
        Reply::Pdf(PdfReply::Attachments(list)) => Ok(list),
        _ => Err(unexpected()),
    }
}

async fn extraction(ctx: &Ctx<'_>, paper: PaperId) -> Result<Vec<AttachmentExtraction>, ToolError> {
    match ctx
        .call(Command::Fulltext(FulltextRequest::Status(paper)))
        .await?
    {
        Reply::Fulltext(FulltextReply::Status(list)) => Ok(list),
        _ => Err(unexpected()),
    }
}

pub(crate) async fn organization(ctx: &Ctx<'_>, paper: PaperId) -> Result<Organization, ToolError> {
    match ctx
        .call(Command::Organization(OrganizationRequest::ForPaper(paper)))
        .await?
    {
        Reply::Organization(o) => Ok(o),
        _ => Err(unexpected()),
    }
}

fn state_name(state: ExtractionState) -> Value {
    serde_json::to_value(state).unwrap_or(Value::Null)
}

fn extraction_json(e: &AttachmentExtraction) -> Value {
    json!({
        "state": state_name(e.state),
        "page_count": e.page_count,
        "pages_done": e.pages_done,
        "pages_with_text": e.pages_with_text,
        "truncated_pages": e.truncated_pages,
        "detail": e.detail,
    })
}

pub(crate) fn organization_json(o: &Organization) -> Value {
    json!({
        "collections": o.collections.iter().map(|c| json!({
            "collection_id": c.id.0, "name": c.name, "parent_id": c.parent.map(|p| p.0)
        })).collect::<Vec<_>>(),
        "tags": o.tags.iter().map(|t| json!({"tag_id": t.id.0, "name": t.name})).collect::<Vec<_>>(),
    })
}

pub fn paper_json(paper: &Paper, abstract_chars: usize) -> Value {
    let b = &paper.bibliography;
    let (abstract_text, abstract_truncated) = match &b.abstract_text {
        Some(text) => {
            let (t, cut) = clip(text, abstract_chars);
            (Some(t), cut)
        }
        None => (None, false),
    };
    json!({
        "paper_id": paper.id.0,
        "title": paper.title,
        "authors": super::source::authors(paper),
        "year": b.year,
        "publication_date": b.publication_date,
        "type": b.paper_type,
        "journal": b.journal,
        "volume": b.volume,
        "issue": b.issue,
        "pages": b.pages,
        "doi": b.doi,
        "url": b.url,
        "language": b.language,
        "citation_key": b.citation_key,
        "external_ids": paper.external_ids.iter()
            .map(|e| json!({"namespace": e.namespace, "value": e.value})).collect::<Vec<_>>(),
        "abstract": abstract_text,
        "abstract_truncated": abstract_truncated,
        "created_at": paper.created_at,
        "updated_at": paper.updated_at,
        "in_trash": paper.deleted_at.is_some(),
    })
}

pub async fn get_paper(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let id = PaperId(args.uuid("paper_id").expect("validated"));
    let paper = read_paper(ctx, id).await?;
    let attachments = attachments(ctx, id).await?;
    let states = extraction(ctx, id).await?;
    let organization = organization(ctx, id).await?;
    let abstract_chars = args.u32("abstract_chars").unwrap_or(4000) as usize;
    let attachments: Vec<Value> = attachments
        .iter()
        .map(|(a, status)| {
            json!({
                "attachment_id": a.id.0,
                "name": a.original_name,
                "media_type": a.media_type,
                "byte_size": a.byte_size,
                "status": match status {
                    AttachmentStatus::Present => "present",
                    AttachmentStatus::Missing => "missing",
                    AttachmentStatus::Corrupt => "corrupt",
                },
                "text_extraction": states.iter().find(|s| s.attachment_id == a.id).map(extraction_json),
                "uri": uri(id, Some(a.id), None, None),
            })
        })
        .collect();
    let org = organization_json(&organization);
    Ok(json!({
        "source": source(&paper, None, None, None),
        "paper": paper_json(&paper, abstract_chars),
        "attachments": attachments,
        "collections": org["collections"],
        "tags": org["tags"],
    }))
}

// ---------------------------------------------------------------- get_paper_fulltext

pub const FULLTEXT_DESCRIPTION: &str = "Extracted text of a paper's PDF, page by page, for a \
bounded page range: from_page_index (0-based physical page) and page_count (1-20, default 5), \
with at most max_chars characters in total (1-100000, default 20000). Each page carries its \
`source` (paper, attachment, page_index, page_label). Continue with next_page_index. Uses the \
paper's first PDF with text unless attachment_id is given. Read-only.";

pub fn fulltext_schema() -> Value {
    object(
        json!({
            "paper_id": paper_id(),
            "attachment_id": uuid("PDF attachment of the paper (default: first PDF with text)."),
            "from_page_index": {"type": "integer", "minimum": 0, "maximum": 100000, "default": 0},
            "page_count": {"type": "integer", "minimum": 1, "maximum": 20, "default": 5},
            "max_chars": {"type": "integer", "minimum": 1, "maximum": 100000, "default": 20000}
        }),
        &["paper_id"],
    )
}

fn pending_note(state: ExtractionState) -> Option<&'static str> {
    match state {
        ExtractionState::Pending | ExtractionState::Extracting => Some(
            "PDF text extraction has not finished for this attachment. The library service \
             extracts text in the background when its PDF runtime is installed; retry later.",
        ),
        ExtractionState::NoText => {
            Some("this PDF has no extractable text (e.g. scanned images; OCR is not available)")
        }
        ExtractionState::Encrypted => Some("this PDF is encrypted; its text is not available"),
        ExtractionState::Malformed | ExtractionState::TooLarge => {
            Some("this PDF could not be read for text extraction")
        }
        _ => None,
    }
}

pub async fn get_paper_fulltext(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let id = PaperId(args.uuid("paper_id").expect("validated"));
    let paper = read_paper(ctx, id).await?;
    let states = extraction(ctx, id).await?;
    let summary: Vec<Value> = states
        .iter()
        .map(|s| json!({"attachment_id": s.attachment_id.0, "state": state_name(s.state), "page_count": s.page_count}))
        .collect();
    let chosen = match args.uuid("attachment_id").map(AttachmentId) {
        Some(wanted) => states
            .iter()
            .find(|s| s.attachment_id == wanted)
            .ok_or_else(|| {
                ToolError::not_found("attachment_id is not a PDF attachment of this paper")
                    .with("attachments", Value::Array(summary.clone()))
            })?,
        None => match states
            .iter()
            .find(|s| s.pages_with_text > 0)
            .or_else(|| states.first())
        {
            Some(state) => state,
            None => {
                return Ok(json!({
                    "source": source(&paper, None, None, None),
                    "pages": [],
                    "next_page_index": null,
                    "attachments": [],
                    "note": "this paper has no PDF attachment",
                }));
            }
        },
    };
    let from = args.u32("from_page_index").unwrap_or(0);
    let count = args.u32("page_count").unwrap_or(5);
    let max_chars = args.u32("max_chars").unwrap_or(20_000);
    let read = match ctx
        .call(Command::Fulltext(FulltextRequest::Pages(PageTextRequest {
            attachment: chosen.attachment_id,
            from_page: from,
            max_pages: count,
            max_chars,
        })))
        .await?
    {
        Reply::Fulltext(FulltextReply::Pages(read)) => read,
        _ => return Err(unexpected()),
    };
    let attachment = Some(read.attachment_id);
    let mut budget = max_chars as usize;
    let mut pages = Vec::new();
    let mut truncated = false;
    for page in read.pages.iter().take(count as usize) {
        let (text, cut) = clip(&page.text, budget);
        budget -= text.chars().count();
        truncated |= cut || page.truncated;
        pages.push(json!({
            "source": source(&paper, attachment, Some(page.page_index), page.page_label.as_deref()),
            "text": text,
            "truncated": cut || page.truncated,
        }));
        if budget == 0 {
            break;
        }
    }
    let mut result = json!({
        "source": source(&paper, attachment, None, None),
        "attachment_id": read.attachment_id.0,
        "text_extraction": extraction_json(&read.extraction),
        "from_page_index": from,
        "pages": pages,
        "next_page_index": read.next_page,
        "truncated": truncated,
        "max_chars": max_chars,
        "attachments": summary,
    });
    if pages_is_empty(&result)
        && let Some(note) = pending_note(read.extraction.state)
    {
        result["note"] = Value::from(note);
    }
    Ok(result)
}

fn pages_is_empty(result: &Value) -> bool {
    result["pages"].as_array().is_none_or(Vec::is_empty)
}

// ---------------------------------------------------------------- get_paper_annotations

pub const ANNOTATIONS_DESCRIPTION: &str = "Highlights and notes on a paper's PDFs, with their \
quoted text, comments, page location (`source`) and origin (human, ai or import; AI \
annotations show whether the owner confirmed them). Bounded: limit 1-500 (default 100) and \
max_chars 1-200000 characters of text in total (default 50000). Read-only.";

pub fn annotations_schema() -> Value {
    object(
        json!({
            "paper_id": paper_id(),
            "attachment_id": uuid("Only annotations on this attachment."),
            "origin": {"type": "string", "enum": ["human", "ai", "import"]},
            "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 100},
            "max_chars": {"type": "integer", "minimum": 1, "maximum": 200000, "default": 50000}
        }),
        &["paper_id"],
    )
}

pub fn origin(source: Source) -> &'static str {
    match source {
        Source::Human => "human",
        Source::Ai => "ai",
        Source::Import => "import",
    }
}

/// One annotation as a cited item; `budget` is the remaining character allowance.
pub fn annotation_json(paper: &Paper, a: &Annotation, budget: &mut usize) -> (Value, bool) {
    let (page_index, page_label, reason) = match &a.anchor {
        Anchor::Resolved {
            page_index,
            page_label,
            ..
        } => (Some(*page_index), page_label.clone(), None),
        Anchor::Unresolved { reason } => (None, None, Some(reason.clone())),
    };
    let (quote, quote_cut) = clip(&a.quote, *budget);
    *budget -= quote.chars().count();
    let (comment, comment_cut) = match &a.comment {
        Some(c) => {
            let (c, cut) = clip(c, *budget);
            *budget -= c.chars().count();
            (Some(c), cut)
        }
        None => (None, false),
    };
    let id = a.id.0.to_string();
    let text = if quote.is_empty() {
        comment.clone().unwrap_or_default()
    } else {
        quote.clone()
    };
    (
        json!({
            "annotation_id": id,
            "kind": match a.kind { AnnotationKind::Highlight => "highlight", AnnotationKind::Note => "note" },
            "origin": origin(a.source),
            "ai_confirmed": (a.source == Source::Ai).then_some(a.confirmed_at.is_some()),
            "confirmed_at": a.confirmed_at,
            "source": source(paper, Some(a.attachment_id), page_index, page_label.as_deref()),
            "text": text,
            "quote": quote,
            "comment": comment,
            "context_before": a.context_before,
            "context_after": a.context_after,
            "color": a.color,
            "anchor": {"resolved": reason.is_none(), "page_index": page_index,
                       "page_label": page_label, "unresolved_reason": reason},
            "truncated": quote_cut || comment_cut,
            "created_at": a.created_at,
            "updated_at": a.updated_at,
            "uri": uri(paper.id, Some(a.attachment_id), page_index, Some(&id)),
        }),
        quote_cut || comment_cut,
    )
}

pub async fn get_paper_annotations(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let id = PaperId(args.uuid("paper_id").expect("validated"));
    let paper = read_paper(ctx, id).await?;
    let list = match ctx
        .call(Command::Annotations(AnnotationRequest::ForPaper(id)))
        .await?
    {
        Reply::Annotations(AnnotationReply::List(list)) => list,
        _ => return Err(unexpected()),
    };
    let attachment = args.uuid("attachment_id").map(AttachmentId);
    let wanted_origin = args.str("origin");
    let matching: Vec<&Annotation> = list
        .iter()
        .filter(|a| attachment.is_none_or(|at| a.attachment_id == at))
        .filter(|a| wanted_origin.is_none_or(|o| origin(a.source) == o))
        .collect();
    let limit = args.u32("limit").unwrap_or(100) as usize;
    let mut budget = args.u32("max_chars").unwrap_or(50_000) as usize;
    let mut items = Vec::new();
    let mut truncated = matching.len() > limit;
    for a in matching.iter().take(limit) {
        if budget == 0 {
            truncated = true;
            break;
        }
        let (item, cut) = annotation_json(&paper, a, &mut budget);
        truncated |= cut;
        items.push(item);
    }
    Ok(json!({
        "source": source(&paper, attachment, None, None),
        "annotations": items,
        "returned": items.len(),
        "total": matching.len(),
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn args(value: Value) -> Args {
        let Value::Object(map) = value else { panic!() };
        Args(map)
    }

    #[test]
    fn filters_compose_into_the_quoted_search_dsl() {
        let a = args(json!({
            "query": "solid state",
            "tags": ["anode", "say \"hi\" \\ there"],
            "collection": "Reading  list",
            "year_from": 2020
        }));
        assert_eq!(
            search_query(&a).unwrap(),
            r#"solid state tag:"anode" tag:"say \"hi\" \\ there" collection:"Reading list" year:2020..9999"#
        );
        let only_years = args(json!({"year_to": 999}));
        assert_eq!(search_query(&only_years).unwrap(), "year:0001..0999");
        assert!(search_query(&args(json!({"year_from": 2025, "year_to": 2020}))).is_err());
        assert!(search_query(&Args(Map::new())).is_err());
        assert!(search_query(&args(json!({"query": "   "}))).is_err());
    }
}
