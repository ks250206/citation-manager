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
