//! Read-only subcommands (LibraryRead only): search, list, show, fulltext, annotations.
use super::{
    find_collection, find_tag, organization, paper_id, paper_summary, parse_id, unexpected,
};
use crate::{
    args::{AnnotationsArgs, FulltextArgs, ListArgs, SearchArgs, ShowArgs, SortKey, SourceKind},
    connect::{Daemon, INTERACTIVE},
    output::{CliError, Kind, Report},
};
use cm_protocol::{
    annotations::{Anchor, Annotation, AnnotationKind, AnnotationReply, AnnotationRequest, Source},
    attachments::PdfReply,
    domain::{AttachmentId, CollectionId},
    ipc::{Command as Ipc, PdfCommand, Reply},
    library::{LibraryReply, LibraryRequest},
    library_query::{
        LibraryCursor, LibraryFilter, LibraryQuery, LibrarySort, PaperVisibility, SortDirection,
        SortField, TagMatch,
    },
    organization::OrganizationRequest,
    search::{
        DocumentSource, ExtractionState, FulltextReply, FulltextRequest, PageTextRequest,
        SearchCursor, SearchReply, SearchRequest, ShortTermScope, SourceSet, UnsearchedScope,
    },
};
use serde_json::{Value, json};

/// DSL quoting (docs/ADR/search-dsl.md): only `"` and `\` are escaped.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn encode_cursor(c: SearchCursor) -> String {
    format!("{}.{}", c.scan_after, c.offset)
}
fn decode_cursor(raw: &str) -> Result<SearchCursor, CliError> {
    let bad = || CliError::usage(format!("invalid --cursor {raw:?}"));
    let (scan, offset) = raw.split_once('.').ok_or_else(bad)?;
    Ok(SearchCursor {
        scan_after: scan.parse().map_err(|_| bad())?,
        offset: offset.parse().map_err(|_| bad())?,
    })
}

fn source_name(s: DocumentSource) -> &'static str {
    match s {
        DocumentSource::Paper => "bibliographic",
        DocumentSource::Annotation => "annotation",
        DocumentSource::Page => "page",
    }
}

pub fn search(daemon: &mut dyn Daemon, args: &SearchArgs) -> Result<Report, CliError> {
    let mut query = args.query.clone();
    for tag in &args.tag {
        query.push_str(&format!(" tag:{}", quoted(tag)));
    }
    let mut request = SearchRequest::new(String::new());
    if let Some(collection) = &args.collection {
        match parse_id::<CollectionId>(collection, "collection") {
            Ok(id) => request.collection = Some(id),
            Err(_) => query.push_str(&format!(" collection:{}", quoted(collection))),
        }
    }
    request.query = query;
    request.limit = args.limit;
    request.snippet_chars = args.snippet_chars;
    request.cursor = args.cursor.as_deref().map(decode_cursor).transpose()?;
    if !args.sources.is_empty() {
        request.sources = SourceSet {
            bibliographic: args.sources.contains(&SourceKind::Bibliographic),
            annotations: args.sources.contains(&SourceKind::Annotations),
            pages: args.sources.contains(&SourceKind::Pages),
        };
    }
    if args.all_pages {
        request.short_term_scope = ShortTermScope::AllPages;
    }
    if args.include_trash {
        request.visibility = PaperVisibility::All;
    }
    let page =
        match daemon.call(Ipc::Search(Box::new(request.clone())), INTERACTIVE)? {
            Reply::Search(SearchReply::Results(page)) => page,
            Reply::Search(SearchReply::Invalid(errors)) => {
                let message = errors
                    .diagnostics
                    .iter()
                    .map(|d| format!("{} at {}..{}", d.message, d.start, d.end))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(CliError::new(Kind::InvalidInput, format!("invalid query: {message}"))
                .with_report(Report::new(json!({
                    "query": request.query,
                    "diagnostics": errors.diagnostics.iter().map(|d| json!({
                        "code": d.code, "message": d.message, "start": d.start, "end": d.end,
                        "corrections": d.corrections.iter().map(|c| json!({
                            "start": c.start, "end": c.end, "replacement": c.replacement,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "truncated": errors.truncated,
                }))));
            }
            _ => return Err(unexpected()),
        };
    let coverage = &page.coverage;
    let mut report = Report::new(json!({
        "query": request.query,
        "hits": page.hits.iter().map(|h| json!({
            "paper_id": h.paper_id.0,
            "title": h.paper_title,
            "doi": h.doi,
            "deleted": h.deleted,
            "source": source_name(h.source),
            "attachment_id": h.attachment_id.map(|a| a.0),
            "annotation_id": h.annotation_id,
            "page_index": h.page_index,
            "page_number": h.page_index.map(|p| p + 1),
            "page_label": h.page_label,
            "uri": h.attachment_id.map(|a| match h.page_index {
                Some(p) => format!("paper://{}/attachment/{}?page={}", h.paper_id.0, a.0, p + 1),
                None => format!("paper://{}/attachment/{}", h.paper_id.0, a.0),
            }),
            "matched_fields": h.matched_fields,
            "snippet": h.snippet,
            "highlights": h.highlights.iter().map(|r| [r.start, r.end]).collect::<Vec<_>>(),
            "snippet_truncated": h.snippet_truncated,
            "score": h.score,
        })).collect::<Vec<_>>(),
        "total_hits": page.total_hits,
        "next_cursor": page.next_page.map(encode_cursor),
        "coverage": {
            "complete": coverage.complete,
            "fts_truncated": coverage.fts_truncated,
            "timed_out": coverage.timed_out,
            "short_terms": coverage.short_terms,
            "scan": format!("{:?}", coverage.scan),
            "scanned_documents": coverage.scanned_documents,
            "unsearched": coverage.unsearched.iter().map(|u| match u {
                UnsearchedScope::PdfPagesForShortTerms => json!({"scope": "pdf_pages_for_short_terms"}),
                UnsearchedScope::DocumentsAfterWindow => json!({"scope": "documents_after_window"}),
                UnsearchedScope::PendingExtraction { attachments } => json!({"scope": "pending_extraction", "attachments": attachments}),
                UnsearchedScope::NoExtractableText { attachments } => json!({"scope": "no_extractable_text", "attachments": attachments}),
            }).collect::<Vec<_>>(),
            "continuation": coverage.continuation.map(encode_cursor),
            "unresolved": coverage.unresolved.iter().map(|u| format!("{u:?}")).collect::<Vec<_>>(),
        },
    }));
    if !coverage.complete {
        report.warnings.push(
            "search coverage is incomplete; see data.coverage (results are not exhaustive)".into(),
        );
    }
    report.text.push(format!("{} hits", page.total_hits));
    for h in &page.hits {
        let at = h
            .page_index
            .map(|p| format!(" p.{}", p + 1))
            .unwrap_or_default();
        report.text.push(format!(
            "{}  [{}{}] {}  — {}",
            h.paper_id.0,
            source_name(h.source),
            at,
            h.paper_title,
            h.snippet.replace('\n', " ")
        ));
    }
    if let Some(next) = page.next_page {
        report
            .text
            .push(format!("next: --cursor {}", encode_cursor(next)));
    }
    Ok(report)
}

pub fn list(daemon: &mut dyn Daemon, args: &ListArgs) -> Result<Report, CliError> {
    let mut filter = LibraryFilter {
        visibility: if args.trash {
            PaperVisibility::Deleted
        } else if args.include_trash {
            PaperVisibility::All
        } else {
            PaperVisibility::Active
        },
        keyword: args.keyword.clone(),
        author: args.author.clone(),
        year_from: args.year_from,
        year_to: args.year_to,
        has_pdf: args.has_pdf,
        tag_match: if args.any_tag {
            TagMatch::Any
        } else {
            TagMatch::All
        },
        ..LibraryFilter::default()
    };
    if args.collection.is_some() || !args.tag.is_empty() {
        let org = organization(daemon)?;
        filter.collection = args
            .collection
            .as_deref()
            .map(|c| find_collection(&org, c))
            .transpose()?;
        filter.tags = args
            .tag
            .iter()
            .map(|t| find_tag(&org, t))
            .collect::<Result<_, _>>()?;
    }
    let query = LibraryQuery {
        filter,
        sort: LibrarySort {
            field: match args.sort {
                SortKey::Id => SortField::Id,
                SortKey::Title => SortField::Title,
                SortKey::Year => SortField::Year,
                SortKey::Created => SortField::CreatedAt,
                SortKey::Updated => SortField::UpdatedAt,
            },
            direction: if args.desc {
                SortDirection::Desc
            } else {
                SortDirection::Asc
            },
        },
        after: args
            .cursor
            .clone()
            .map(LibraryCursor::from_encoded)
            .transpose()
            .map_err(|e| CliError::usage(format!("invalid --cursor: {e}")))?,
        limit: args.limit,
    };
    let page = match daemon.call(Ipc::Query(query), INTERACTIVE)? {
        Reply::Page(page) => page,
        _ => return Err(unexpected()),
    };
    let mut report = Report::new(json!({
        "papers": page.papers.iter().map(paper_summary).collect::<Vec<_>>(),
        "count": page.papers.len(),
        "next_cursor": page.next.as_ref().map(|c| c.as_str()),
    }));
    for p in &page.papers {
        report.text.push(format!(
            "{}  {}  {}",
            p.id.0,
            p.bibliography
                .year
                .map(|y| y.to_string())
                .unwrap_or_else(|| "----".into()),
            p.title
        ));
    }
    if let Some(next) = &page.next {
        report
            .text
            .push(format!("next: --cursor {}", next.as_str()));
    }
    Ok(report)
}

pub fn show(daemon: &mut dyn Daemon, args: &ShowArgs) -> Result<Report, CliError> {
    let id = paper_id(&args.paper)?;
    let paper = match daemon.call(Ipc::Library(LibraryRequest::Get(id)), INTERACTIVE)? {
        Reply::Library(LibraryReply::Paper(paper)) => paper,
        _ => return Err(unexpected()),
    };
    let org = match daemon.call(
        Ipc::Organization(OrganizationRequest::ForPaper(id)),
        INTERACTIVE,
    )? {
        Reply::Organization(org) => org,
        _ => return Err(unexpected()),
    };
    let attachments = match daemon.call(Ipc::Pdf(PdfCommand::List(id)), INTERACTIVE)? {
        Reply::Pdf(PdfReply::Attachments(list)) => list,
        _ => return Err(unexpected()),
    };
    let mut report = Report::new(json!({
        "paper": *paper,
        "collections": org.collections.iter().map(|c| json!({"id": c.id.0, "name": c.name, "parent": c.parent.map(|p| p.0)})).collect::<Vec<_>>(),
        "tags": org.tags.iter().map(|t| json!({"id": t.id.0, "name": t.name})).collect::<Vec<_>>(),
        "attachments": attachments.iter().map(|(a, status)| json!({
            "id": a.id.0,
            "original_name": a.original_name,
            "media_type": a.media_type,
            "byte_size": a.byte_size,
            "sha256": super::add::hex(a.sha256),
            "status": format!("{status:?}").to_lowercase(),
            "uri": format!("paper://{}/attachment/{}", id.0, a.id.0),
        })).collect::<Vec<_>>(),
    }));
    report.text.push(format!("{}  {}", paper.id.0, paper.title));
    let b = &paper.bibliography;
    if !b.authors.is_empty() {
        report.text.push(format!(
            "authors: {}",
            b.authors
                .iter()
                .map(super::contributor)
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    for (label, value) in [
        ("year", b.year.map(|y| y.to_string())),
        ("journal", b.journal.clone()),
        ("doi", b.doi.clone()),
        ("citation key", b.citation_key.clone()),
    ] {
        if let Some(value) = value {
            report.text.push(format!("{label}: {value}"));
        }
    }
    for (a, status) in &attachments {
        report.text.push(format!(
            "attachment {} {} ({status:?})",
            a.id.0, a.original_name
        ));
    }
    Ok(report)
}

fn page_range(args: &FulltextArgs) -> Result<(u32, u32), CliError> {
    if let Some(page) = args.page {
        return Ok((page - 1, 1));
    }
    let Some(range) = &args.pages else {
        return Ok((0, args.max_pages));
    };
    let bad = || {
        CliError::usage(format!(
            "--pages {range:?}: expected FROM-TO (1-based, at most 50 pages)"
        ))
    };
    let (from, to) = range.split_once('-').ok_or_else(bad)?;
    let (from, to): (u32, u32) = (
        from.trim().parse().map_err(|_| bad())?,
        to.trim().parse().map_err(|_| bad())?,
    );
    if from == 0 || to < from || to - from >= 50 {
        return Err(bad());
    }
    Ok((from - 1, to - from + 1))
}

fn state_name(state: ExtractionState) -> String {
    let name = format!("{state:?}");
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

pub fn fulltext(daemon: &mut dyn Daemon, args: &FulltextArgs) -> Result<Report, CliError> {
    let paper = paper_id(&args.paper)?;
    let (from_page, max_pages) = page_range(args)?;
    let status = match daemon.call(Ipc::Fulltext(FulltextRequest::Status(paper)), INTERACTIVE)? {
        Reply::Fulltext(FulltextReply::Status(list)) => list,
        _ => return Err(unexpected()),
    };
    let attachment = match &args.attachment {
        Some(raw) => {
            let id: AttachmentId = parse_id(raw, "attachment ID")?;
            if !status.iter().any(|s| s.attachment_id == id) {
                return Err(CliError::new(
                    Kind::NotFound,
                    "attachment does not belong to this paper",
                ));
            }
            id
        }
        None => status
            .first()
            .map(|s| s.attachment_id)
            .ok_or_else(|| CliError::new(Kind::NotFound, "paper has no PDF attachment"))?,
    };
    let read = match daemon.call(
        Ipc::Fulltext(FulltextRequest::Pages(PageTextRequest {
            attachment,
            from_page,
            max_pages,
            max_chars: args.max_chars,
        })),
        INTERACTIVE,
    )? {
        Reply::Fulltext(FulltextReply::Pages(read)) => read,
        _ => return Err(unexpected()),
    };
    let e = &read.extraction;
    let mut report = Report::new(json!({
        "paper_id": read.paper_id.0,
        "attachment_id": read.attachment_id.0,
        "extraction": {
            "state": state_name(e.state),
            "page_count": e.page_count,
            "pages_done": e.pages_done,
            "pages_with_text": e.pages_with_text,
            "truncated_pages": e.truncated_pages,
            "detail": e.detail,
        },
        "attachments": status.iter().map(|s| json!({"id": s.attachment_id.0, "state": state_name(s.state), "page_count": s.page_count})).collect::<Vec<_>>(),
        "pages": read.pages.iter().map(|p| json!({
            "page_index": p.page_index,
            "page_number": p.page_index + 1,
            "page_label": p.page_label,
            "uri": format!("paper://{}/attachment/{}?page={}", read.paper_id.0, read.attachment_id.0, p.page_index + 1),
            "text": p.text,
            "truncated": p.truncated,
        })).collect::<Vec<_>>(),
        "next_page_number": read.next_page.map(|p| p + 1),
        "limits": { "from_page_number": from_page + 1, "max_pages": max_pages, "max_chars": args.max_chars },
    }));
    if !matches!(e.state, ExtractionState::Complete) {
        report.warnings.push(format!(
            "text extraction state is {}; page text may be missing{}",
            state_name(e.state),
            e.detail
                .as_ref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        ));
    }
    if read.pages.iter().any(|p| p.truncated) {
        report.warnings.push("text truncated by --max-chars".into());
    }
    for p in &read.pages {
        report.text.push(format!(
            "--- page {}{} ---",
            p.page_index + 1,
            p.page_label
                .as_ref()
                .map(|l| format!(" ({l})"))
                .unwrap_or_default()
        ));
        report.text.push(p.text.clone());
    }
    Ok(report)
}

pub fn annotations(daemon: &mut dyn Daemon, args: &AnnotationsArgs) -> Result<Report, CliError> {
    let paper = paper_id(&args.paper)?;
    let request = match &args.attachment {
        Some(raw) => AnnotationRequest::List(parse_id(raw, "attachment ID")?),
        None => AnnotationRequest::ForPaper(paper),
    };
    let mut list = match daemon.call(Ipc::Annotations(request), INTERACTIVE)? {
        Reply::Annotations(AnnotationReply::List(list)) => list,
        _ => return Err(unexpected()),
    };
    let total = list.len();
    let truncated = total > args.limit as usize;
    list.truncate(args.limit as usize);
    let mut report = Report::new(json!({
        "paper_id": paper.0,
        "annotations": list.iter().map(|a| annotation_json(paper, a)).collect::<Vec<_>>(),
        "total": total,
        "truncated": truncated,
    }));
    report.text.push(format!("{total} annotations"));
    if truncated {
        report.warnings.push(format!(
            "showing {} of {total} annotations (--limit)",
            list.len()
        ));
    }
    for a in &list {
        let at = match &a.anchor {
            Anchor::Resolved { page_index, .. } => format!("p.{}", page_index + 1),
            Anchor::Unresolved { .. } => "unresolved".into(),
        };
        report.text.push(format!(
            "{} [{at}] {:?}/{:?}: {}{}",
            a.id.0,
            a.kind,
            a.source,
            a.quote,
            a.comment
                .as_ref()
                .map(|c| format!(" — {c}"))
                .unwrap_or_default()
        ));
    }
    Ok(report)
}

fn annotation_json(paper: cm_protocol::domain::PaperId, a: &Annotation) -> Value {
    let (page_index, page_label, quads, unresolved) = match &a.anchor {
        Anchor::Resolved {
            page_index,
            page_label,
            quads,
        } => (Some(*page_index), page_label.clone(), quads.len(), None),
        Anchor::Unresolved { reason } => (None, None, 0, Some(reason.clone())),
    };
    json!({
        "id": a.id.0,
        "attachment_id": a.attachment_id.0,
        "kind": match a.kind { AnnotationKind::Highlight => "highlight", AnnotationKind::Note => "note" },
        "source": match a.source { Source::Human => "human", Source::Ai => "ai", Source::Import => "import" },
        "page_index": page_index,
        "page_number": page_index.map(|p| p + 1),
        "page_label": page_label,
        "quads": quads,
        "unresolved_reason": unresolved,
        "uri": page_index.map(|p| format!("paper://{}/attachment/{}?page={}", paper.0, a.attachment_id.0, p + 1)),
        "quote": a.quote,
        "comment": a.comment,
        "color": a.color,
        "confirmed_at": a.confirmed_at,
        "created_at": a.created_at,
        "updated_at": a.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsl_quoting_and_cursor_roundtrip() {
        assert_eq!(quoted(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        let c = SearchCursor {
            scan_after: 42,
            offset: 7,
        };
        assert_eq!(decode_cursor(&encode_cursor(c)).unwrap(), c);
        assert!(decode_cursor("x").is_err());
    }

    #[test]
    fn page_ranges_are_one_based_and_bounded() {
        let mut args = FulltextArgs {
            paper: String::new(),
            page: Some(5),
            pages: None,
            max_pages: 5,
            max_chars: 100,
            attachment: None,
        };
        assert_eq!(page_range(&args).unwrap(), (4, 1));
        args.page = None;
        assert_eq!(page_range(&args).unwrap(), (0, 5));
        args.pages = Some("3-7".into());
        assert_eq!(page_range(&args).unwrap(), (2, 5));
        for bad in ["0-1", "7-3", "1-51", "x"] {
            args.pages = Some(bad.into());
            assert!(page_range(&args).is_err(), "{bad}");
        }
        assert_eq!(state_name(ExtractionState::NoText), "no_text");
    }
}
