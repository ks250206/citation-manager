//! Write tools. Each one is refused by papersd unless this server's credential maps to a
//! separately approved grant (LibraryWrite; export also LocalFileWrite). No tool deletes
//! papers, collections or tags, and none acts on more than one paper per call; bulk
//! delete/move elevation is out of scope (LibraryDelete is never even requested).
use super::{
    Args, Ctx, ToolError,
    fragments::{object, paper_id, uuid},
    read::{annotation_json, attachments, organization, organization_json},
    source::{contributor, get_paper, source, unexpected, uri},
};
use cm_protocol::{
    annotations::{
        Anchor, AnnotationKind, AnnotationReply, AnnotationRequest, COLORS, Edit, NewAnnotation,
        Source,
    },
    attachments::Rect,
    domain::{AttachmentId, CollectionId, PaperId, TagKey},
    exchange::{ExportRequest, ExportSelection, Exported, Format},
    ipc::{Command, Reply},
    library::{LibraryReply, LibraryRequest},
    organization::{CollectionInput, Organization, OrganizationRequest, TagInput},
    search::{FulltextReply, FulltextRequest, PageTextRequest},
};
use serde_json::{Value, json};

// ---------------------------------------------------------------- update_paper

pub const UPDATE_DESCRIPTION: &str = "Edit a paper's bibliographic metadata. Only the fields \
given change; null clears an optional field. authors replaces the whole author list \
(\"Family, Given\" or literal names). expected_updated_at (from get_paper) refuses the edit \
if the record changed meanwhile. Needs the separately approved MCP write grant (the default \
MCP grant is read-only).";

const TEXT_FIELDS: [(&str, usize); 9] = [
    ("journal", 1000),
    ("volume", 64),
    ("issue", 64),
    ("pages", 64),
    ("doi", 512),
    ("url", 2048),
    ("publication_date", 64),
    ("language", 64),
    ("type", 64),
];

pub fn update_schema() -> Value {
    let mut properties = json!({
        "paper_id": paper_id(),
        "title": {"type": "string", "minLength": 1, "maxLength": 2000},
        "year": {"type": ["integer", "null"], "minimum": 1, "maximum": 9999},
        "abstract": {"type": ["string", "null"], "maxLength": 20000},
        "authors": {"type": "array", "maxItems": 200,
            "items": {"type": "string", "minLength": 1, "maxLength": 300}},
        "expected_updated_at": {"type": "string", "minLength": 1, "maxLength": 64},
    });
    for (field, max) in TEXT_FIELDS {
        properties[field] = json!({"type": ["string", "null"], "maxLength": max});
    }
    object(properties, &["paper_id"])
}

pub async fn update_paper(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let id = PaperId(args.uuid("paper_id").expect("validated"));
    let mut changed = Vec::new();
    let mut paper = get_paper(ctx, id).await?;
    if let Some(expected) = args.str("expected_updated_at")
        && paper.updated_at.as_deref() != Some(expected)
    {
        return Err(ToolError::new(
            "conflict",
            "the paper changed since expected_updated_at; re-read it with get_paper",
        )
        .with("updated_at", json!(paper.updated_at)));
    }
    if let Some(title) = args.str("title") {
        paper.title = title.trim().to_owned();
        changed.push("title");
    }
    if args.present("year") {
        paper.bibliography.year = args.i32("year");
        changed.push("year");
    }
    if let Some(value) = args.nullable_str("abstract") {
        paper.bibliography.abstract_text = value;
        changed.push("abstract");
    }
    if args.present("authors") {
        paper.bibliography.authors = args
            .strings("authors")
            .iter()
            .map(|a| contributor(a))
            .collect();
        changed.push("authors");
    }
    let b = &mut paper.bibliography;
    for (field, _) in TEXT_FIELDS {
        if let Some(value) = args.nullable_str(field) {
            let slot = match field {
                "journal" => &mut b.journal,
                "volume" => &mut b.volume,
                "issue" => &mut b.issue,
                "pages" => &mut b.pages,
                "doi" => &mut b.doi,
                "url" => &mut b.url,
                "publication_date" => &mut b.publication_date,
                "language" => &mut b.language,
                _ => &mut b.paper_type,
            };
            *slot = value;
            changed.push(field);
        }
    }
    if changed.is_empty() {
        return Err(ToolError::invalid("give at least one field to change"));
    }
    let updated = match ctx
        .call(Command::Library(LibraryRequest::Update(Box::new(paper))))
        .await?
    {
        Reply::Library(LibraryReply::Paper(p)) => *p,
        _ => return Err(unexpected()),
    };
    Ok(json!({
        "status": "updated",
        "changed_fields": changed,
        "source": source(&updated, None, None, None),
        "updated_at": updated.updated_at,
    }))
}

// ---------------------------------------------------------------- collections and tags

pub const COLLECTION_DESCRIPTION: &str = "Create a collection (optionally inside parent_id). \
If a collection with the same name already exists under the same parent it is returned \
instead (status \"exists\"). Needs the separately approved MCP write grant (the default MCP \
grant is read-only).";

pub fn collection_schema() -> Value {
    object(
        json!({
            "name": {"type": "string", "minLength": 1, "maxLength": 256},
            "parent_id": uuid("Parent collection ID (default: top level)."),
        }),
        &["name"],
    )
}

async fn org(ctx: &Ctx<'_>, request: OrganizationRequest) -> Result<Organization, ToolError> {
    match ctx.call(Command::Organization(request)).await? {
        Reply::Organization(o) => Ok(o),
        _ => Err(unexpected()),
    }
}

fn display_name(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub async fn create_collection(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let name = display_name(args.str("name").expect("validated"));
    let parent = args.uuid("parent_id").map(CollectionId);
    let before = org(ctx, OrganizationRequest::List).await?;
    let describe = |c: &cm_protocol::domain::Collection| json!({"collection_id": c.id.0, "name": c.name, "parent_id": c.parent.map(|p| p.0)});
    if let Some(existing) = before
        .collections
        .iter()
        .find(|c| c.parent == parent && c.name == name)
    {
        return Ok(json!({"status": "exists", "collection": describe(existing)}));
    }
    let after = org(
        ctx,
        OrganizationRequest::CreateCollection(CollectionInput {
            name: name.clone(),
            parent,
            sort_order: 0,
        }),
    )
    .await?;
    let created = after
        .collections
        .iter()
        .find(|c| c.parent == parent && !before.collections.iter().any(|b| b.id == c.id))
        .ok_or_else(unexpected)?;
    Ok(json!({"status": "created", "collection": describe(created)}))
}

pub const MOVE_DESCRIPTION: &str = "Put one paper into a collection. mode=add (default) keeps \
its other collections; mode=move also removes it from every other collection (papers are \
never deleted). Needs the separately approved MCP write grant (the default MCP grant is \
read-only).";

pub fn move_schema() -> Value {
    object(
        json!({
            "paper_id": paper_id(),
            "collection_id": uuid("Target collection ID."),
            "mode": {"type": "string", "enum": ["add", "move"], "default": "add"},
        }),
        &["paper_id", "collection_id"],
    )
}

pub async fn move_paper_to_collection(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let paper_id = PaperId(args.uuid("paper_id").expect("validated"));
    let target = CollectionId(args.uuid("collection_id").expect("validated"));
    let paper = get_paper(ctx, paper_id).await?;
    let before = organization(ctx, paper_id).await?;
    // Add first: a failure part-way never leaves the paper in fewer collections than asked.
    org(
        ctx,
        OrganizationRequest::CollectionMember(paper_id, target, true),
    )
    .await?;
    let mut removed = Vec::new();
    if args.str("mode") == Some("move") {
        for other in before.collections.iter().filter(|c| c.id != target) {
            org(
                ctx,
                OrganizationRequest::CollectionMember(paper_id, other.id, false),
            )
            .await?;
            removed.push(other.id.0);
        }
    }
    let after = organization(ctx, paper_id).await?;
    Ok(json!({
        "status": if removed.is_empty() { "added" } else { "moved" },
        "source": source(&paper, None, None, None),
        "removed_from": removed,
        "collections": organization_json(&after)["collections"],
    }))
}

pub const TAG_DESCRIPTION: &str = "Add tags (by name; missing tags are created) to one \
paper. Tag names are matched case-insensitively after whitespace collapse. Needs the \
separately approved MCP write grant (the default MCP grant is read-only).";

pub fn tag_schema() -> Value {
    object(
        json!({
            "paper_id": paper_id(),
            "tags": {"type": "array", "minItems": 1, "maxItems": 20, "uniqueItems": true,
                "items": {"type": "string", "minLength": 1, "maxLength": 256}},
        }),
        &["paper_id", "tags"],
    )
}

pub async fn add_tag_to_paper(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let paper_id = PaperId(args.uuid("paper_id").expect("validated"));
    let names = args.strings("tags");
    let mut keys = Vec::with_capacity(names.len());
    for name in &names {
        let key =
            TagKey::new(name).map_err(|e| ToolError::invalid(format!("tag {name:?}: {e}")))?;
        keys.push((name.clone(), key));
    }
    let paper = get_paper(ctx, paper_id).await?;
    let mut all = org(ctx, OrganizationRequest::List).await?;
    let mut created = Vec::new();
    for (name, key) in keys {
        let tag = match all.tags.iter().find(|t| t.key == key) {
            Some(tag) => tag.id,
            None => {
                all = org(
                    ctx,
                    OrganizationRequest::CreateTag(TagInput::named(name.clone())),
                )
                .await?;
                created.push(name.clone());
                all.tags
                    .iter()
                    .find(|t| t.key == key)
                    .map(|t| t.id)
                    .ok_or_else(unexpected)?
            }
        };
        org(ctx, OrganizationRequest::TagMember(paper_id, tag, true)).await?;
    }
    let after = organization(ctx, paper_id).await?;
    Ok(json!({
        "status": "tagged",
        "source": source(&paper, None, None, None),
        "created_tags": created,
        "tags": organization_json(&after)["tags"],
    }))
}

// ---------------------------------------------------------------- add_annotation

pub const ANNOTATION_DESCRIPTION: &str = "Annotate a paper's PDF. kind=highlight needs the \
quoted text; kind=note needs a comment. Position it with page_index (0-based physical page) \
and optional rects (PDF points, unrotated page space); a highlight without rects, or an \
annotation without page_index, is stored as an unplaced text anchor (quote and context) for \
the owner to place. Always stored with origin=ai and unconfirmed until the owner confirms it \
in the app. Needs the separately approved MCP write grant (the default MCP grant is \
read-only).";

pub fn annotation_schema() -> Value {
    object(
        json!({
            "paper_id": paper_id(),
            "attachment_id": uuid("PDF attachment (required when the paper has several PDFs)."),
            "kind": {"type": "string", "enum": ["highlight", "note"]},
            "page_index": {"type": "integer", "minimum": 0, "maximum": 100000},
            "page_label": {"type": "string", "minLength": 1, "maxLength": 64},
            "rects": {"type": "array", "minItems": 1, "maxItems": 64,
                "items": {"type": "object", "additionalProperties": false,
                    "required": ["left", "bottom", "right", "top"],
                    "properties": {
                        "left": {"type": "number", "minimum": -100000, "maximum": 100000},
                        "bottom": {"type": "number", "minimum": -100000, "maximum": 100000},
                        "right": {"type": "number", "minimum": -100000, "maximum": 100000},
                        "top": {"type": "number", "minimum": -100000, "maximum": 100000}
                    }}},
            "quote": {"type": "string", "minLength": 1, "maxLength": 4000},
            "context_before": {"type": "string", "minLength": 1, "maxLength": 48},
            "context_after": {"type": "string", "minLength": 1, "maxLength": 48},
            "comment": {"type": "string", "minLength": 1, "maxLength": 8000},
            "color": {"type": "string", "enum": COLORS},
        }),
        &["paper_id", "kind"],
    )
}

pub async fn add_annotation(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let paper_id = PaperId(args.uuid("paper_id").expect("validated"));
    let kind = match args.str("kind") {
        Some("highlight") => AnnotationKind::Highlight,
        _ => AnnotationKind::Note,
    };
    let quote = args.str("quote").unwrap_or_default().trim().to_owned();
    let comment = args
        .str("comment")
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    if kind == AnnotationKind::Highlight && quote.is_empty() {
        return Err(ToolError::invalid(
            "a highlight needs quote (the highlighted text)",
        ));
    }
    if kind == AnnotationKind::Note && comment.is_none() {
        return Err(ToolError::invalid("a note needs comment"));
    }
    let page_index = args.u32("page_index");
    let mut quads = Vec::new();
    for rect in args
        .value("rects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let n = |k: &str| rect[k].as_f64().unwrap_or(f64::NAN) as f32;
        let rect = Rect {
            left: n("left"),
            bottom: n("bottom"),
            right: n("right"),
            top: n("top"),
        };
        if !rect.valid() {
            return Err(ToolError::invalid(
                "each rect needs right > left and top > bottom",
            ));
        }
        quads.push(rect.quad());
    }
    if !quads.is_empty() && page_index.is_none() {
        return Err(ToolError::invalid("rects need page_index"));
    }
    let paper = get_paper(ctx, paper_id).await?;
    let pdfs: Vec<AttachmentId> = attachments(ctx, paper_id)
        .await?
        .into_iter()
        .filter(|(a, _)| a.media_type == "application/pdf")
        .map(|(a, _)| a.id)
        .collect();
    let attachment = match args.uuid("attachment_id").map(AttachmentId) {
        Some(wanted) if pdfs.contains(&wanted) => wanted,
        Some(_) => {
            return Err(ToolError::not_found(
                "attachment_id is not a PDF attachment of this paper",
            ));
        }
        None => match pdfs.as_slice() {
            [only] => *only,
            [] => {
                return Err(ToolError::invalid(
                    "this paper has no PDF attachment to annotate",
                ));
            }
            several => {
                return Err(
                    ToolError::invalid("this paper has several PDFs; give attachment_id").with(
                        "attachment_ids",
                        Value::Array(several.iter().map(|a| json!(a.0)).collect()),
                    ),
                );
            }
        },
    };
    let page_label = match (args.str("page_label"), page_index) {
        (Some(label), _) => Some(label.to_owned()),
        (None, Some(page)) => stored_label(ctx, attachment, page).await,
        (None, None) => None,
    };
    let anchor = match page_index {
        Some(page) if !quads.is_empty() || kind == AnnotationKind::Note => Anchor::Resolved {
            page_index: page,
            page_label: page_label.clone(),
            quads,
        },
        Some(page) => Anchor::Unresolved {
            reason: format!(
                "AI text anchor on page index {page}{}; position not placed yet",
                page_label
                    .as_deref()
                    .map(|l| format!(" (label {l})"))
                    .unwrap_or_default()
            ),
        },
        None => Anchor::Unresolved {
            reason: "AI text anchor without a page; position not placed yet".into(),
        },
    };
    let new = NewAnnotation {
        attachment_id: attachment,
        kind,
        // Provenance is fixed by this server, never chosen by the caller.
        source: Source::Ai,
        anchor,
        quote,
        context_before: args.str("context_before").map(str::to_owned),
        context_after: args.str("context_after").map(str::to_owned),
        color: args.str("color").map(str::to_owned),
        comment,
    };
    let created = match ctx
        .call(Command::Annotations(AnnotationRequest::Create(Box::new(
            new,
        ))))
        .await?
    {
        Reply::Annotations(AnnotationReply::Edit(edit)) => match *edit {
            Edit::Created(annotation) => annotation,
            _ => return Err(unexpected()),
        },
        _ => return Err(unexpected()),
    };
    let mut budget = usize::MAX;
    let (item, _) = annotation_json(&paper, &created, &mut budget);
    Ok(json!({
        "status": "created",
        "annotation": item,
        "note": "Stored with origin=ai and unconfirmed; the library owner can confirm or remove it in the app.",
    }))
}

/// The stored page label of an extracted page, if any (best effort; never guessed).
async fn stored_label(ctx: &Ctx<'_>, attachment: AttachmentId, page: u32) -> Option<String> {
    let reply = ctx
        .call(Command::Fulltext(FulltextRequest::Pages(PageTextRequest {
            attachment,
            from_page: page,
            max_pages: 1,
            max_chars: 1,
        })))
        .await
        .ok()?;
    match reply {
        Reply::Fulltext(FulltextReply::Pages(read)) => read
            .pages
            .into_iter()
            .find(|p| p.page_index == page)
            .and_then(|p| p.page_label),
        _ => None,
    }
}

// ---------------------------------------------------------------- export_citations

pub const EXPORT_DESCRIPTION: &str = "Export papers (by ID, 1-200) as BibTeX or RIS text. \
The text is returned (at most max_chars characters, default 200000); nothing is written \
outside this server's private temporary directory. Needs a separately approved MCP export \
grant (the default MCP grant is read-only).";

pub fn export_schema() -> Value {
    object(
        json!({
            "paper_ids": {"type": "array", "minItems": 1, "maxItems": 200, "uniqueItems": true,
                "items": {"type": "string", "format": "uuid"}},
            "format": {"type": "string", "enum": ["bibtex", "ris"], "default": "bibtex"},
            "max_chars": {"type": "integer", "minimum": 1000, "maximum": 1000000, "default": 200000},
        }),
        &["paper_ids"],
    )
}

pub async fn export_citations(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    let ids: Vec<PaperId> = args.uuids("paper_ids").into_iter().map(PaperId).collect();
    let (format, extension, name) = match args.str("format") {
        Some("ris") => (Format::Ris, "ris", "ris"),
        _ => (Format::Bibtex, "bib", "bibtex"),
    };
    let max_chars = args.u32("max_chars").unwrap_or(200_000) as usize;
    // papersd creates a new file (never overwrites) in a fresh 0700 directory of ours and
    // refuses symlinked paths, so the directory is canonicalized first.
    let dir = tempfile::Builder::new()
        .prefix("papers-mcp-export-")
        .tempdir()
        .map_err(|e| {
            ToolError::new(
                "unavailable",
                format!("cannot create a temporary directory: {e}"),
            )
        })?;
    let path = dir
        .path()
        .canonicalize()
        .map_err(|e| ToolError::new("unavailable", e.to_string()))?
        .join(format!("citations.{extension}"));
    let exported: Exported = match ctx
        .call(Command::Export(ExportRequest {
            selection: ExportSelection::Ids(ids.clone()),
            format,
            path: path.clone(),
        }))
        .await?
    {
        Reply::Exported(e) => e,
        _ => return Err(unexpected()),
    };
    let limit = (max_chars as u64).saturating_mul(4).saturating_add(4);
    let bytes = std::fs::metadata(&path)
        .map_err(|e| ToolError::new("unavailable", format!("export file missing: {e}")))?
        .len();
    if bytes > limit {
        return Err(ToolError::new(
            "too_large",
            format!("the export is {bytes} bytes; request fewer papers or raise max_chars"),
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ToolError::new("unavailable", format!("cannot read the export: {e}")))?;
    drop(dir);
    let (text, truncated) = super::source::clip(&text, max_chars);
    Ok(json!({
        "format": name,
        "records": exported.records,
        "text": text,
        "truncated": truncated,
        "diagnostics": exported.diagnostics,
        "migration_notice": exported.migration_notice,
        "papers": ids.iter().map(|id| json!({"paper_id": id.0, "uri": uri(*id, None, None, None)})).collect::<Vec<_>>(),
    }))
}
