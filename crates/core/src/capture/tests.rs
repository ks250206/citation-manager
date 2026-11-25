use super::*;
use crate::metadata_resolve::{Provider, ProviderOutcome};
use cm_domain::Contributor;

fn page() -> PageMetadata {
    PageMetadata {
        page_url: "https://example.org/p".into(),
        landing_url: Some("https://example.org/p".into()),
        document_title: Some("Document title".into()),
        ..Default::default()
    }
}
fn author(name: &str) -> Contributor {
    Contributor {
        given_name: None,
        family_name: None,
        literal_name: Some(name.into()),
        orcid: None,
        role: "author".into(),
    }
}

#[test]
fn request_limits() {
    let ok = CaptureRequest {
        page_url: "https://example.org".into(),
        ..Default::default()
    };
    ok.validate().unwrap();
    let mut r = ok.clone();
    r.page_url = " ".into();
    assert!(r.validate().is_err());
    let mut r = ok.clone();
    r.meta = vec![
        MetaTag {
            name: "a".into(),
            content: "b".into()
        };
        MAX_META_TAGS + 1
    ];
    assert!(r.validate().is_err());
    let mut r = ok.clone();
    r.meta = vec![MetaTag {
        name: "a".repeat(MAX_META_NAME_BYTES + 1),
        content: String::new(),
    }];
    assert!(r.validate().is_err());
    let mut r = ok;
    r.json_ld = vec!["x".repeat(MAX_JSON_LD_BYTES); 9];
    assert!(r.validate().is_err(), "aggregate limit");
}

#[test]
fn arxiv_doi_drops_version_only() {
    assert_eq!(
        arxiv_doi("2401.01234v3").as_deref(),
        Some("10.48550/arxiv.2401.01234")
    );
    assert_eq!(
        arxiv_doi("hep-th/9901001v1").as_deref(),
        Some("10.48550/arxiv.hep-th/9901001")
    );
    assert_eq!(
        arxiv_doi("solv-int/9901001").as_deref(),
        Some("10.48550/arxiv.solv-int/9901001")
    );
}

#[test]
fn tier_order_provider_complement_and_provisional() {
    let mut p = page();
    let hw = Bibliography {
        authors: vec![author("High Wire")],
        volume: Some("1".into()),
        ..Default::default()
    };
    let ld = Bibliography {
        volume: Some("2".into()),
        year: Some(2020),
        publication_date: Some("2020-05".into()),
        ..Default::default()
    };
    let dc = Bibliography {
        year: Some(2019),
        publication_date: Some("2019-01-01".into()),
        ..Default::default()
    };
    p.tiers = vec![
        TierRecord {
            tier: Some(MetadataTier::Highwire),
            title: None,
            bibliography: hw,
        },
        TierRecord {
            tier: Some(MetadataTier::JsonLd),
            title: Some("LD title".into()),
            bibliography: ld,
        },
        TierRecord {
            tier: Some(MetadataTier::DublinCore),
            title: Some("DC title".into()),
            bibliography: dc,
        },
    ];
    p.identifiers = vec![FoundIdentifier {
        namespace: "doi".into(),
        value: "10.1234/x".into(),
        tier: MetadataTier::Highwire,
    }];
    let mut calls = vec![];
    let mut lookup = |doi: &str, _: &Cancellation| {
        calls.push(doi.to_owned());
        let mut draft = PaperDraft::titled("Provider title");
        draft.bibliography.journal = Some("Provider journal".into());
        draft.bibliography.year = Some(1999);
        Ok(MetadataResolution {
            canonical_doi: doi.into(),
            draft,
            manual_add: false,
            outcomes: vec![ProviderOutcome {
                provider: Provider::Crossref,
                status: ProviderStatus::Found,
                attempts: 1,
                cached: false,
                retry_after_seconds: None,
            }],
            fields: vec![],
            pdf_candidates: vec![],
        })
    };
    let r = resolve(p, Some(&mut lookup), &Cancellation::default());
    assert_eq!(calls, ["10.1234/x"]);
    assert_eq!(r.draft.title, "LD title");
    assert_eq!(r.draft.bibliography.volume.as_deref(), Some("1"));
    assert_eq!(r.draft.bibliography.year, Some(2020));
    assert_eq!(
        r.draft.bibliography.publication_date.as_deref(),
        Some("2020-05")
    );
    assert_eq!(
        r.draft.bibliography.journal.as_deref(),
        Some("Provider journal")
    );
    assert_eq!(r.draft.bibliography.doi.as_deref(), Some("10.1234/x"));
    assert_eq!(
        r.tiers_used,
        [
            MetadataTier::Highwire,
            MetadataTier::JsonLd,
            MetadataTier::Provider
        ]
    );
    assert!(!r.provisional);
    normalize::normalize(&r.draft).unwrap();

    // Nothing usable: provisional document title + landing URL; no lookup without an ID.
    let mut called = false;
    let mut lookup = |_: &str, _: &Cancellation| {
        called = true;
        Err(ServiceError::Unavailable)
    };
    let r = resolve(page(), Some(&mut lookup), &Cancellation::default());
    assert!(!called);
    assert!(r.provisional);
    assert_eq!(r.draft.title, "Document title");
    assert_eq!(
        r.draft.bibliography.url.as_deref(),
        Some("https://example.org/p")
    );
    normalize::normalize(&r.draft).unwrap();
}

#[test]
fn provider_failure_keeps_page_metadata_and_budget_is_enforced() {
    let mut p = page();
    let hw = Bibliography {
        abstract_text: Some("a".repeat(64 * 1024)),
        authors: vec![author("A")],
        ..Default::default()
    };
    p.tiers = vec![TierRecord {
        tier: Some(MetadataTier::Highwire),
        title: Some("Kept".into()),
        bibliography: hw,
    }];
    p.identifiers = vec![FoundIdentifier {
        namespace: "doi".into(),
        value: "10.1234/y".into(),
        tier: MetadataTier::DublinCore,
    }];
    let mut lookup = |_: &str, _: &Cancellation| Err(ServiceError::Unavailable);
    let r = resolve(p, Some(&mut lookup), &Cancellation::default());
    assert_eq!(r.draft.title, "Kept");
    assert!(
        r.draft
            .diagnostics
            .iter()
            .any(|d| d.message.contains("DOI lookup unavailable"))
    );
    // The oversized abstract is omitted (with a diagnostic) so the row stays reviewable.
    assert_eq!(r.draft.bibliography.abstract_text, None);
    assert!(
        r.draft
            .diagnostics
            .iter()
            .any(|d| d.message.contains("budget"))
    );
    normalize::normalize(&r.draft).unwrap();
}
