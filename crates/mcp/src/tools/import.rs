//! add_paper_by_doi / add_paper_from_url through the common #33 import contract: a draft is
//! previewed (normalization, duplicate detection, differences) and only an explicit commit
//! writes. With `confirm: true` a preview without any possible duplicate is committed as a
//! new paper in the same call; otherwise the preview is returned and the caller confirms
//! with `preview_id` + `decision` (create, merge or skip). Previews are bound to this
//! server's grant by papersd and expire when the library changes.
use super::{
    Args, Ctx, ToolError,
    fragments::{object, uuid},
    source::{author, contributor, get_paper, source, unexpected, uri},
};
use cm_protocol::{
    domain::{CollectionId, PaperId},
    exchange::{ImportReply, ImportRequestUi, MetadataResolution},
    import::{
        ImportAction, ImportCommit, ImportDecision, ImportDestination, ImportOutcome,
        ImportPreview, ImportRequest, ImportSource, MatchKind, PaperDraft, PreviewId,
    },
    ipc::{Command, Reply},
};
use serde_json::{Value, json};

pub const DOI_DESCRIPTION: &str = "Add a paper by DOI: resolves metadata from DOI providers \
(Crossref, DataCite), previews it against the library (duplicates by DOI, identifiers and \
similar title/author/year), and adds it only when confirmed. Step 1: call with doi \
(confirm=true adds it immediately when no possible duplicate exists). Step 2 (when a preview \
is returned): call with preview_id and decision create|merge|skip. title is used only when \
the providers return none. Needs the separately approved MCP write grant (the default MCP \
grant is read-only).";

pub const URL_DESCRIPTION: &str = "Add a paper from a web page. This server does not fetch \
pages itself: a DOI in the URL (or the doi argument) is resolved through DOI providers; \
otherwise give the metadata you read from the page (title, authors, year, ...). Same preview \
and confirmation contract as add_paper_by_doi (confirm, preview_id, decision). Needs the \
separately approved MCP write grant (the default MCP grant is read-only).";

fn decision_properties() -> Value {
    json!({
        "confirm": {"type": "boolean", "default": false,
            "description": "Add immediately when the preview finds no possible duplicate."},
        "preview_id": {"type": "string", "minLength": 1, "maxLength": 256,
            "description": "preview_id from a previous call, to commit a decision."},
        "decision": {"type": "string", "enum": ["create", "merge", "skip"],
            "description": "With preview_id: add as new, merge into a duplicate, or skip."},
        "match_index": {"type": "integer", "minimum": 0, "maximum": 99, "default": 0,
            "description": "With decision=merge: which duplicate (duplicates[].match_index)."},
        "merge_fields": {"type": "array", "minItems": 1, "maxItems": 32, "uniqueItems": true,
            "items": {"type": "string", "minLength": 1, "maxLength": 64},
            "description": "With decision=merge: fields to take from the new record (from duplicates[].differences)."},
        "collection_id": uuid("Also add the new paper to this collection."),
    })
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

pub fn doi_schema() -> Value {
    object(
        merge(
            json!({
                "doi": {"type": "string", "minLength": 3, "maxLength": 512,
                    "description": "DOI or DOI URL, e.g. 10.1234/abc or https://doi.org/10.1234/abc."},
                "title": {"type": "string", "minLength": 1, "maxLength": 2000,
                    "description": "Fallback title when providers return none."},
            }),
            decision_properties(),
        ),
        &[],
    )
}

pub fn url_schema() -> Value {
    object(
        merge(
            json!({
                "url": {"type": "string", "format": "uri", "maxLength": 2048},
                "doi": {"type": "string", "minLength": 3, "maxLength": 512},
                "title": {"type": "string", "minLength": 1, "maxLength": 2000},
                "authors": {"type": "array", "maxItems": 100,
                    "items": {"type": "string", "minLength": 1, "maxLength": 300},
                    "description": "\"Family, Given\" or a literal name, in order."},
                "year": {"type": "integer", "minimum": 1, "maximum": 9999},
                "journal": {"type": "string", "minLength": 1, "maxLength": 1000},
                "publication_date": {"type": "string", "minLength": 1, "maxLength": 64},
                "abstract": {"type": "string", "minLength": 1, "maxLength": 20000},
            }),
            decision_properties(),
        ),
        &[],
    )
}

pub async fn add_paper_by_doi(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    if let Some(preview) = args.str("preview_id") {
        return commit(ctx, args, preview).await;
    }
    let Some(doi) = args.str("doi") else {
        return Err(ToolError::invalid(
            "give doi (to preview) or preview_id with decision (to commit)",
        ));
    };
    let resolution = resolve(ctx, doi).await?;
    let mut draft = resolution.draft.clone();
    if draft.title.trim().is_empty() {
        match args.str("title") {
            Some(title) => draft.title = title.trim().to_owned(),
            None => return Ok(not_found(&resolution)),
        }
    }
    preview(ctx, args, draft, Some(&resolution)).await
}

pub async fn add_paper_from_url(ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    if let Some(preview) = args.str("preview_id") {
        return commit(ctx, args, preview).await;
    }
    let Some(url) = args.str("url") else {
        return Err(ToolError::invalid(
            "give url (to preview) or preview_id with decision (to commit)",
        ));
    };
    let doi = args
        .str("doi")
        .map(str::to_owned)
        .or_else(|| doi_in_url(url));
    let mut resolution = None;
    let mut draft = PaperDraft::default();
    if let Some(doi) = &doi {
        let resolved = resolve(ctx, doi).await?;
        if !resolved.draft.title.trim().is_empty() {
            draft = resolved.draft.clone();
        } else {
            draft.bibliography.doi = Some(resolved.canonical_doi.clone());
        }
        resolution = Some(resolved);
    }
    // Page metadata supplied by the caller fills what the providers did not return.
    if draft.title.trim().is_empty() {
        match args.str("title") {
            Some(title) => draft.title = title.trim().to_owned(),
            None => {
                return Err(ToolError::invalid(
                    "no metadata for this URL: this server does not fetch web pages. Give the \
                     title (and authors, year, ...) read from the page, or a DOI",
                ));
            }
        }
    }
    let b = &mut draft.bibliography;
    if b.url.is_none() {
        b.url = Some(url.to_owned());
    }
    if b.authors.is_empty() {
        b.authors = args.strings("authors").iter().map(|a| contributor(a)).collect();
    }
    if b.year.is_none() {
        b.year = args.i32("year");
    }
    for (field, key) in [
        (&mut b.journal, "journal"),
        (&mut b.publication_date, "publication_date"),
        (&mut b.abstract_text, "abstract"),
    ] {
        if field.is_none() {
            *field = args.str(key).map(str::to_owned);
        }
    }
    if resolution.is_none() {
        draft.source = ImportSource {
            adapter: "mcp-url".into(),
            locator: Some(url.to_owned()),
            ..ImportSource::default()
        };
    }
    preview(ctx, args, draft, resolution.as_ref()).await
}

/// A DOI in a URL path (doi.org/10.x/..., publisher /doi/10.x/... or /article/10.x/...).
pub fn doi_in_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let start = path.find("/10.")? + 1;
    let candidate = &path[start..];
    let (prefix, suffix) = candidate.split_once('/')?;
    let registrant = prefix.strip_prefix("10.")?;
    let valid_prefix = (4..=9).contains(&registrant.len())
        && registrant.chars().all(|c| c.is_ascii_digit() || c == '.');
    (valid_prefix && !suffix.is_empty()).then(|| candidate.trim_end_matches('/').to_owned())
}

async fn resolve(ctx: &Ctx<'_>, doi: &str) -> Result<MetadataResolution, ToolError> {
    match ctx.call(Command::Resolve { doi: doi.into() }).await? {
        Reply::Resolution(resolution) => Ok(*resolution),
        _ => Err(unexpected()),
    }
}

fn not_found(resolution: &MetadataResolution) -> Value {
    json!({
        "status": "metadata_not_found",
        "canonical_doi": resolution.canonical_doi,
        "provider_outcomes": resolution.outcomes,
        "next_step": "Nothing was added. Call again with title (and only metadata you can \
                      cite) to add the DOI manually.",
    })
}

fn draft_json(draft: &PaperDraft) -> Value {
    let b = &draft.bibliography;
    json!({
        "title": draft.title,
        "doi": b.doi,
        "authors": b.authors.iter().filter_map(author).collect::<Vec<_>>(),
        "year": b.year,
        "journal": b.journal,
        "url": b.url,
    })
}

fn match_kind(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Doi => "doi",
        MatchKind::ExternalId => "external_id",
        MatchKind::TitleAuthorYear => "title_author_year",
        MatchKind::SimilarTitle => "similar_title",
    }
}

fn preview_json(preview: &ImportPreview) -> Value {
    let candidate = &preview.candidates[0];
    let duplicates: Vec<Value> = candidate
        .matches
        .iter()
        .enumerate()
        .map(|(i, m)| {
            json!({
                "match_index": i,
                "match": match_kind(m.kind),
                "exact": m.kind.exact(),
                "source": source(&m.existing, None, None, None),
                "differences": m.differences.iter().map(|d| json!({
                    "field": d.field, "existing": d.before, "incoming": d.after
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "preview_id": preview.id.0,
        "candidate": draft_json(&candidate.draft),
        "duplicates": duplicates,
        "existing_match": candidate.existing_match.map(|p| p.0),
        "diagnostics": candidate.diagnostics.iter().map(|d| json!({
            "field": d.field, "message": d.message, "error": d.error
        })).collect::<Vec<_>>(),
        "warnings": preview.warnings,
    })
}

async fn preview(
    ctx: &Ctx<'_>,
    args: &Args,
    draft: PaperDraft,
    resolution: Option<&MetadataResolution>,
) -> Result<Value, ToolError> {
    let destination = ImportDestination {
        collection: args.uuid("collection_id").map(CollectionId),
        tags: Vec::new(),
    };
    let reply = ctx
        .call(Command::Import(ImportRequestUi::Preview(ImportRequest {
            candidates: vec![draft],
            destination,
        })))
        .await?;
    let Reply::Import(ImportReply::Preview(preview)) = reply else {
        return Err(unexpected());
    };
    if preview.candidates.len() != 1 {
        return Err(unexpected());
    }
    let candidate = &preview.candidates[0];
    let blocked = candidate.diagnostics.iter().any(|d| d.error);
    let possible_duplicate = !candidate.matches.is_empty()
        || candidate.existing_match.is_some()
        || !candidate.batch_matches.is_empty();
    if args.bool("confirm") == Some(true) && !possible_duplicate && !blocked {
        let outcome = commit_decision(ctx, &preview.id, ImportAction::Create).await?;
        return outcome_json(ctx, outcome, "created").await;
    }
    let mut result = preview_json(&preview);
    result["status"] = Value::from(if blocked {
        "invalid"
    } else if possible_duplicate {
        "needs_confirmation"
    } else {
        "preview"
    });
    if let Some(resolution) = resolution {
        result["provider_outcomes"] = serde_json::to_value(&resolution.outcomes).unwrap_or_default();
    }
    result["next_step"] = Value::from(if blocked {
        "The record has errors (see diagnostics) and can only be skipped; nothing was added."
    } else {
        "Nothing was added yet. Call again with preview_id and decision: create (new paper), \
         merge (with match_index and merge_fields) or skip."
    });
    Ok(result)
}

async fn commit_decision(
    ctx: &Ctx<'_>,
    preview: &PreviewId,
    action: ImportAction,
) -> Result<ImportOutcome, ToolError> {
    let reply = ctx
        .call(Command::Import(ImportRequestUi::Commit(ImportCommit {
            preview_id: preview.clone(),
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action,
            }],
        })))
        .await
        .map_err(|e| {
            if e.kind == "conflict" || e.kind == "not_found" {
                ToolError::new(
                    e.kind,
                    "the preview expired or the library changed since it was made; request a \
                     new preview (call again without preview_id)",
                )
            } else {
                e
            }
        })?;
    match reply {
        Reply::Import(ImportReply::Committed(outcome)) => Ok(outcome),
        _ => Err(unexpected()),
    }
}

async fn outcome_json(
    ctx: &Ctx<'_>,
    outcome: ImportOutcome,
    status: &str,
) -> Result<Value, ToolError> {
    let paper: Option<PaperId> = outcome
        .created
        .first()
        .or_else(|| outcome.merged.first())
        .copied();
    let source = match paper {
        Some(id) => {
            let paper = get_paper(ctx, id).await?;
            Some(source(&paper, None, None, None))
        }
        None => None,
    };
    Ok(json!({
        "status": if paper.is_none() { "skipped" } else { status },
        "paper_id": paper.map(|p| p.0),
        "source": source,
        "uri": paper.map(|p| uri(p, None, None, None)),
        "diagnostics": outcome.rows.iter().flat_map(|r| r.diagnostics.iter()).map(|d| json!({
            "field": d.field, "message": d.message, "error": d.error
        })).collect::<Vec<_>>(),
    }))
}

async fn commit(ctx: &Ctx<'_>, args: &Args, preview: &str) -> Result<Value, ToolError> {
    let Some(decision) = args.str("decision") else {
        return Err(ToolError::invalid(
            "preview_id needs decision: create, merge or skip",
        ));
    };
    let action = match decision {
        "create" => ImportAction::Create,
        "skip" => ImportAction::Skip,
        _ => {
            let fields = args.strings("merge_fields");
            if fields.is_empty() {
                return Err(ToolError::invalid(
                    "decision=merge needs merge_fields (field names from duplicates[].differences)",
                ));
            }
            ImportAction::Merge {
                match_index: args.u32("match_index").unwrap_or(0) as usize,
                fields,
            }
        }
    };
    let status = match decision {
        "create" => "created",
        "merge" => "merged",
        _ => "skipped",
    };
    let outcome = commit_decision(ctx, &PreviewId(preview.to_owned()), action).await?;
    outcome_json(ctx, outcome, status).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dois_are_found_only_in_url_paths() {
        assert_eq!(
            doi_in_url("https://doi.org/10.1234/abc.def").as_deref(),
            Some("10.1234/abc.def")
        );
        assert_eq!(
            doi_in_url("https://link.example.com/article/10.1007/s123-456/?x=1").as_deref(),
            Some("10.1007/s123-456")
        );
        assert_eq!(
            doi_in_url("https://pubs.example.org/doi/10.1021/jacs.5b0#sec").as_deref(),
            Some("10.1021/jacs.5b0")
        );
        assert_eq!(doi_in_url("https://example.com/v10.2/page"), None);
        assert_eq!(doi_in_url("https://example.com/10.12/x"), None);
        assert_eq!(doi_in_url("https://example.com/?doi=10.1234/x"), None);
    }

    #[test]
    fn write_tools_document_the_grant() {
        assert!(DOI_DESCRIPTION.contains("write grant"));
        assert!(URL_DESCRIPTION.contains("write grant"));
    }
}
