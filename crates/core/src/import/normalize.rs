//! The single import normalizer; adapters and SQLite use this same implementation.
use super::*;
use crate::{
    ServiceError,
    papers::{normalize_doi, validate_metadata},
};

pub const MAX_TITLE_BYTES: usize = 16 * 1024;
pub const MAX_DRAFT_BYTES: usize = 64 * 1024;
pub const MAX_BATCH: usize = 100;
pub const MAX_LIBRARY_PAPERS: usize = 20_000;
pub const MAX_INVENTORY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
pub fn invalid(message: &str) -> ServiceError {
    ServiceError::InvalidInput(message.into())
}
pub fn spaces(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn text(value: &str, limit: usize) -> Result<(), ServiceError> {
    if value.len() > limit || value.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return Err(invalid(
            "import text exceeds limit or contains control characters",
        ));
    }
    Ok(())
}
pub fn validate_limits(draft: &PaperDraft) -> Result<(), ServiceError> {
    if serde_json::to_vec(draft)
        .map_err(|_| ServiceError::Unavailable)?
        .len()
        > MAX_DRAFT_BYTES
        || draft.bibliography.authors.len() > 256
        || draft.external_ids.len() > 128
        || draft.diagnostics.len() > 128
    {
        return Err(invalid("import row exceeds byte/count budget"));
    }
    text(&draft.title, MAX_TITLE_BYTES)?;
    text(&draft.source.adapter, 256)?;
    if let Some(v) = &draft.source.locator {
        text(v, 4096)?;
    }
    for d in &draft.diagnostics {
        text(&d.field, 128)?;
        text(&d.message, 1024)?;
    }
    Ok(())
}
/// Known IDs canonicalize here; unknown namespaces/values retain spelling.
pub fn external_id(id: &ExternalId) -> Result<ExternalId, ServiceError> {
    let ns = id.namespace.trim();
    let value = id.value.trim();
    text(ns, 128)?;
    text(value, 4096)?;
    if ns.is_empty() || value.is_empty() {
        return Err(invalid("empty external identifier"));
    }
    let known = ns.to_ascii_lowercase();
    let (namespace, value) = match known.as_str() {
        "doi" => (known, normalize_doi(value)?),
        "arxiv" => {
            let v = value
                .strip_prefix("https://arxiv.org/abs/")
                .or_else(|| value.strip_prefix("arXiv:"))
                .unwrap_or(value);
            if v.chars().any(char::is_whitespace) {
                return Err(invalid("invalid arXiv identifier"));
            }
            (known, v.to_ascii_lowercase())
        }
        "pmid" => {
            if !value.bytes().all(|v| v.is_ascii_digit()) {
                return Err(invalid("invalid PMID"));
            }
            (known, value.trim_start_matches('0').to_owned())
        }
        _ => (ns.to_owned(), value.to_owned()),
    };
    if value.is_empty() {
        return Err(invalid("empty external identifier"));
    }
    Ok(ExternalId { namespace, value })
}
pub fn normalize(input: &PaperDraft) -> Result<PaperDraft, ServiceError> {
    validate_limits(input)?;
    let mut out = input.clone();
    out.title = spaces(&out.title);
    let b = &mut out.bibliography;
    for a in &mut b.authors {
        for v in [&mut a.given_name, &mut a.family_name, &mut a.literal_name] {
            if let Some(s) = v {
                *s = spaces(s);
                if s.is_empty() {
                    *v = None;
                }
            }
        }
        a.role = spaces(&a.role).to_lowercase();
    }
    if let Some(date) = &mut b.publication_date {
        *date = date.trim().replace('/', "-");
        let parts = date.split('-').collect::<Vec<_>>();
        if parts.is_empty()
            || parts.len() > 3
            || parts[0].len() != 4
            || parts
                .iter()
                .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(invalid("date must be YYYY, YYYY-MM or YYYY-MM-DD"));
        }
        let year: i32 = parts[0].parse().map_err(|_| invalid("invalid year"))?;
        let month = parts
            .get(1)
            .map(|v| v.parse::<u32>())
            .transpose()
            .map_err(|_| invalid("invalid month"))?;
        let day = parts
            .get(2)
            .map(|v| v.parse::<u32>())
            .transpose()
            .map_err(|_| invalid("invalid day"))?;
        if year == 0 || month.is_some_and(|m| !(1..=12).contains(&m)) {
            return Err(invalid("invalid date"));
        }
        if let (Some(m), Some(d)) = (month, day) {
            let max = match m {
                2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
                2 => 28,
                4 | 6 | 9 | 11 => 30,
                _ => 31,
            };
            if d == 0 || d > max {
                return Err(invalid("invalid date"));
            }
        }
        if b.year.is_some_and(|v| v != year) {
            return Err(invalid("date/year disagreement"));
        }
        b.year = Some(year);
        *date = match (month, day) {
            (Some(m), Some(d)) => format!("{year:04}-{m:02}-{d:02}"),
            (Some(m), _) => format!("{year:04}-{m:02}"),
            _ => format!("{year:04}"),
        };
    }
    if let Some(p) = &mut b.pages {
        *p = spaces(p)
            .split(',')
            .map(|part| {
                let part = part.trim();
                let range = part
                    .split_once("--")
                    .or_else(|| part.split_once(['-', '–', '—']));
                match range {
                    Some((start, end))
                        if start.chars().any(|c| c.is_ascii_digit())
                            && end.chars().any(|c| c.is_ascii_digit()) =>
                    {
                        format!("{}–{}", start.trim(), end.trim())
                    }
                    _ => part.to_owned(),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
    }
    if let Some(k) = &mut b.citation_key {
        *k = k.trim().to_owned();
        text(k, 256)?;
        if k.is_empty() || k.chars().any(char::is_whitespace) {
            return Err(invalid("invalid citation key"));
        }
    }
    out.external_ids = out
        .external_ids
        .iter()
        .map(external_id)
        .collect::<Result<_, _>>()?;
    let mut paper = Paper {
        id: PaperId(Default::default()),
        title: out.title.clone(),
        bibliography: out.bibliography.clone(),
        external_ids: out.external_ids.clone(),
        created_at: None,
        updated_at: None,
        deleted_at: None,
    };
    validate_metadata(&mut paper)?;
    out.bibliography = paper.bibliography;
    validate_limits(&out)?;
    Ok(out)
}
pub fn paper_draft(p: &Paper) -> PaperDraft {
    PaperDraft {
        title: p.title.clone(),
        external_ids: p.external_ids.clone(),
        bibliography: p.bibliography.clone(),
        ..Default::default()
    }
}
pub fn check_snapshot(snapshot: &ImportSnapshot) -> Result<(), ServiceError> {
    if snapshot.papers.len() > MAX_LIBRARY_PAPERS
        || serde_json::to_vec(&snapshot.papers)
            .map_err(|_| ServiceError::Unavailable)?
            .len()
            > MAX_INVENTORY_BYTES
    {
        return Err(invalid(
            "library inventory exceeds import budget; no fields truncated",
        ));
    }
    Ok(())
}
