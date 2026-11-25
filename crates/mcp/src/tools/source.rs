//! Citation `source` blocks and internal URIs (DESIGN §14.3).
//!
//! Every text a tool returns is paired with where it came from: paper_id, doi, title,
//! authors, year, attachment_id, page_index (0-based physical page) and page_label (the
//! PDF's printed label, when known). The location comes from the daemon's stored data
//! (search hit, extracted page, annotation anchor) — never inferred by this server.
//!
//! Internal URI scheme (stable, for jumping back from an AI answer):
//! - `paper://<paper_id>`
//! - `paper://<paper_id>/attachment/<attachment_id>`
//! - `...?page=<n>` with n = page_index + 1 (1-based physical page, not the printed label)
//! - `...&annotation=<annotation_id>` (or `?annotation=` without a page)
use super::{Ctx, ToolError};
use cm_protocol::{
    domain::{AttachmentId, Contributor, Paper, PaperId},
    ipc::{Command, Reply},
    library::{LibraryReply, LibraryRequest},
};
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn uri(
    paper: PaperId,
    attachment: Option<AttachmentId>,
    page_index: Option<u32>,
    annotation: Option<&str>,
) -> String {
    let mut uri = format!("paper://{}", paper.0);
    if let Some(attachment) = attachment {
        uri.push_str(&format!("/attachment/{}", attachment.0));
    }
    let mut query = Vec::new();
    if let Some(page) = page_index.filter(|_| attachment.is_some()) {
        query.push(format!("page={}", u64::from(page) + 1));
    }
    if let Some(annotation) = annotation {
        query.push(format!("annotation={annotation}"));
    }
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(&query.join("&"));
    }
    uri
}

/// "Family, Given", or the literal name.
pub fn author(c: &Contributor) -> Option<String> {
    let family = c
        .family_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let given = c
        .given_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (family, given) {
        (Some(f), Some(g)) => Some(format!("{f}, {g}")),
        (Some(f), None) => Some(f.to_owned()),
        (None, Some(g)) => Some(g.to_owned()),
        (None, None) => c
            .literal_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    }
}

pub fn authors(paper: &Paper) -> Vec<String> {
    paper
        .bibliography
        .authors
        .iter()
        .filter(|c| c.role.is_empty() || c.role.eq_ignore_ascii_case("author"))
        .filter_map(author)
        .collect()
}

/// Parses "Family, Given" (or a literal name) into an author contributor.
pub fn contributor(name: &str) -> Contributor {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let (family, given, literal) = match name.split_once(',') {
        Some((family, given)) if !family.trim().is_empty() && !given.trim().is_empty() => (
            Some(family.trim().to_owned()),
            Some(given.trim().to_owned()),
            None,
        ),
        _ => (None, None, Some(name)),
    };
    Contributor {
        given_name: given,
        family_name: family,
        literal_name: literal,
        orcid: None,
        role: "author".into(),
    }
}

/// The source block for a location in a known paper.
pub fn source(
    paper: &Paper,
    attachment: Option<AttachmentId>,
    page_index: Option<u32>,
    page_label: Option<&str>,
) -> Value {
    json!({
        "paper_id": paper.id.0,
        "doi": paper.bibliography.doi,
        "title": paper.title,
        "authors": authors(paper),
        "year": paper.bibliography.year,
        "attachment_id": attachment.map(|a| a.0),
        "page_index": page_index,
        "page_label": page_label,
        "uri": uri(paper.id, attachment, page_index, None),
    })
}

/// Fallback when the paper record could not be read (e.g. trashed meanwhile).
pub fn partial_source(
    paper: PaperId,
    title: &str,
    doi: Option<&str>,
    attachment: Option<AttachmentId>,
    page_index: Option<u32>,
    page_label: Option<&str>,
) -> Value {
    json!({
        "paper_id": paper.0,
        "doi": doi,
        "title": title,
        "authors": [],
        "year": null,
        "attachment_id": attachment.map(|a| a.0),
        "page_index": page_index,
        "page_label": page_label,
        "uri": uri(paper, attachment, page_index, None),
    })
}

/// Reads one paper (any visibility the daemon serves for `Get`).
pub async fn get_paper(ctx: &Ctx<'_>, id: PaperId) -> Result<Paper, ToolError> {
    match ctx.call(Command::Library(LibraryRequest::Get(id))).await? {
        Reply::Library(LibraryReply::Paper(paper)) => Ok(*paper),
        _ => Err(unexpected()),
    }
}

/// Per-call cache so a result list reads each paper once.
#[derive(Default)]
pub struct Papers(HashMap<PaperId, Option<Paper>>);
impl Papers {
    /// `None` when the paper is gone or unreadable; other errors (permission, daemon) abort.
    pub async fn get(&mut self, ctx: &Ctx<'_>, id: PaperId) -> Result<Option<&Paper>, ToolError> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.0.entry(id) {
            slot.insert(match get_paper(ctx, id).await {
                Ok(paper) => Some(paper),
                Err(e) if e.kind == "not_found" => None,
                Err(e) => return Err(e),
            });
        }
        Ok(self.0.get(&id).and_then(Option::as_ref))
    }
}

pub fn unexpected() -> ToolError {
    ToolError::new(
        "protocol",
        "unexpected reply from the library service (version mismatch?)",
    )
}

/// At most `max` characters of `text`, and whether it was cut.
pub fn clip(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((cut, _)) => (text[..cut].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn uris_use_one_based_physical_pages() {
        let p = PaperId(Uuid::from_u128(1));
        let a = AttachmentId(Uuid::from_u128(2));
        assert_eq!(
            uri(p, None, None, None),
            "paper://00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(
            uri(p, Some(a), Some(4), None),
            "paper://00000000-0000-0000-0000-000000000001/attachment/00000000-0000-0000-0000-000000000002?page=5"
        );
        assert!(uri(p, Some(a), Some(0), Some("x")).ends_with("?page=1&annotation=x"));
        assert!(
            uri(p, Some(a), None, Some("x"))
                .ends_with("/attachment/00000000-0000-0000-0000-000000000002?annotation=x")
        );
    }

    #[test]
    fn authors_round_trip_and_clip_counts_characters() {
        let c = contributor("  Doe,   Jane ");
        assert_eq!(
            (c.family_name.as_deref(), c.given_name.as_deref()),
            (Some("Doe"), Some("Jane"))
        );
        assert_eq!(author(&c).unwrap(), "Doe, Jane");
        let literal = contributor("Research Consortium");
        assert_eq!(author(&literal).unwrap(), "Research Consortium");
        assert_eq!(clip("日本語テキスト", 3), ("日本語".to_owned(), true));
        assert_eq!(clip("abc", 3), ("abc".to_owned(), false));
    }
}
