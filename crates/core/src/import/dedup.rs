use super::*;
use normalize::{external_id, spaces};

pub fn doi(draft: &PaperDraft) -> Option<String> {
    draft
        .bibliography
        .doi
        .as_deref()
        .and_then(|d| crate::papers::normalize_doi(d).ok())
        .or_else(|| {
            draft
                .external_ids
                .iter()
                .filter_map(|id| external_id(id).ok())
                .find(|id| id.namespace == "doi")
                .map(|id| id.value)
        })
}
fn ids(draft: &PaperDraft) -> Vec<ExternalId> {
    draft
        .external_ids
        .iter()
        .filter_map(|id| external_id(id).ok())
        .filter(|id| matches!(id.namespace.as_str(), "arxiv" | "pmid"))
        .collect()
}
fn folded(v: &str) -> String {
    spaces(v).to_lowercase()
}
fn author(d: &PaperDraft) -> Option<String> {
    d.bibliography
        .authors
        .iter()
        .find(|a| a.role.eq_ignore_ascii_case("author"))
        .and_then(|a| a.literal_name.as_ref().or(a.family_name.as_ref()))
        .map(|v| folded(v))
}
/// Identifier equality is exact. Bibliographic resemblance is only a review suggestion.
pub fn classify(a: &PaperDraft, b: &PaperDraft) -> Option<MatchKind> {
    if doi(a).zip(doi(b)).is_some_and(|(a, b)| a == b) {
        return Some(MatchKind::Doi);
    }
    let other = ids(b);
    if ids(a).iter().any(|id| other.contains(id)) {
        return Some(MatchKind::ExternalId);
    }
    if a.bibliography.year.is_some()
        && a.bibliography.year == b.bibliography.year
        && author(a).is_some()
        && author(a) == author(b)
        && folded(&a.title) == folded(&b.title)
    {
        return Some(MatchKind::TitleAuthorYear);
    }
    // Bounded heuristic on the first 512 scalars; never truncates stored fields.
    let grams = |s: &str| {
        let c = folded(s).chars().take(512).collect::<Vec<_>>();
        c.windows(2)
            .map(|v| (v[0], v[1]))
            .collect::<std::collections::HashSet<_>>()
    };
    let x = grams(&a.title);
    let y = grams(&b.title);
    if x.len() >= 4 && y.len() >= 4 && x.intersection(&y).count() * 5 >= x.union(&y).count() * 4 {
        Some(MatchKind::SimilarTitle)
    } else {
        None
    }
}
pub fn differences(existing: &Paper, incoming: &PaperDraft) -> Vec<FieldDiff> {
    let mut before = serde_json::to_value(&existing.bibliography).expect("bibliography JSON");
    let mut after = serde_json::to_value(&incoming.bibliography).expect("bibliography JSON");
    before["title"] = existing.title.clone().into();
    after["title"] = incoming.title.clone().into();
    let mut union = existing.external_ids.clone();
    for id in &incoming.external_ids {
        if !union
            .iter()
            .any(|old| external_id(old).ok().as_ref() == Some(id))
        {
            union.push(id.clone());
        }
    }
    before["external_ids"] =
        serde_json::to_value(&existing.external_ids).expect("identifiers JSON");
    after["external_ids"] = serde_json::to_value(union).expect("identifiers JSON");
    after
        .as_object()
        .expect("object")
        .iter()
        .filter_map(|(field, value)| {
            let old = before
                .get(field)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            (old != *value).then(|| FieldDiff {
                field: field.clone(),
                before: old,
                after: value.clone(),
            })
        })
        .collect()
}
pub fn merge(
    candidate: &DuplicateCandidate,
    fields: &[String],
) -> Result<Paper, crate::ServiceError> {
    use normalize::invalid;
    let mut seen = std::collections::HashSet::new();
    let mut result = candidate.existing.clone();
    if result.deleted_at.is_some() {
        return Err(invalid("restore trash explicitly before merge"));
    }
    let mut b =
        serde_json::to_value(&result.bibliography).map_err(|_| crate::ServiceError::Unavailable)?;
    for field in fields {
        if !seen.insert(field) {
            return Err(invalid("duplicate merge field"));
        }
        let diff = candidate
            .differences
            .iter()
            .find(|d| &d.field == field)
            .ok_or_else(|| invalid("unreviewed merge field"))?;
        match field.as_str() {
            "title" => {
                result.title = serde_json::from_value(diff.after.clone())
                    .map_err(|_| invalid("invalid title diff"))?
            }
            "external_ids" => {
                result.external_ids = serde_json::from_value(diff.after.clone())
                    .map_err(|_| invalid("invalid identifier diff"))?
            }
            _ => b[field] = diff.after.clone(),
        }
    }
    result.bibliography =
        serde_json::from_value(b).map_err(|_| invalid("invalid bibliography diff"))?;
    // Validate DOI coherence but preserve every unselected existing field verbatim.
    crate::papers::validate_metadata(&mut result)?;
    Ok(result)
}
