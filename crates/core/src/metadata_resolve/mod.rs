//! Read-only worker-side DOI resolution. Import preview/commit remains in #33.
use crate::{ServiceError, import::*, papers::normalize_doi, ports::ImportService};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    Crossref,
    DataCite,
}
impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Crossref => "crossref",
            Self::DataCite => "datacite",
        }
    }
}
/// Local cancellation only, not a job/IPC/auth envelope. Use a fresh token for retry.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderStatus {
    Found,
    NotFound,
    RateLimited,
    Timeout,
    Network,
    Unavailable,
    InvalidResponse,
    TooLarge,
    IdentityMismatch,
    RedirectRejected,
    Http(u16),
    Cancelled,
    NotNeeded,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOutcome {
    pub provider: Provider,
    pub status: ProviderStatus,
    pub attempts: u8,
    pub cached: bool,
    /// Retry advice only. Long Retry-After values are never shortened to retry early.
    pub retry_after_seconds: Option<u64>,
}
/// A reported public PDF URL is neither a downloaded attachment nor license evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfCandidate {
    pub url: String,
    pub provider: Provider,
    pub source: ImportSource,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    pub draft: PaperDraft,
    pub pdf_candidates: Vec<PdfCandidate>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderReply {
    pub outcome: ProviderOutcome,
    pub record: Option<ProviderRecord>,
}
/// Concrete HTTP/runtime/cache belongs to metadata, never to this port.
pub trait MetadataProvider {
    fn lookup(
        &mut self,
        provider: Provider,
        canonical_doi: &str,
        cancel: &Cancellation,
    ) -> ProviderReply;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldEvidence {
    pub field: String,
    pub provider: Provider,
    pub value: Value,
    pub selected: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataResolution {
    pub canonical_doi: String,
    /// Blank title means manual completion is required; do not invent a successful title.
    pub draft: PaperDraft,
    pub manual_add: bool,
    pub outcomes: Vec<ProviderOutcome>,
    pub fields: Vec<FieldEvidence>,
    pub pdf_candidates: Vec<PdfCandidate>,
}
impl MetadataResolution {
    /// Reuse common duplicate/key/difference policies. Saving requires a separate explicit
    /// ImportCommit. Callers may edit the draft and request a fresh preview for manual add.
    pub fn preview(
        &self,
        imports: &mut impl ImportService,
        destination: ImportDestination,
    ) -> Result<ImportPreview, ServiceError> {
        imports.preview(ImportRequest {
            candidates: vec![self.draft.clone()],
            destination,
        })
    }
}
pub struct MetadataResolveService<P> {
    provider: P,
}
impl<P: MetadataProvider> MetadataResolveService<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
    pub fn into_provider(self) -> P {
        self.provider
    }
    pub fn resolve(
        &mut self,
        input: &str,
        cancel: &Cancellation,
    ) -> Result<MetadataResolution, ServiceError> {
        if input.len() > 4096 {
            return Err(ServiceError::InvalidInput("DOI exceeds 4096 bytes".into()));
        }
        let doi = normalize_doi(input)?;
        let mut draft = PaperDraft::default();
        draft.bibliography.doi = Some(doi.clone());
        draft.source = ImportSource {
            adapter: "doi-resolve".into(),
            locator: Some(doi.clone()),
            ..Default::default()
        };
        let mut result = MetadataResolution {
            canonical_doi: doi.clone(),
            draft,
            manual_add: true,
            outcomes: vec![],
            fields: vec![],
            pdf_candidates: vec![],
        };
        let mut sources = vec![];
        let mut raw_ids = vec![];
        let mut selected = serde_json::Map::new();
        let mut any_record = false;
        for provider in [Provider::Crossref, Provider::DataCite] {
            let mut reply = if cancel.is_cancelled() {
                ProviderReply {
                    outcome: ProviderOutcome {
                        provider,
                        status: ProviderStatus::Cancelled,
                        attempts: 0,
                        cached: false,
                        retry_after_seconds: None,
                    },
                    record: None,
                }
            } else if provider == Provider::DataCite && complete(&selected, &result.pdf_candidates)
            {
                ProviderReply {
                    outcome: ProviderOutcome {
                        provider,
                        status: ProviderStatus::NotNeeded,
                        attempts: 0,
                        cached: false,
                        retry_after_seconds: None,
                    },
                    record: None,
                }
            } else {
                self.provider.lookup(provider, &doi, cancel)
            };
            // Defend the port boundary too; request and ALL result DOI representations must agree.
            if let Some(record) = &reply.record {
                let ids = record
                    .draft
                    .bibliography
                    .doi
                    .iter()
                    .map(String::as_str)
                    .chain(
                        record
                            .draft
                            .external_ids
                            .iter()
                            .filter(|id| id.namespace.eq_ignore_ascii_case("doi"))
                            .map(|id| id.value.as_str()),
                    )
                    .collect::<Vec<_>>();
                if reply.outcome.provider != provider
                    || reply.outcome.status != ProviderStatus::Found
                    || ids.is_empty()
                    || ids
                        .iter()
                        .any(|id| normalize_doi(id).ok().as_ref() != Some(&doi))
                {
                    reply.outcome.status = ProviderStatus::IdentityMismatch;
                    reply.record = None;
                } else if record
                    .draft
                    .external_ids
                    .iter()
                    .any(|id| normalize::external_id(id).is_err())
                {
                    reply.outcome.status = ProviderStatus::InvalidResponse;
                    reply.record = None;
                }
            }
            if let Some(record) = reply.record {
                any_record = true;
                sources.push(serde_json::json!({"source":record.draft.source, "unmapped":record.draft.bibliography.extra_json}));
                raw_ids.extend(record.draft.external_ids.clone());
                let mut fields = serde_json::to_value(&record.draft.bibliography)
                    .expect("bibliography JSON")
                    .as_object()
                    .expect("object")
                    .clone();
                fields.remove("extra_json");
                fields.remove("citation_key");
                fields.insert("title".into(), record.draft.title.clone().into());
                // Date and year are one consistency group. Never combine conflicting years.
                let date_conflict = selected
                    .get("year")
                    .zip(fields.get("year"))
                    .is_some_and(|(a, b)| a != b);
                for (field, value) in fields {
                    if !present(&value) {
                        continue;
                    }
                    let choose = !selected.contains_key(&field)
                        && !(field == "publication_date" && date_conflict);
                    result.fields.push(FieldEvidence {
                        field: field.clone(),
                        provider,
                        value: value.clone(),
                        selected: choose,
                    });
                    if choose {
                        selected.insert(field, value);
                    }
                }
                for id in record.draft.external_ids {
                    let normalized = normalize::external_id(&id)?;
                    if !result
                        .draft
                        .external_ids
                        .iter()
                        .any(|old| normalize::external_id(old).ok().as_ref() == Some(&normalized))
                    {
                        result.draft.external_ids.push(id);
                    }
                }
                result.draft.diagnostics.extend(record.draft.diagnostics);
                for candidate in record.pdf_candidates {
                    if !result.pdf_candidates.iter().any(|c| c.url == candidate.url)
                        && result.pdf_candidates.len() < 32
                    {
                        result.pdf_candidates.push(candidate);
                    }
                }
            }
            result.outcomes.push(reply.outcome);
        }
        result.draft.title = selected
            .remove("title")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        selected.insert("doi".into(), doi.into());
        result.draft.bibliography =
            serde_json::from_value(Value::Object(selected)).expect("selected bibliography fields");
        result.manual_add = !any_record || result.draft.title.trim().is_empty();
        for outcome in &result.outcomes {
            if !matches!(
                outcome.status,
                ProviderStatus::Found | ProviderStatus::NotNeeded
            ) {
                result.draft.diagnostics.push(RowDiagnostic {
                    field: "doi".into(),
                    message: format!(
                        "{}: {:?}; metadata may be completed manually",
                        outcome.provider.name(),
                        outcome.status
                    ),
                    error: false,
                });
            }
        }
        result.draft.bibliography.extra_json = Some(
            serde_json::json!({"doi_resolution": {"sources": sources, "fields": result.fields, "raw_external_ids": raw_ids}}),
        );
        if normalize::validate_limits(&result.draft).is_err() {
            result.draft = PaperDraft::default();
            result.draft.bibliography.doi = Some(result.canonical_doi.clone());
            result.draft.source = ImportSource {
                adapter: "doi-resolve".into(),
                locator: Some(result.canonical_doi.clone()),
                ..Default::default()
            };
            result.draft.diagnostics.push(RowDiagnostic { field:"doi".into(), message:"Combined provider metadata exceeds the common preview budget; complete metadata manually".into(), error:false });
            result.fields.clear();
            result.pdf_candidates.clear();
            result.manual_add = true;
            for outcome in &mut result.outcomes {
                if outcome.status == ProviderStatus::Found {
                    outcome.status = ProviderStatus::TooLarge;
                }
            }
        }
        Ok(result)
    }
}
fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(v) => !v.trim().is_empty(),
        Value::Array(v) => !v.is_empty(),
        _ => true,
    }
}
fn complete(fields: &serde_json::Map<String, Value>, pdfs: &[PdfCandidate]) -> bool {
    [
        "title",
        "authors",
        "year",
        "publication_date",
        "type",
        "abstract",
        "journal",
        "volume",
        "issue",
        "pages",
        "url",
        "language",
    ]
    .iter()
    .all(|f| fields.get(*f).is_some_and(present))
        && !pdfs.is_empty()
}

#[cfg(test)]
mod tests;
