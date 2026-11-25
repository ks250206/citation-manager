use super::*;
use crate::html::tests::{fixture, request};
use cm_core::{
    import::{ImportSource, PaperDraft},
    metadata_resolve::{
        MetadataProvider, PdfCandidate, Provider, ProviderOutcome, ProviderRecord, ProviderReply,
        ProviderStatus,
    },
};
use cm_domain::{Contributor, ExternalId};
use std::collections::HashMap;

/// Offline provider: canonical DOI -> Crossref record; records every lookup.
#[derive(Default)]
struct Fake {
    records: HashMap<String, PaperDraft>,
    calls: Vec<String>,
}
impl MetadataProvider for Fake {
    fn lookup(&mut self, provider: Provider, doi: &str, _: &Cancellation) -> ProviderReply {
        self.calls.push(format!("{}:{doi}", provider.name()));
        let found = (provider == Provider::Crossref)
            .then(|| self.records.get(doi).cloned())
            .flatten();
        ProviderReply {
            outcome: ProviderOutcome {
                provider,
                status: if found.is_some() {
                    ProviderStatus::Found
                } else {
                    ProviderStatus::NotFound
                },
                attempts: 1,
                cached: false,
                retry_after_seconds: None,
            },
            record: found.map(|draft| ProviderRecord {
                pdf_candidates: vec![
                    PdfCandidate {
                        url: format!("https://provider.example.org/{}.pdf", draft.title.len()),
                        provider,
                        source: ImportSource::default(),
                    },
                    PdfCandidate {
                        url: "http://plain.example.org/x.pdf".into(),
                        provider,
                        source: ImportSource::default(),
                    },
                ],
                draft,
            }),
        }
    }
}
fn record(doi: &str, title: &str) -> PaperDraft {
    let mut d = PaperDraft::titled(title);
    d.bibliography.doi = Some(doi.into());
    d.external_ids.push(ExternalId {
        namespace: "doi".into(),
        value: doi.into(),
    });
    d.bibliography.journal = Some("Provider Journal".into());
    d.bibliography.year = Some(2024);
    d.bibliography.issue = Some("99".into());
    d.bibliography.authors.push(Contributor {
        given_name: Some("Pro".into()),
        family_name: Some("Vider".into()),
        literal_name: None,
        orcid: None,
        role: "author".into(),
    });
    d
}
fn source(r: &CaptureResolution, field: &str) -> Option<MetadataTier> {
    r.fields.iter().find(|f| f.field == field).map(|f| f.tier)
}

#[test]
fn identifier_then_page_tiers_then_provider_complement() {
    let mut service = MetadataResolveService::new(Fake {
        records: [(
            "10.9999/jse.2024.0012".into(),
            record("10.9999/jse.2024.0012", "Provider title must not win"),
        )]
        .into(),
        ..Default::default()
    });
    let mut req = request(
        "https://journal.example.org/article/0012",
        &fixture("publisher.html"),
    );
    req.destination = Default::default();
    let r = resolve(
        &req,
        &FetchPolicy::default(),
        Some(&mut service),
        &Cancellation::default(),
    )
    .unwrap();
    // Page metadata is complete for title/authors/year/journal -> no provider call.
    assert!(service.into_provider().calls.is_empty());
    assert_eq!(r.draft.title, "Lattice Methods for Sparse Citation Graphs");
    assert_eq!(source(&r, "title"), Some(MetadataTier::Highwire));
    assert_eq!(source(&r, "doi"), Some(MetadataTier::Highwire));
    assert_eq!(
        r.draft.bibliography.doi.as_deref(),
        Some("10.9999/jse.2024.0012")
    );
    assert!(!r.provisional);
    assert_eq!(r.tiers_used[0], MetadataTier::Highwire);
    assert_eq!(r.draft.source.adapter, "browser-capture");
    // Lower tiers that disagree are recorded, not applied.
    let extra = r.draft.bibliography.extra_json.as_ref().unwrap();
    assert!(!extra["capture"]["conflicts"].as_array().unwrap().is_empty());
    cm_core::import::normalize::normalize(&r.draft).unwrap();

    // The same page without journal metadata triggers the DOI complement, which only fills
    // missing fields (journal/issue) and never replaces page values.
    let html = fixture("publisher.html").replace("citation_journal_title", "x_journal");
    let mut service = MetadataResolveService::new(Fake {
        records: [(
            "10.9999/jse.2024.0012".into(),
            record("10.9999/jse.2024.0012", "Provider title must not win"),
        )]
        .into(),
        ..Default::default()
    });
    let r = resolve(
        &request("https://journal.example.org/article/0012", &html),
        &FetchPolicy::default(),
        Some(&mut service),
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(
        service.into_provider().calls,
        [
            "crossref:10.9999/jse.2024.0012",
            "datacite:10.9999/jse.2024.0012"
        ]
    );
    assert_eq!(r.draft.title, "Lattice Methods for Sparse Citation Graphs");
    assert_eq!(
        r.draft.bibliography.journal.as_deref(),
        Some("Provider Journal")
    );
    assert_eq!(source(&r, "journal"), Some(MetadataTier::Provider));
    assert_eq!(r.draft.bibliography.issue.as_deref(), Some("3"));
    assert_eq!(r.draft.bibliography.authors.len(), 3);
    assert!(r.tiers_used.contains(&MetadataTier::Provider));
    // Provider PDF candidates follow page ones; plain-HTTP ones are filtered by the policy.
    let origins: Vec<_> = r.pdf_candidates.iter().map(|c| c.origin).collect();
    assert_eq!(
        origins,
        [PdfOrigin::Highwire, PdfOrigin::Link, PdfOrigin::Provider]
    );
    assert!(
        r.draft
            .diagnostics
            .iter()
            .any(|d| d.message.contains("provider PDF link"))
    );
    cm_core::import::normalize::normalize(&r.draft).unwrap();
}

#[test]
fn arxiv_uses_confirmed_datacite_doi_and_pubmed_stays_offline() {
    let doi = "10.48550/arxiv.2401.01234";
    let mut fake = Fake::default();
    fake.records
        .insert(doi.into(), record(doi, "Provider arXiv"));
    let mut service = MetadataResolveService::new(fake);
    let r = resolve(
        &request("https://arxiv.org/abs/2401.01234v2", &fixture("arxiv.html")),
        &FetchPolicy::default(),
        Some(&mut service),
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(service.into_provider().calls[0], format!("crossref:{doi}"));
    assert_eq!(r.draft.bibliography.doi.as_deref(), Some(doi));
    assert_eq!(source(&r, "doi"), Some(MetadataTier::Provider));
    assert!(
        r.draft
            .external_ids
            .iter()
            .any(|i| i.namespace == "arxiv" && i.value == "2401.01234v2")
    );
    assert_eq!(r.draft.title, "Synthetic Preprint on Bounded Parsers");
    cm_core::import::normalize::normalize(&r.draft).unwrap();

    // Unconfirmed derived DOI is not added.
    let mut service = MetadataResolveService::new(Fake::default());
    let r = resolve(
        &request("https://arxiv.org/abs/2401.01234v2", &fixture("arxiv.html")),
        &FetchPolicy::default(),
        Some(&mut service),
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(r.draft.bibliography.doi, None);
    assert!(
        r.draft
            .diagnostics
            .iter()
            .any(|d| d.message.contains("NotFound"))
    );

    // PubMed: no PMID provider, no network call.
    let mut service = MetadataResolveService::new(Fake::default());
    let r = resolve(
        &request(
            "https://pubmed.ncbi.nlm.nih.gov/31234567/",
            &fixture("pubmed.html"),
        ),
        &FetchPolicy::default(),
        Some(&mut service),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(service.into_provider().calls.is_empty());
    assert!(
        r.draft
            .external_ids
            .iter()
            .any(|i| i.namespace == "pmid" && i.value == "31234567")
    );
    assert_eq!(
        r.draft.bibliography.journal.as_deref(),
        Some("Example Medical Journal")
    );
}

#[test]
fn jsonld_dublin_core_and_provisional_fallbacks() {
    let policy = FetchPolicy::default();
    let r = resolve::<Fake>(
        &request(
            "https://bulletin.example.org/e1001",
            &fixture("jsonld.html"),
        ),
        &policy,
        None,
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(r.draft.title, "Graph-Shaped Metadata Without Meta Tags");
    assert_eq!(source(&r, "title"), Some(MetadataTier::JsonLd));
    assert!(
        r.draft
            .diagnostics
            .iter()
            .any(|d| d.message.contains("offline"))
    );
    cm_core::import::normalize::normalize(&r.draft).unwrap();

    let r = resolve::<Fake>(
        &request(
            "https://repository.example.org/record/77",
            &fixture("dublin-core.html"),
        ),
        &policy,
        None,
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(source(&r, "title"), Some(MetadataTier::DublinCore));
    assert_eq!(
        r.draft.bibliography.authors[0].family_name.as_deref(),
        Some("Kowalski")
    );

    let mut service = MetadataResolveService::new(Fake::default());
    let r = resolve(
        &request(
            "https://blog.example.org/post",
            &fixture("no-metadata.html"),
        ),
        &policy,
        Some(&mut service),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(service.into_provider().calls.is_empty());
    assert!(r.provisional);
    assert_eq!(r.draft.title, "A Blog Post About Citations");
    assert_eq!(source(&r, "title"), Some(MetadataTier::Provisional));
    assert_eq!(
        r.draft.bibliography.url.as_deref(),
        Some("https://blog.example.org/post")
    );
    assert!(r.draft.external_ids.is_empty());
    cm_core::import::normalize::normalize(&r.draft).unwrap();

    // No title at all and a non-public page URL: the URL text becomes the provisional title,
    // but no URL field is stored.
    let r = resolve::<Fake>(
        &CaptureRequest {
            page_url: "file:///home/user/paper.html".into(),
            ..Default::default()
        },
        &policy,
        None,
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(r.draft.title, "file:///home/user/paper.html");
    assert_eq!(r.draft.bibliography.url, None);
}

#[test]
fn malicious_page_resolves_to_a_valid_reviewable_draft() {
    let mut service = MetadataResolveService::new(Fake::default());
    let mut req = request("https://evil.example.org/x", &fixture("malicious.html"));
    req.pdf_url = Some("https://127.0.0.1/explicit.pdf".into());
    let r = resolve(
        &req,
        &FetchPolicy::default(),
        Some(&mut service),
        &Cancellation::default(),
    )
    .unwrap();
    // Only the single page DOI link was looked up; no URL from the page was requested.
    assert_eq!(
        service.into_provider().calls,
        ["crossref:10.9999/only.link", "datacite:10.9999/only.link"]
    );
    assert!(r.pdf_candidates.is_empty());
    assert_eq!(r.draft.bibliography.publication_date, None);
    assert_eq!(r.draft.bibliography.year, Some(2024));
    assert!(
        r.draft
            .diagnostics
            .iter()
            .any(|d| d.field == "publication_date")
    );
    cm_core::import::normalize::normalize(&r.draft).unwrap();
}

#[test]
fn oversized_requests_are_rejected_before_extraction() {
    let policy = FetchPolicy::default();
    let mut req = request("https://example.org/a", "x");
    req.html = Some("a".repeat(MAX_HTML_BYTES + 1));
    assert!(resolve::<Fake>(&req, &policy, None, &Cancellation::default()).is_err());
    let mut req = request("https://example.org/a", "x");
    req.page_url = "https://example.org/".to_owned() + &"a".repeat(MAX_PAGE_URL_BYTES);
    assert!(resolve::<Fake>(&req, &policy, None, &Cancellation::default()).is_err());
    let mut req = request("https://example.org/a", "x");
    req.json_ld = vec!["{}".into(); MAX_JSON_LD_BLOCKS + 1];
    assert!(resolve::<Fake>(&req, &policy, None, &Cancellation::default()).is_err());
    // Page-forwarded authority fields are not part of the DTO and are ignored.
    let parsed: CaptureRequest = serde_json::from_value(serde_json::json!({
        "page_url": "https://example.org/a",
        "client_kind": "desktop",
        "capabilities": ["grant_admin", "library_write"],
        "requested": ["library_read"]
    }))
    .unwrap();
    assert_eq!(parsed.page_url, "https://example.org/a");
    assert!(
        !serde_json::to_string(&parsed)
            .unwrap()
            .contains("grant_admin")
    );
}
