//! #22 browser/MCP capture contract: the shared request DTO and the pure, bounded resolution
//! policy. HTML extraction (`cm_metadata::html`), DOI providers and the PDF fetcher live in
//! metadata; the daemon authorizes, composes and commits through the common #33 import path.
//!
//! Page-supplied strings (URL, HTML, meta values, JSON-LD) are untrusted data. They are never
//! interpreted as commands, local paths, endpoints, client kinds or permission requests.
//! Resolution order (DESIGN §8.2): identifiers (DOI/arXiv/PMID) -> `citation_*` (Highwire)
//! -> JSON-LD -> Dublin Core/PRISM -> DOI provider complement -> provisional title/URL.
use crate::{
    ServiceError,
    import::{ImportDestination, ImportSource, PaperDraft, RowDiagnostic, normalize},
    metadata_resolve::{Cancellation, MetadataResolution, ProviderOutcome, ProviderStatus},
    papers::normalize_doi,
};
use cm_domain::{Bibliography, ExternalId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(test)]
mod tests;

/// Byte limits of one capture request. Larger requests are rejected before any work.
pub const MAX_PAGE_URL_BYTES: usize = 4096;
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_META_TAGS: usize = 1024;
pub const MAX_META_NAME_BYTES: usize = 256;
pub const MAX_META_CONTENT_BYTES: usize = 64 * 1024;
pub const MAX_JSON_LD_BLOCKS: usize = 32;
pub const MAX_JSON_LD_BYTES: usize = 512 * 1024;
/// HTML + meta + JSON-LD + title together (well under the 8 MiB IPC request frame).
pub const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
/// PDF candidates retained per capture (explicit, page and provider candidates together).
pub const MAX_PDF_CANDIDATES: usize = 16;
pub const CAPTURE_ADAPTER: &str = "browser-capture";

/// One `<meta name|property=... content=...>` pair as read by a browser content script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaTag {
    pub name: String,
    pub content: String,
}

/// Shared Browser/MCP capture input. Unknown JSON fields (for example a page-forwarded
/// `client_kind` or `capabilities`) are ignored: authority comes only from the daemon grant.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CaptureRequest {
    /// The page the user explicitly saved. Never fetched by the daemon.
    pub page_url: String,
    /// `document.title` as seen by the browser.
    #[serde(default)]
    pub title: Option<String>,
    /// Bounded HTML snapshot (typically `<head>` plus a limited body), parsed as inert text.
    #[serde(default)]
    pub html: Option<String>,
    /// Meta tags already read by the content script (alternative or addition to `html`).
    #[serde(default)]
    pub meta: Vec<MetaTag>,
    /// Raw `application/ld+json` script contents.
    #[serde(default)]
    pub json_ld: Vec<String>,
    /// A PDF link the user explicitly chose (e.g. the page's "Download PDF" link).
    #[serde(default)]
    pub pdf_url: Option<String>,
    /// Collection/tag IDs chosen from the destination list; validated by #33 at commit.
    #[serde(default)]
    pub destination: ImportDestination,
    /// Skip the DOI provider complement (no network during preview).
    #[serde(default)]
    pub offline: bool,
}
impl CaptureRequest {
    /// Size/shape limits only; content is validated by the extractor and #33.
    pub fn validate(&self) -> Result<(), ServiceError> {
        let invalid = |m: &str| Err(ServiceError::InvalidInput(m.into()));
        if self.page_url.trim().is_empty() || self.page_url.len() > MAX_PAGE_URL_BYTES {
            return invalid("capture page URL must be 1..=4096 bytes");
        }
        if self
            .pdf_url
            .as_ref()
            .is_some_and(|u| u.len() > MAX_PAGE_URL_BYTES)
        {
            return invalid("capture PDF URL exceeds 4096 bytes");
        }
        if self
            .title
            .as_ref()
            .is_some_and(|t| t.len() > normalize::MAX_TITLE_BYTES)
        {
            return invalid("capture title exceeds 16 KiB");
        }
        if self.html.as_ref().is_some_and(|h| h.len() > MAX_HTML_BYTES) {
            return invalid("capture HTML snapshot exceeds 2 MiB");
        }
        if self.meta.len() > MAX_META_TAGS
            || self.meta.iter().any(|m| {
                m.name.len() > MAX_META_NAME_BYTES || m.content.len() > MAX_META_CONTENT_BYTES
            })
        {
            return invalid("capture meta tags exceed count or size limits");
        }
        if self.json_ld.len() > MAX_JSON_LD_BLOCKS
            || self.json_ld.iter().any(|b| b.len() > MAX_JSON_LD_BYTES)
        {
            return invalid("capture JSON-LD exceeds count or size limits");
        }
        let total = self.html.as_ref().map_or(0, String::len)
            + self.title.as_ref().map_or(0, String::len)
            + self
                .meta
                .iter()
                .map(|m| m.name.len() + m.content.len())
                .sum::<usize>()
            + self.json_ld.iter().map(String::len).sum::<usize>();
        if total > MAX_CAPTURE_BYTES {
            return invalid("capture request exceeds 4 MiB");
        }
        Ok(())
    }
}

/// Where a field or identifier came from. Earlier variants win.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataTier {
    /// Highwire Press `citation_*` meta tags.
    Highwire,
    /// JSON-LD `ScholarlyArticle` (and close schema.org types).
    JsonLd,
    /// Dublin Core / `dcterms` / PRISM meta tags.
    DublinCore,
    /// The page URL itself (doi.org, arxiv.org, pubmed) or a single unambiguous DOI link.
    PageUrl,
    /// Crossref/DataCite complement (#14).
    Provider,
    /// Document title / page URL fallback.
    Provisional,
}

/// Partial bibliographic record from one tier of page metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TierRecord {
    pub tier: Option<MetadataTier>,
    pub title: Option<String>,
    pub bibliography: Bibliography,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundIdentifier {
    /// `doi`, `arxiv` or `pmid`, canonical per #33.
    pub namespace: String,
    pub value: String,
    pub tier: MetadataTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfOrigin {
    /// Chosen by the user in the browser.
    Explicit,
    Highwire,
    JsonLd,
    /// `<link rel="alternate" type="application/pdf">`.
    Link,
    Provider,
}

/// A syntactically allowed PDF URL. Not proof of reachability, access rights or license;
/// the fetcher re-checks every hop (DNS, redirects, size, type) when the user commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturePdf {
    pub url: String,
    pub origin: PdfOrigin,
}

/// Output of the inert page extractor (`cm_metadata::html::extract`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PageMetadata {
    pub page_url: String,
    /// Public landing URL (syntactic public-web check), if the page URL qualifies.
    pub landing_url: Option<String>,
    /// `document.title`, `og:title` or `<title>`, for the provisional fallback only.
    pub document_title: Option<String>,
    /// Already canonical and validated; at most one per namespace, highest tier first.
    pub identifiers: Vec<FoundIdentifier>,
    /// Highwire, JSON-LD, Dublin Core in that order (absent tiers omitted).
    pub tiers: Vec<TierRecord>,
    pub pdf_candidates: Vec<CapturePdf>,
    pub diagnostics: Vec<RowDiagnostic>,
    /// SHA-256 of the HTML snapshot, if one was supplied.
    pub html_sha256: Option<[u8; 32]>,
}
impl PageMetadata {
    pub fn identifier(&self, namespace: &str) -> Option<&FoundIdentifier> {
        self.identifiers.iter().find(|i| i.namespace == namespace)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSource {
    pub field: String,
    pub tier: MetadataTier,
}

/// The reviewed-import input produced by a capture. It never writes anything: callers pass
/// `draft` to the common #33 preview and commit only an explicitly approved decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureResolution {
    pub page_url: String,
    pub draft: PaperDraft,
    pub identifiers: Vec<FoundIdentifier>,
    /// Selected field -> tier. Lower tiers only fill fields absent from higher ones.
    pub fields: Vec<FieldSource>,
    pub tiers_used: Vec<MetadataTier>,
    /// DOI provider lookup outcomes (empty when offline or not needed).
    pub provider_outcomes: Vec<ProviderOutcome>,
    /// The title and/or URL came only from the provisional fallback: review before saving.
    pub provisional: bool,
    pub pdf_candidates: Vec<CapturePdf>,
}

/// Prefix of the DataCite DOI that arXiv registers for every e-print.
pub fn arxiv_doi(arxiv: &str) -> Option<String> {
    let base = arxiv.rsplit_once('v').map_or(arxiv, |(head, tail)| {
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) && head.contains(['.', '/'])
        {
            head
        } else {
            arxiv
        }
    });
    normalize_doi(&format!("10.48550/arXiv.{base}")).ok()
}

/// Fields whose presence makes the page metadata "complete" enough to skip the provider.
fn complete(b: &Bibliography, title: bool) -> bool {
    title && !b.authors.is_empty() && b.year.is_some() && b.journal.is_some()
}

type Resolver<'a> = dyn FnMut(&str, &Cancellation) -> Result<MetadataResolution, ServiceError> + 'a;

/// Pure capture resolution. `resolver` performs the #14 DOI lookup (None = offline).
pub fn resolve(
    page: PageMetadata,
    resolver: Option<&mut Resolver<'_>>,
    cancel: &Cancellation,
) -> CaptureResolution {
    let mut diagnostics = page.diagnostics.clone();
    let mut bibliography = Bibliography::default();
    let mut title: Option<String> = None;
    let mut fields: Vec<FieldSource> = vec![];
    let mut tiers_used = vec![];
    let mut conflicts = vec![];
    for record in &page.tiers {
        let tier = record.tier.unwrap_or(MetadataTier::DublinCore);
        if fill(
            &mut bibliography,
            &mut title,
            &record.bibliography,
            record.title.as_deref(),
            tier,
            &mut fields,
            &mut conflicts,
        ) {
            tiers_used.push(tier);
        }
    }
    let mut external_ids: Vec<ExternalId> = page
        .identifiers
        .iter()
        .map(|i| ExternalId {
            namespace: i.namespace.clone(),
            value: i.value.clone(),
        })
        .collect();
    if let Some(doi) = page.identifier("doi") {
        bibliography.doi = Some(doi.value.clone());
        push_source(&mut fields, "doi", doi.tier);
    }
    // Provider complement: only for identifiers found on the page and only when fields are
    // missing. An arXiv-only page tries arXiv's own DataCite DOI, kept only when confirmed.
    let mut provider_outcomes = vec![];
    let mut provider_extra = None;
    let mut provider_pdfs: Vec<String> = vec![];
    let lookup = page
        .identifier("doi")
        .map(|d| (d.value.clone(), false))
        .or_else(|| {
            page.identifier("arxiv")
                .and_then(|a| arxiv_doi(&a.value))
                .map(|d| (d, true))
        });
    match (resolver, lookup) {
        (Some(resolver), Some((doi, derived))) if !complete(&bibliography, title.is_some()) => {
            match resolver(&doi, cancel) {
                Ok(resolution) => {
                    provider_outcomes = resolution.outcomes.clone();
                    let found = resolution
                        .outcomes
                        .iter()
                        .any(|o| o.status == ProviderStatus::Found);
                    for outcome in &resolution.outcomes {
                        if !matches!(
                            outcome.status,
                            ProviderStatus::Found | ProviderStatus::NotNeeded
                        ) {
                            diagnostics.push(note(
                                "doi",
                                format!(
                                    "{} lookup: {:?}; page metadata kept",
                                    outcome.provider.name(),
                                    outcome.status
                                ),
                            ));
                        }
                    }
                    if found {
                        provider_pdfs = resolution
                            .pdf_candidates
                            .iter()
                            .map(|c| c.url.clone())
                            .collect();
                        let mut provided = resolution.draft.bibliography.clone();
                        provider_extra = provided.extra_json.take();
                        if derived {
                            bibliography.doi = Some(doi.clone());
                            push_source(&mut fields, "doi", MetadataTier::Provider);
                            external_ids.push(ExternalId {
                                namespace: "doi".into(),
                                value: doi.clone(),
                            });
                        }
                        let provider_title =
                            Some(resolution.draft.title.clone()).filter(|t| !t.trim().is_empty());
                        if fill(
                            &mut bibliography,
                            &mut title,
                            &provided,
                            provider_title.as_deref(),
                            MetadataTier::Provider,
                            &mut fields,
                            &mut conflicts,
                        ) {
                            tiers_used.push(MetadataTier::Provider);
                        }
                        for id in &resolution.draft.external_ids {
                            if let Ok(id) = normalize::external_id(id)
                                && !external_ids
                                    .iter()
                                    .any(|o| o.namespace == id.namespace && o.value == id.value)
                            {
                                external_ids.push(id);
                            }
                        }
                    }
                }
                Err(e) => diagnostics.push(note("doi", format!("DOI lookup unavailable: {e}"))),
            }
        }
        (None, Some(_)) => diagnostics.push(note(
            "doi",
            "offline capture: DOI provider complement skipped".into(),
        )),
        _ => {}
    }
    let mut provisional = false;
    if title.is_none() {
        let fallback = page
            .document_title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| page.page_url.clone());
        title = Some(fallback);
        push_source(&mut fields, "title", MetadataTier::Provisional);
        provisional = true;
        diagnostics.push(note(
            "title",
            "no bibliographic title found; provisional title from the page — review before saving"
                .into(),
        ));
    }
    if bibliography.url.is_none()
        && let Some(url) = &page.landing_url
    {
        bibliography.url = Some(url.clone());
        push_source(&mut fields, "url", MetadataTier::Provisional);
    }
    if !conflicts.is_empty() {
        diagnostics.push(note(
            "capture",
            format!(
                "{} lower-priority value(s) differed and were not used (see extra_json.capture)",
                conflicts.len()
            ),
        ));
    }
    let mut pdf_candidates = page.pdf_candidates.clone();
    pdf_candidates.truncate(MAX_PDF_CANDIDATES);
    for url in provider_pdfs {
        if pdf_candidates.len() < MAX_PDF_CANDIDATES && !pdf_candidates.iter().any(|c| c.url == url)
        {
            pdf_candidates.push(CapturePdf {
                url,
                origin: PdfOrigin::Provider,
            });
        }
    }
    let mut extra = serde_json::Map::new();
    extra.insert(
        "capture".into(),
        json!({
            "page_url": page.page_url,
            "fields": fields,
            "conflicts": conflicts,
            "identifiers": page.identifiers,
        }),
    );
    if let Some(Value::Object(provided)) = provider_extra {
        extra.extend(provided);
    }
    bibliography.extra_json = Some(Value::Object(extra));
    let mut draft = PaperDraft {
        title: title.unwrap_or_default(),
        external_ids,
        bibliography,
        source: ImportSource {
            adapter: CAPTURE_ADAPTER.into(),
            locator: Some(page.page_url.clone()),
            row: None,
            sha256: page.html_sha256,
        },
        diagnostics,
    };
    sanitize(&mut draft);
    CaptureResolution {
        page_url: page.page_url,
        draft,
        identifiers: page.identifiers,
        fields,
        tiers_used,
        provider_outcomes,
        provisional,
        pdf_candidates,
    }
}

impl CaptureResolution {
    /// The common #33 preview request. Committing remains a separate explicit decision.
    pub fn request(&self, destination: ImportDestination) -> crate::import::ImportRequest {
        crate::import::ImportRequest {
            candidates: vec![self.draft.clone()],
            destination,
        }
    }
}

fn note(field: &str, message: String) -> RowDiagnostic {
    let mut message = message;
    if message.len() > 1024 {
        let mut end = 1000;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    RowDiagnostic {
        field: field.into(),
        message,
        error: false,
    }
}

fn push_source(fields: &mut Vec<FieldSource>, field: &str, tier: MetadataTier) {
    fields.retain(|f| f.field != field);
    fields.push(FieldSource {
        field: field.into(),
        tier,
    });
}

/// Copies every field absent in `into` from `from`. Records disagreements. Returns whether
/// this tier supplied at least one field.
fn fill(
    into: &mut Bibliography,
    title: &mut Option<String>,
    from: &Bibliography,
    from_title: Option<&str>,
    tier: MetadataTier,
    fields: &mut Vec<FieldSource>,
    conflicts: &mut Vec<Value>,
) -> bool {
    let mut used = false;
    let mut differ = |field: &str, kept: Value, other: Value| {
        if conflicts.len() < 32 {
            conflicts.push(json!({"field": field, "tier": tier, "kept": kept, "ignored": other}));
        }
    };
    if let Some(t) = from_title.filter(|t| !t.trim().is_empty()) {
        match title {
            None => {
                *title = Some(t.to_owned());
                push_source(fields, "title", tier);
                used = true;
            }
            Some(old) if normalize::spaces(old) != normalize::spaces(t) => {
                differ("title", json!(old), json!(t))
            }
            _ => {}
        }
    }
    macro_rules! opt {
        ($($f:ident => $name:literal),* $(,)?) => {$(
            if let Some(v) = &from.$f {
                match &into.$f {
                    None => {
                        into.$f = Some(v.clone());
                        push_source(fields, $name, tier);
                        used = true;
                    }
                    Some(old) if old != v => differ($name, json!(old), json!(v)),
                    _ => {}
                }
            }
        )*};
    }
    opt!(paper_type => "type", abstract_text => "abstract", year => "year",
        publication_date => "publication_date", journal => "journal", volume => "volume",
        issue => "issue", pages => "pages", url => "url", language => "language");
    // A date from a lower tier must agree with the selected year (one consistency group).
    if let (Some(year), Some(date)) = (into.year, &into.publication_date)
        && !date.starts_with(&format!("{year:04}"))
    {
        differ("publication_date", json!(year), json!(date));
        into.publication_date = None;
        fields.retain(|f| f.field != "publication_date");
    }
    if into.authors.is_empty() && !from.authors.is_empty() {
        into.authors = from.authors.clone();
        push_source(fields, "authors", tier);
        used = true;
    } else if !from.authors.is_empty() && from.authors.len() != into.authors.len() {
        differ(
            "authors",
            json!(into.authors.len()),
            json!(from.authors.len()),
        );
    }
    used
}

/// Keep the draft acceptable to the common normalizer without trusting page values:
/// invalid optional fields are dropped with a diagnostic, never guessed or truncated.
fn sanitize(draft: &mut PaperDraft) {
    let b = &mut draft.bibliography;
    let mut dropped: Vec<&'static str> = vec![];
    if b.doi.as_deref().is_some_and(|d| normalize_doi(d).is_err()) {
        b.doi = None;
        dropped.push("doi");
    }
    if let Some(date) = b.publication_date.clone() {
        let mut probe = PaperDraft::titled("probe");
        probe.bibliography.publication_date = Some(date);
        probe.bibliography.year = b.year;
        if normalize::normalize(&probe).is_err() {
            b.publication_date = None;
            dropped.push("publication_date");
        }
    }
    if b.year.is_some_and(|y| !(1..=9999).contains(&y)) {
        b.year = None;
        dropped.push("year");
    }
    let before = draft.external_ids.len();
    let mut seen = std::collections::HashSet::new();
    draft
        .external_ids
        .retain(|id| normalize::external_id(id).is_ok_and(|n| seen.insert((n.namespace, n.value))));
    if draft.external_ids.len() != before {
        dropped.push("external_ids");
    }
    for name in dropped {
        draft.diagnostics.push(note(
            name,
            format!("page value for {name} was invalid and was not used"),
        ));
    }
    draft.diagnostics.truncate(120);
    // Budget: shed the largest optional page-derived data first, never the identity.
    if normalize::normalize(draft).is_err() {
        for step in 0..4 {
            match step {
                0 => draft.bibliography.abstract_text = None,
                1 => {
                    if let Some(Value::Object(extra)) = &mut draft.bibliography.extra_json {
                        extra.remove("doi_resolution");
                        if let Some(Value::Object(c)) = extra.get_mut("capture") {
                            c.remove("conflicts");
                        }
                    }
                }
                2 => draft.bibliography.authors.clear(),
                _ => {
                    draft.bibliography.extra_json = None;
                    draft.diagnostics.truncate(8);
                }
            }
            if normalize::normalize(draft).is_ok() {
                draft.diagnostics.push(note(
                    "capture",
                    "page metadata exceeded the common import budget; some optional fields were omitted"
                        .into(),
                ));
                return;
            }
        }
        // Still invalid (e.g. the only title is oversized or has control characters):
        // the common preview will report the row error; nothing is fabricated here.
    }
}
