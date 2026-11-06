//! Strict singleton provider schemas; no search, file download, or library access.
mod crossref;
mod datacite;
use cm_core::{
    import::{ImportSource, PaperDraft, normalize},
    metadata_resolve::*,
};
use cm_domain::{Contributor, ExternalId};
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(crate) fn parse(
    provider: Provider,
    doi: &str,
    endpoint: &str,
    body: &[u8],
) -> Result<ProviderRecord, ProviderStatus> {
    let source = ImportSource {
        adapter: provider.name().into(),
        locator: Some(endpoint.into()),
        sha256: Some(Sha256::digest(body).into()),
        ..Default::default()
    };
    let mut record = match provider {
        Provider::Crossref => crossref::parse(body, source),
        Provider::DataCite => datacite::parse(body, source),
    }
    .map_err(|_| ProviderStatus::InvalidResponse)?;
    if record
        .draft
        .bibliography
        .doi
        .as_deref()
        .and_then(|d| cm_core::papers::normalize_doi(d).ok())
        .as_deref()
        != Some(doi)
        || record
            .draft
            .external_ids
            .iter()
            .filter(|id| id.namespace.eq_ignore_ascii_case("doi"))
            .any(|id| cm_core::papers::normalize_doi(&id.value).ok().as_deref() != Some(doi))
    {
        return Err(ProviderStatus::IdentityMismatch);
    }
    for id in &record.draft.external_ids {
        normalize::external_id(id).map_err(|_| ProviderStatus::InvalidResponse)?;
    }
    let raw_ids = record.draft.external_ids.clone();
    let mut seen = std::collections::HashSet::new();
    record.draft.external_ids.retain(|id| {
        normalize::external_id(id).is_ok_and(|id| seen.insert((id.namespace, id.value)))
    });
    if let Some(extra) = record
        .draft
        .bibliography
        .extra_json
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
    {
        extra.insert("raw_external_ids".into(), serde_json::json!(raw_ids));
    }
    // Use the common validator without making missing title unusable as a partial record.
    let mut check = record.draft.clone();
    if check.title.trim().is_empty() {
        check.title = "Partial provider record".into();
    }
    normalize::normalize(&check).map_err(|_| ProviderStatus::InvalidResponse)?;
    Ok(record)
}
fn raw_doi(draft: &mut PaperDraft, value: String) {
    draft.bibliography.doi = Some(value.clone());
    draft.external_ids.push(ExternalId {
        namespace: "doi".into(),
        value,
    });
}
fn landing_url(draft: &mut PaperDraft, raw: Option<String>) {
    if let Some(url) = raw {
        if public_url(&url).is_some() {
            draft.bibliography.url = Some(url);
        } else {
            if let Some(extra) = draft
                .bibliography
                .extra_json
                .as_mut()
                .and_then(serde_json::Value::as_object_mut)
            {
                extra.insert("unusable_landing_url".into(), url.into());
            }
            draft.diagnostics.push(cm_core::import::RowDiagnostic {
                field: "url".into(),
                message: "Landing URL excluded by public-web candidate policy; original retained in source metadata".into(),
                error: false,
            });
        }
    }
}
fn pdf(url: &str, provider: Provider, source: &ImportSource) -> Option<PdfCandidate> {
    public_url(url).map(|url| PdfCandidate {
        url,
        provider,
        source: source.clone(),
    })
}
/// Syntactic public-web candidate guard only: no DNS, reachability or license claim.
pub fn public_url(input: &str) -> Option<String> {
    if input.len() > 4096 || input.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let url = Url::parse(input).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port(), None | Some(80) | Some(443))
    {
        return None;
    }
    let host = url
        .host_str()?
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let global = match ip {
            std::net::IpAddr::V4(v) => {
                let [a, b, c, _] = v.octets();
                !(a == 0
                    || a == 10
                    || a == 127
                    || a >= 224
                    || (a == 169 && b == 254)
                    || (a == 172 && (16..=31).contains(&b))
                    || (a == 192 && (b == 168 || b == 0))
                    || (a == 100 && (64..=127).contains(&b))
                    || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                    || (a == 203 && b == 0 && c == 113))
            }
            std::net::IpAddr::V6(v) => {
                v.segments()[0] & 0xe000 == 0x2000
                    && !(v.segments()[0] == 0x2001 && v.segments()[1] == 0x0db8)
            }
        };
        if !global {
            return None;
        }
    } else if !host.contains('.')
        || host.ends_with('.')
        || ["localhost", "local", "internal", "invalid", "test"]
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
    {
        return None;
    }
    Some(url.into())
}
#[derive(Deserialize)]
struct CrossrefPerson {
    given: Option<String>,
    family: Option<String>,
    name: Option<String>,
    #[serde(rename = "ORCID")]
    orcid: Option<String>,
}
impl CrossrefPerson {
    fn contributor(self, role: &str) -> Contributor {
        Contributor {
            given_name: self.given,
            family_name: self.family,
            literal_name: self.name,
            orcid: self.orcid,
            role: role.into(),
        }
    }
}
