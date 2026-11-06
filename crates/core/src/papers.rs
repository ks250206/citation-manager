//! Synchronous worker-side manual paper CRUD. Import/dedup orchestration belongs to #33.
use crate::{ServiceError, ports::PaperRepository};
use cm_domain::{Bibliography, ExternalId, Paper, PaperId};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewPaper {
    pub title: String,
    pub external_ids: Vec<ExternalId>,
    pub bibliography: Bibliography,
}

impl NewPaper {
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PaperVisibility {
    #[default]
    Active,
    Deleted,
    All,
}

/// Stable keyset order is internal UUID ascending. New inserts never shift offsets.
/// Separate calls see the latest committed state, not a long-lived snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperList {
    pub visibility: PaperVisibility,
    pub after: Option<PaperId>,
    pub limit: u32,
}
impl Default for PaperList {
    fn default() -> Self {
        Self {
            visibility: PaperVisibility::Active,
            after: None,
            limit: 50,
        }
    }
}
impl PaperList {
    pub fn validate(&self) -> Result<(), ServiceError> {
        if !(1..=500).contains(&self.limit) {
            return Err(ServiceError::InvalidInput("limit must be 1..=500".into()));
        }
        Ok(())
    }
}

/// Additive port: #7 PaperRepository remains usable by existing lightweight substitutes.
pub trait PaperLibrary: PaperRepository {
    fn create(&mut self, input: NewPaper) -> Result<Paper, ServiceError>;
    /// Full replacement of bibliographic fields, authors and external IDs. Lifecycle
    /// timestamps are adapter-owned. Does not change attachments or collection/tag links.
    fn update(&mut self, paper: &Paper) -> Result<Paper, ServiceError>;
    fn list(&self, query: &PaperList) -> Result<Vec<Paper>, ServiceError>;
    fn get_including_deleted(&self, id: PaperId) -> Result<Option<Paper>, ServiceError>;
    fn set_deleted(&mut self, id: PaperId, deleted: bool) -> Result<Paper, ServiceError>;
}

pub struct PaperService<R> {
    repository: R,
}
impl<R: PaperLibrary> PaperService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
    pub fn into_repository(self) -> R {
        self.repository
    }
    pub fn create(&mut self, input: NewPaper) -> Result<Paper, ServiceError> {
        self.repository.create(input)
    }
    pub fn get(&self, id: PaperId) -> Result<Paper, ServiceError> {
        self.repository.get(id)?.ok_or(ServiceError::NotFound)
    }
    pub fn update(&mut self, paper: &Paper) -> Result<Paper, ServiceError> {
        self.repository.update(paper)
    }
    pub fn list(&self, query: &PaperList) -> Result<Vec<Paper>, ServiceError> {
        self.repository.list(query)
    }
    pub fn soft_delete(&mut self, id: PaperId) -> Result<Paper, ServiceError> {
        self.repository.set_deleted(id, true)
    }
    pub fn restore(&mut self, id: PaperId) -> Result<Paper, ServiceError> {
        self.repository.set_deleted(id, false)
    }
}

/// One DOI normalization point shared by manual persistence and later import adapters.
pub fn normalize_doi(raw: &str) -> Result<String, ServiceError> {
    let mut value = raw.trim();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "https://www.doi.org/",
        "http://www.doi.org/",
        "www.doi.org/",
        "doi.org/",
        "dx.doi.org/",
        "doi:",
    ] {
        if value
            .get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
        {
            value = value[prefix.len()..].trim();
            break;
        }
    }
    let valid = value.split_once('/').is_some_and(|(head, tail)| {
        head.strip_prefix("10.").is_some_and(|registrant| {
            registrant
                .split('.')
                .all(|digits| !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()))
        }) && !tail.is_empty()
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
    });
    if !valid {
        return Err(ServiceError::InvalidInput("invalid DOI".into()));
    }
    Ok(value.to_ascii_lowercase())
}

/// Adapters must call this even when a caller bypasses PaperService.
pub fn validate_metadata(paper: &mut Paper) -> Result<(), ServiceError> {
    let invalid = |message: &str| ServiceError::InvalidInput(message.into());
    if paper.title.trim().is_empty() {
        return Err(invalid("title is required"));
    }
    let b = &mut paper.bibliography;
    if b.paper_type.as_ref().is_some_and(|v| v.trim().is_empty())
        || b.citation_key.as_ref().is_some_and(|v| v.trim().is_empty())
    {
        return Err(invalid("type and citation key cannot be blank"));
    }
    for author in &b.authors {
        if author.role.trim().is_empty()
            || ![
                &author.given_name,
                &author.family_name,
                &author.literal_name,
            ]
            .iter()
            .any(|v| v.as_ref().is_some_and(|s| !s.trim().is_empty()))
        {
            return Err(invalid("contributor needs a role and name"));
        }
    }
    let mut doi = b.doi.as_deref().map(normalize_doi).transpose()?;
    let mut external_ids = std::collections::HashSet::new();
    for id in &paper.external_ids {
        if id.namespace.trim().is_empty() || id.value.trim().is_empty() {
            return Err(invalid("external ID namespace and value are required"));
        }
        if !external_ids.insert((&id.namespace, &id.value)) {
            return Err(invalid("duplicate external ID within a paper"));
        }
        if id.namespace.eq_ignore_ascii_case("doi") {
            let normalized = normalize_doi(&id.value)?;
            if doi.as_ref().is_some_and(|d| *d != normalized) {
                return Err(invalid("conflicting DOIs within a paper"));
            }
            doi = Some(normalized);
        }
    }
    b.doi = doi;
    b.paper_type.get_or_insert_with(|| "journal-article".into());
    b.extra_json.get_or_insert_with(|| serde_json::json!({}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doi_resolver_wrappers_and_registrant_subdivisions_are_normalized() {
        for input in [
            " https://www.doi.org/10.1000/ABC ",
            "http://www.doi.org/10.1000/ABC",
            "www.doi.org/10.1000/ABC",
            "DOI.ORG/10.1000/ABC",
            "dx.doi.org/10.1000/ABC",
            "https://dx.doi.org/10.1000/ABC",
            "doi:10.1000/ABC",
        ] {
            assert_eq!(normalize_doi(input).unwrap(), "10.1000/abc", "{input}");
        }
        assert_eq!(normalize_doi("10.1000.10/ABC").unwrap(), "10.1000.10/abc");
        assert_eq!(normalize_doi("10.23/ABC").unwrap(), "10.23/abc");
        // Raw suffix escapes are identifiers, not URL-decoding instructions.
        assert_eq!(normalize_doi("10.1000/a%2Fb").unwrap(), "10.1000/a%2fb");
    }

    #[test]
    fn doi_normalization_rejects_malformed_prefixes_and_unparsed_urls() {
        for input in [
            "10./x",
            "10..1/x",
            "10.1000./x",
            "10.a/x",
            "11.1000/x",
            "10.1000/",
            "10.1000/a b",
            "10.1000/a\0b",
            "https://doi.org.evil/10.1000/x",
            "https://doi.org/10.1000%2Fx",
        ] {
            assert!(
                matches!(normalize_doi(input), Err(ServiceError::InvalidInput(_))),
                "{input:?}"
            );
        }
    }
}
