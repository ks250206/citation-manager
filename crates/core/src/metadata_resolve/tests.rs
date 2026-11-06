use super::*;
use cm_domain::ExternalId;
struct Port {
    calls: Vec<Provider>,
    replies: std::collections::VecDeque<ProviderReply>,
}
impl MetadataProvider for Port {
    fn lookup(&mut self, provider: Provider, doi: &str, _: &Cancellation) -> ProviderReply {
        assert_eq!(doi, "10.99/test");
        self.calls.push(provider);
        self.replies.pop_front().unwrap()
    }
}
fn reply(provider: Provider, date: &str) -> ProviderReply {
    let mut draft = PaperDraft::titled("Ω Title");
    draft.bibliography.doi = Some("10.99/TEST".into());
    draft.bibliography.publication_date = Some(date.into());
    draft.bibliography.year = Some(date[..4].parse().unwrap());
    ProviderReply {
        outcome: ProviderOutcome {
            provider,
            status: ProviderStatus::Found,
            attempts: 1,
            cached: false,
            retry_after_seconds: None,
        },
        record: Some(ProviderRecord {
            draft,
            pdf_candidates: vec![],
        }),
    }
}
#[test]
fn different_year_complement_cannot_create_inconsistent_date() {
    let mut cr = reply(Provider::Crossref, "2024");
    cr.record
        .as_mut()
        .unwrap()
        .draft
        .bibliography
        .publication_date = None;
    let dc = reply(Provider::DataCite, "2025-03");
    let r = MetadataResolveService::new(Port {
        calls: vec![],
        replies: vec![cr, dc].into(),
    })
    .resolve("10.99/test", &Cancellation::default())
    .unwrap();
    assert_eq!(r.draft.bibliography.year, Some(2024));
    assert_eq!(r.draft.bibliography.publication_date, None);
    assert!(
        r.fields
            .iter()
            .any(|f| f.field == "publication_date" && !f.selected)
    );
}
#[test]
fn port_mismatched_doi_is_rejected_and_empty_records_remain_manual() {
    let mut cr = reply(Provider::Crossref, "2024");
    cr.record
        .as_mut()
        .unwrap()
        .draft
        .external_ids
        .push(ExternalId {
            namespace: "DOI".into(),
            value: "10.99/other".into(),
        });
    let mut dc = reply(Provider::DataCite, "2024");
    dc.record.as_mut().unwrap().draft.title.clear();
    let r = MetadataResolveService::new(Port {
        calls: vec![],
        replies: vec![cr, dc].into(),
    })
    .resolve("10.99/test", &Cancellation::default())
    .unwrap();
    assert_eq!(r.outcomes[0].status, ProviderStatus::IdentityMismatch);
    assert!(r.manual_add);
}
#[test]
fn invalid_input_and_pre_cancelled_calls_do_no_provider_work() {
    let mut s = MetadataResolveService::new(Port {
        calls: vec![],
        replies: vec![].into(),
    });
    for input in [
        "https://evil/10.99/test",
        "https://doi.org/10.99/a%GG",
        "not DOI",
    ] {
        assert!(s.resolve(input, &Cancellation::default()).is_err());
    }
    let cancel = Cancellation::default();
    cancel.cancel();
    let r = s.resolve("10.99/test", &cancel).unwrap();
    assert!(
        r.outcomes
            .iter()
            .all(|o| o.status == ProviderStatus::Cancelled)
    );
    assert!(s.into_provider().calls.is_empty());
}
#[test]
fn invalid_port_identifier_discards_only_that_provider_and_allows_fallback() {
    for both_invalid in [false, true] {
        let mut cr = reply(Provider::Crossref, "2024");
        cr.record
            .as_mut()
            .unwrap()
            .draft
            .external_ids
            .push(ExternalId {
                namespace: "pmid".into(),
                value: "not-a-pmid".into(),
            });
        let mut dc = reply(Provider::DataCite, "2024");
        if both_invalid {
            dc.record
                .as_mut()
                .unwrap()
                .draft
                .external_ids
                .push(ExternalId {
                    namespace: "pmid".into(),
                    value: "bad".into(),
                });
        }
        let r = MetadataResolveService::new(Port {
            calls: vec![],
            replies: vec![cr, dc].into(),
        })
        .resolve("10.99/test", &Cancellation::default())
        .unwrap();
        assert_eq!(r.outcomes[0].status, ProviderStatus::InvalidResponse);
        assert_eq!(r.manual_add, both_invalid);
        assert!(r.draft.external_ids.is_empty());
        if both_invalid {
            assert!(r.draft.title.is_empty());
            assert_eq!(r.outcomes[1].status, ProviderStatus::InvalidResponse);
        } else {
            assert_eq!(r.draft.title, "Ω Title");
            assert_eq!(r.outcomes[1].status, ProviderStatus::Found);
        }
    }
}
#[test]
fn combined_evidence_overflow_is_diagnostic_manual_fallback_without_truncation() {
    let mut cr = reply(Provider::Crossref, "2024");
    cr.record.as_mut().unwrap().draft.bibliography.extra_json =
        Some(serde_json::json!({"large":"x".repeat(65_536)}));
    let r = MetadataResolveService::new(Port {
        calls: vec![],
        replies: vec![cr, reply(Provider::DataCite, "2024")].into(),
    })
    .resolve("10.99/test", &Cancellation::default())
    .unwrap();
    assert!(r.manual_add);
    assert!(r.fields.is_empty());
    assert_eq!(r.draft.bibliography.doi.as_deref(), Some("10.99/test"));
    assert_eq!(r.outcomes[0].status, ProviderStatus::TooLarge);
}
