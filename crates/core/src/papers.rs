//! Synchronous worker-side manual paper CRUD. Import/dedup orchestration belongs to #33.
use crate::{ServiceError, ports::PaperRepository};
use cm_domain::{Bibliography, ExternalId, Paper, PaperId};

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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

/// #27 JSON merge-patch of a paper's bibliographic fields (REST `PATCH /papers/{id}` and the
/// body of `POST /papers`). An absent field is unchanged, `null` clears an optional field and
/// a value replaces it. Unknown fields are rejected. The daemon applies it on the library
/// worker in the same step as the read, so a write-only token never receives the record.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ids: Option<Vec<ExternalId>>,
    #[serde(rename = "type", default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_type: Option<Option<String>>,
    #[serde(rename = "abstract", default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstract_text: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<Option<i32>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_date: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation_key: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<cm_domain::Contributor>>,
    #[serde(default, deserialize_with = "present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_json: Option<Option<serde_json::Value>>,
}
/// A present field (even `null`) is `Some`; `#[serde(default)]` makes an absent one `None`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer).map(Some)
}
impl PaperPatch {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// Applies the patch to a stored paper (ID and lifecycle timestamps are untouched).
    pub fn apply(self, paper: &mut Paper) {
        if let Some(title) = self.title {
            paper.title = title;
        }
        if let Some(ids) = self.external_ids {
            paper.external_ids = ids;
        }
        let b = &mut paper.bibliography;
        if let Some(authors) = self.authors {
            b.authors = authors;
        }
        for (patch, field) in [
            (self.paper_type, &mut b.paper_type),
            (self.abstract_text, &mut b.abstract_text),
            (self.publication_date, &mut b.publication_date),
            (self.journal, &mut b.journal),
            (self.volume, &mut b.volume),
            (self.issue, &mut b.issue),
            (self.pages, &mut b.pages),
            (self.doi, &mut b.doi),
            (self.url, &mut b.url),
            (self.language, &mut b.language),
            (self.citation_key, &mut b.citation_key),
        ] {
            if let Some(value) = patch {
                *field = value;
            }
        }
        if let Some(year) = self.year {
            b.year = year;
        }
        if let Some(extra) = self.extra_json {
            b.extra_json = extra;
        }
    }
    /// A new paper from a patch over an empty record; the title is required.
    pub fn into_new_paper(self) -> Result<NewPaper, ServiceError> {
        if self.title.is_none() {
            return Err(ServiceError::InvalidInput("title is required".into()));
        }
        let mut paper = Paper {
            id: PaperId(uuid::Uuid::nil()),
            title: String::new(),
            external_ids: Vec::new(),
            bibliography: Bibliography::default(),
            created_at: None,
            updated_at: None,
            deleted_at: None,
        };
        self.apply(&mut paper);
        Ok(NewPaper {
            title: paper.title,
            external_ids: paper.external_ids,
            bibliography: paper.bibliography,
        })
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

// Share wrapper recognition with the persisted legacy-spelling witness.
fn doi_suffix(raw: &str) -> (&str, bool) {
    let value = raw.trim();
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
            return (value[prefix.len()..].trim(), prefix != "doi:");
        }
    }
    (value, false)
}

/// One DOI normalization point shared by manual persistence and import adapters.
pub fn normalize_doi(raw: &str) -> Result<String, ServiceError> {
    let (mut value, resolver) = doi_suffix(raw);
    let mut decoded;
    // Resolver URLs decode their suffix exactly once; raw DOI percent escapes remain literal.
    // The registrant separator must be an actual slash, not an encoded path separator.
    if resolver {
        value = value.split(['?', '#']).next().unwrap_or(value);
        let (head, tail) = value
            .split_once('/')
            .ok_or_else(|| ServiceError::InvalidInput("invalid DOI URL".into()))?;
        let mut bytes = Vec::with_capacity(tail.len());
        let input = tail.as_bytes();
        let mut i = 0;
        while i < input.len() {
            if input[i] == b'%' {
                let hex = tail
                    .get(i + 1..i + 3)
                    .ok_or_else(|| ServiceError::InvalidInput("invalid DOI URL escape".into()))?;
                bytes
                    .push(u8::from_str_radix(hex, 16).map_err(|_| {
                        ServiceError::InvalidInput("invalid DOI URL escape".into())
                    })?);
                i += 3;
            } else {
                bytes.push(input[i]);
                i += 1;
            }
        }
        decoded = format!(
            "{head}/{}",
            String::from_utf8(bytes)
                .map_err(|_| ServiceError::InvalidInput("invalid DOI URL UTF-8".into()))?
        );
        value = &mut decoded;
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

/// Collect every valid DOI without guessing how an unwrapped percent literal arose.
pub fn doi_forms(b: &Bibliography, ids: &[ExternalId]) -> std::collections::HashSet<String> {
    b.doi
        .iter()
        .map(String::as_str)
        .chain(
            ids.iter()
                .filter(|id| id.namespace.eq_ignore_ascii_case("doi"))
                .map(|id| id.value.as_str()),
        )
        .filter_map(|raw| normalize_doi(raw).ok())
        .collect()
}

/// Adapters must call this even when a caller bypasses PaperService.
pub fn validate_metadata(paper: &mut Paper) -> Result<(), ServiceError> {
    validate_metadata_inner(paper, None)
}

/// Only adapters holding a freshly read persisted record may use this compatibility path.
/// An unchanged stored DOI plus a retained raw resolver ID proves the old normalization
/// spelling; new inputs and explicit DOI replacements still use strict normalization.
pub fn validate_metadata_update(paper: &mut Paper, existing: &Paper) -> Result<(), ServiceError> {
    validate_metadata_inner(paper, Some(existing))
}

fn validate_metadata_inner(
    paper: &mut Paper,
    existing: Option<&Paper>,
) -> Result<(), ServiceError> {
    let legacy = existing.and_then(|old| {
        let stored = old.bibliography.doi.as_deref()?;
        if paper.bibliography.doi.as_deref() != Some(stored) {
            return None;
        }
        old.external_ids.iter().find_map(|id| {
            let (suffix, resolver) = doi_suffix(&id.value);
            (id.namespace.eq_ignore_ascii_case("doi")
                && resolver
                && paper.external_ids.contains(id)
                && suffix.to_ascii_lowercase() == stored)
                .then(|| (stored.to_owned(), normalize_doi(&id.value).ok()))
        })
    });
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
            let normalized = match normalize_doi(&id.value) {
                Ok(value) => value,
                Err(error) => {
                    // Old CRUD accepted malformed URL escapes. Preserve only an unchanged
                    // witnessed identifier; never grant that exception to new input.
                    let inherited = existing.is_some_and(|old| old.external_ids.contains(id));
                    let (suffix, resolver) = doi_suffix(&id.value);
                    if inherited
                        && resolver
                        && legacy
                            .as_ref()
                            .is_some_and(|(stored, _)| suffix.to_ascii_lowercase() == *stored)
                    {
                        legacy.as_ref().expect("checked legacy witness").0.clone()
                    } else {
                        return Err(error);
                    }
                }
            };
            let normalized = if let Some((stored, Some(canonical))) = &legacy {
                if &normalized == canonical {
                    stored.clone()
                } else {
                    normalized
                }
            } else {
                normalized
            };
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
    fn paper_patch_distinguishes_absent_null_and_value_and_round_trips() {
        let patch: PaperPatch =
            serde_json::from_str(r#"{"title":"New","journal":null,"year":2020}"#).unwrap();
        assert_eq!(patch.title.as_deref(), Some("New"));
        assert_eq!(patch.journal, Some(None));
        assert_eq!(patch.year, Some(Some(2020)));
        assert_eq!(patch.doi, None);
        let wire = serde_json::to_string(&patch).unwrap();
        assert_eq!(serde_json::from_str::<PaperPatch>(&wire).unwrap(), patch);
        assert!(serde_json::from_str::<PaperPatch>(r#"{"titel":"x"}"#).is_err());
        let untitled = PaperPatch {
            journal: Some(Some("J".into())),
            ..PaperPatch::default()
        }
        .into_new_paper();
        assert!(matches!(untitled, Err(ServiceError::InvalidInput(_))));
        let new = PaperPatch {
            title: Some("T".into()),
            journal: Some(Some("J".into())),
            doi: Some(Some("10.1/x".into())),
            ..PaperPatch::default()
        }
        .into_new_paper()
        .unwrap();
        let mut stored = Paper {
            id: PaperId(uuid::Uuid::from_u128(1)),
            title: new.title,
            external_ids: new.external_ids,
            bibliography: new.bibliography,
            created_at: Some("c".into()),
            updated_at: None,
            deleted_at: None,
        };
        patch.apply(&mut stored);
        assert_eq!(stored.title, "New");
        assert_eq!(stored.bibliography.journal, None);
        assert_eq!(stored.bibliography.year, Some(2020));
        assert_eq!(stored.bibliography.doi.as_deref(), Some("10.1/x"));
        assert_eq!(stored.created_at.as_deref(), Some("c"));
        assert!(PaperPatch::default().is_empty());
    }

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
