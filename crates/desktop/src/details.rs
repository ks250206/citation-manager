//! Metadata form fields preserve all unedited bibliographic values and external IDs.
use cm_protocol::{domain::*, import::PaperDraft, library::ServiceError};
pub const FIELDS: &[&str] = &[
    "Title",
    "Abstract",
    "DOI",
    "Year",
    "Type",
    "Journal",
    "Authors (one literal name per ;)",
    "Date",
    "Volume",
    "Issue",
    "Pages",
    "URL",
    "Language",
    "Citation key",
];
pub fn values(draft: &PaperDraft) -> Vec<String> {
    let b = &draft.bibliography;
    vec![
        draft.title.clone(),
        b.abstract_text.clone().unwrap_or_default(),
        b.doi.clone().unwrap_or_default(),
        b.year.map(|v| v.to_string()).unwrap_or_default(),
        b.paper_type.clone().unwrap_or_default(),
        b.journal.clone().unwrap_or_default(),
        b.authors
            .iter()
            .map(|a| {
                a.literal_name.clone().unwrap_or_else(|| {
                    format!(
                        "{} {}",
                        a.given_name.as_deref().unwrap_or(""),
                        a.family_name.as_deref().unwrap_or("")
                    )
                    .trim()
                    .into()
                })
            })
            .collect::<Vec<String>>()
            .join("; "),
        b.publication_date.clone().unwrap_or_default(),
        b.volume.clone().unwrap_or_default(),
        b.issue.clone().unwrap_or_default(),
        b.pages.clone().unwrap_or_default(),
        b.url.clone().unwrap_or_default(),
        b.language.clone().unwrap_or_default(),
        b.citation_key.clone().unwrap_or_default(),
    ]
}
pub fn apply(base: &PaperDraft, input: &[String]) -> Result<PaperDraft, ServiceError> {
    if input.len() != FIELDS.len() {
        return Err(ServiceError::Unavailable);
    }
    let previous = values(base);
    let mut draft = base.clone();
    let optional = |index: usize| {
        if input[index].is_empty() {
            None
        } else {
            Some(input[index].clone())
        }
    };
    draft.title = input[0].clone();
    let b = &mut draft.bibliography;
    if input[1] != previous[1] {
        b.abstract_text = optional(1);
    }
    if input[2] != previous[2] {
        b.doi = optional(2);
    }
    if input[3] != previous[3] {
        b.year = if input[3].trim().is_empty() {
            None
        } else {
            Some(
                input[3]
                    .parse()
                    .map_err(|_| ServiceError::InvalidInput("Year must be a number".into()))?,
            )
        };
    }
    if input[4] != previous[4] {
        b.paper_type = optional(4);
    }
    if input[5] != previous[5] {
        b.journal = optional(5);
    }
    if input[6] != previous[6] {
        b.authors = input[6]
            .split(';')
            .filter(|s| !s.trim().is_empty())
            .map(|s| Contributor {
                given_name: None,
                family_name: None,
                literal_name: Some(s.trim().into()),
                orcid: None,
                role: "author".into(),
            })
            .collect();
    }
    if input[7] != previous[7] {
        b.publication_date = optional(7);
    }
    if input[8] != previous[8] {
        b.volume = optional(8);
    }
    if input[9] != previous[9] {
        b.issue = optional(9);
    }
    if input[10] != previous[10] {
        b.pages = optional(10);
    }
    if input[11] != previous[11] {
        b.url = optional(11);
    }
    if input[12] != previous[12] {
        b.language = optional(12);
    }
    if input[13] != previous[13] {
        b.citation_key = optional(13);
    }
    Ok(draft)
}
/// Plain-text citation for the clipboard (`y` / "Copy citation"):
/// `Family, G.; Other, X. (2017). Title. Journal, 1(2), 3–4. https://doi.org/…`.
/// Missing parts are omitted; no style engine is implied.
pub fn citation_text(title: &str, b: &cm_protocol::domain::Bibliography) -> String {
    let authors: Vec<String> = b
        .authors
        .iter()
        .filter(|a| a.role.is_empty() || a.role == "author")
        .filter_map(|a| {
            if let Some(literal) = a.literal_name.as_deref().filter(|s| !s.trim().is_empty()) {
                return Some(literal.trim().to_owned());
            }
            let family = a.family_name.as_deref().map(str::trim).unwrap_or("");
            let initials: String = a
                .given_name
                .as_deref()
                .unwrap_or("")
                .split_whitespace()
                .filter_map(|g| g.chars().next())
                .map(|c| format!("{c}."))
                .collect::<Vec<_>>()
                .join(" ");
            match (family.is_empty(), initials.is_empty()) {
                (true, true) => None,
                (true, false) => Some(initials),
                (false, true) => Some(family.to_owned()),
                (false, false) => Some(format!("{family}, {initials}")),
            }
        })
        .collect();
    let mut out = String::new();
    if !authors.is_empty() {
        out.push_str(&authors.join("; "));
        out.push(' ');
    }
    if let Some(year) = b.year {
        out.push_str(&format!("({year}). "));
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    out.push_str(title.trim_end_matches('.'));
    out.push('.');
    if let Some(journal) = b.journal.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push(' ');
        out.push_str(journal.trim());
        if let Some(volume) = b.volume.as_deref().filter(|s| !s.is_empty()) {
            out.push_str(&format!(", {volume}"));
            if let Some(issue) = b.issue.as_deref().filter(|s| !s.is_empty()) {
                out.push_str(&format!("({issue})"));
            }
        }
        if let Some(pages) = b.pages.as_deref().filter(|s| !s.is_empty()) {
            out.push_str(&format!(", {pages}"));
        }
        out.push('.');
    }
    if let Some(doi) = b.doi.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!(" https://doi.org/{}", doi.trim()));
    } else if let Some(url) = b.url.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!(" {}", url.trim()));
    }
    out
}
