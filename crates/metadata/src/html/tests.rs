use super::*;

pub(crate) fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../fixtures/html/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}
pub(crate) fn request(page_url: &str, html: &str) -> CaptureRequest {
    CaptureRequest {
        page_url: page_url.into(),
        html: Some(html.into()),
        ..Default::default()
    }
}
fn tier(page: &PageMetadata, tier: MetadataTier) -> &TierRecord {
    page.tiers.iter().find(|t| t.tier == Some(tier)).unwrap()
}
fn id(page: &PageMetadata, ns: &str) -> Option<(String, MetadataTier)> {
    page.identifier(ns).map(|i| (i.value.clone(), i.tier))
}

#[test]
fn publisher_page_tiers_identifiers_and_pdfs() {
    let page = extract(
        &request(
            "https://journal.example.org/article/0012",
            &fixture("publisher.html"),
        ),
        &FetchPolicy::default(),
    );
    let hw = tier(&page, MetadataTier::Highwire);
    assert_eq!(
        hw.title.as_deref(),
        Some("Lattice Methods for Sparse Citation Graphs")
    );
    let authors = &hw.bibliography.authors;
    assert_eq!(authors.len(), 3);
    assert_eq!(authors[0].family_name.as_deref(), Some("Schröder"));
    assert_eq!(authors[0].given_name.as_deref(), Some("Jörg"));
    assert_eq!(
        authors[2].literal_name.as_deref(),
        Some("Example Consortium")
    );
    assert_eq!(
        hw.bibliography.publication_date.as_deref(),
        Some("2024-03-07")
    );
    assert_eq!(hw.bibliography.year, Some(2024));
    assert_eq!(hw.bibliography.pages.as_deref(), Some("101-118"));
    assert_eq!(
        hw.bibliography.abstract_text.as_deref(),
        Some("We describe lattice methods & their <bounded> behaviour.")
    );
    assert_eq!(
        id(&page, "doi"),
        Some(("10.9999/jse.2024.0012".into(), MetadataTier::Highwire))
    );
    // DC and JSON-LD are still collected as lower tiers.
    assert!(tier(&page, MetadataTier::JsonLd).title.is_some());
    assert!(tier(&page, MetadataTier::DublinCore).title.is_some());
    // Relative citation_pdf_url is resolved against the page; the <link> is offered too.
    let urls: Vec<_> = page.pdf_candidates.iter().map(|c| c.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://journal.example.org/content/jse.2024.0012.pdf",
            "https://cdn.example.org/jse/0012.pdf"
        ]
    );
    assert_eq!(page.pdf_candidates[0].origin, PdfOrigin::Highwire);
    assert_eq!(page.pdf_candidates[1].origin, PdfOrigin::Link);
    assert_eq!(
        page.landing_url.as_deref(),
        Some("https://journal.example.org/article/0012")
    );
    assert!(page.html_sha256.is_some());
}

#[test]
fn arxiv_pubmed_jsonld_and_dublin_core_shapes() {
    let policy = FetchPolicy::default();
    let arxiv = extract(
        &request("https://arxiv.org/abs/2401.01234v2", &fixture("arxiv.html")),
        &policy,
    );
    assert_eq!(
        id(&arxiv, "arxiv"),
        Some(("2401.01234v2".into(), MetadataTier::Highwire))
    );
    assert!(arxiv.identifier("doi").is_none());
    assert_eq!(
        arxiv.pdf_candidates[0].url,
        "https://arxiv.org/pdf/2401.01234v2"
    );
    // citation_date wins over citation_online_date.
    assert_eq!(
        tier(&arxiv, MetadataTier::Highwire)
            .bibliography
            .publication_date
            .as_deref(),
        Some("2024-01-05")
    );

    let pubmed = extract(
        &request(
            "https://pubmed.ncbi.nlm.nih.gov/31234567/",
            &fixture("pubmed.html"),
        ),
        &policy,
    );
    assert_eq!(
        id(&pubmed, "pmid"),
        Some(("31234567".into(), MetadataTier::Highwire))
    );
    let hw = tier(&pubmed, MetadataTier::Highwire);
    assert_eq!(hw.bibliography.authors.len(), 2);
    assert_eq!(
        hw.bibliography.authors[1].family_name.as_deref(),
        Some("Müller")
    );
    assert_eq!(hw.bibliography.year, Some(2023));
    assert_eq!(hw.bibliography.publication_date.as_deref(), Some("2023"));
    // The same PMID from DC and the URL is not a conflict.
    assert!(
        !pubmed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("conflicting"))
    );

    let jsonld = extract(
        &request(
            "https://bulletin.example.org/e1001",
            &fixture("jsonld.html"),
        ),
        &policy,
    );
    let j = tier(&jsonld, MetadataTier::JsonLd);
    assert_eq!(
        j.title.as_deref(),
        Some("Graph-Shaped Metadata Without Meta Tags")
    );
    let a = &j.bibliography.authors;
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].family_name.as_deref(), Some("Silva"));
    assert_eq!(
        a[0].orcid.as_deref(),
        Some("https://orcid.org/0000-0002-1825-0097")
    );
    assert_eq!(a[1].literal_name.as_deref(), Some("Synthetic Lab"));
    assert_eq!(
        j.bibliography.journal.as_deref(),
        Some("Bulletin of Example Studies")
    );
    assert_eq!(j.bibliography.volume.as_deref(), Some("7"));
    assert_eq!(j.bibliography.issue.as_deref(), Some("4"));
    assert_eq!(j.bibliography.pages.as_deref(), Some("e1001"));
    assert_eq!(
        j.bibliography.publication_date.as_deref(),
        Some("2022-11-30")
    );
    assert_eq!(
        id(&jsonld, "doi"),
        Some(("10.9999/bes.7.4.e1001".into(), MetadataTier::JsonLd))
    );
    assert_eq!(
        jsonld.pdf_candidates[0].url,
        "https://files.example.org/bes/e1001.pdf"
    );
    assert!(
        jsonld
            .diagnostics
            .iter()
            .any(|d| d.message.contains("malformed JSON-LD"))
    );

    let dc = extract(
        &request(
            "https://repository.example.org/record/77",
            &fixture("dublin-core.html"),
        ),
        &policy,
    );
    let d = tier(&dc, MetadataTier::DublinCore);
    assert_eq!(d.bibliography.publication_date.as_deref(), Some("2021-06"));
    assert_eq!(d.bibliography.journal.as_deref(), Some("Repository Series"));
    assert_eq!(
        id(&dc, "doi"),
        Some(("10.9999/repo.2021.77".into(), MetadataTier::DublinCore))
    );
}

#[test]
fn no_metadata_page_has_only_a_document_title() {
    let page = extract(
        &request(
            "https://blog.example.org/post",
            &fixture("no-metadata.html"),
        ),
        &FetchPolicy::default(),
    );
    assert!(page.tiers.is_empty());
    assert!(page.identifiers.is_empty());
    assert!(page.pdf_candidates.is_empty());
    assert_eq!(
        page.document_title.as_deref(),
        Some("A Blog Post About Citations")
    );
    assert!(
        page.diagnostics
            .iter()
            .any(|d| d.message.contains("2 distinct DOI links"))
    );
}

#[test]
fn malicious_page_is_inert_and_offers_no_unsafe_pdf() {
    let page = extract(
        &request("https://evil.example.org/x", &fixture("malicious.html")),
        &FetchPolicy::default(),
    );
    let hw = tier(&page, MetadataTier::Highwire);
    // Values are plain text. Commented and script-written tags are not metadata.
    assert_eq!(hw.title.as_deref(), Some("../../../../etc/passwd&#0;&#x7;"));
    assert_eq!(
        hw.bibliography.authors[0].literal_name.as_deref(),
        Some("<script>alert(1)</script>")
    );
    assert_eq!(hw.bibliography.authors.len(), 1);
    assert_eq!(
        hw.bibliography.volume.as_deref(),
        Some("single-quoted & fine")
    );
    assert_eq!(hw.bibliography.issue.as_deref(), Some("unquoted-9"));
    assert_eq!(hw.bibliography.journal, None);
    assert_eq!(hw.bibliography.pages, None);
    // Invalid identifiers are dropped with diagnostics; the single DOI link is used.
    assert!(page.identifier("arxiv").is_none());
    assert!(page.identifier("pmid").is_none());
    assert_eq!(
        id(&page, "doi"),
        Some(("10.9999/only.link".into(), MetadataTier::PageUrl))
    );
    for ns in ["doi", "arxiv", "pmid"] {
        assert!(
            page.diagnostics
                .iter()
                .any(|d| d.field == ns && d.message.contains("invalid"))
        );
    }
    // Every SSRF-ish/odd PDF link is refused by the fetch policy.
    assert!(page.pdf_candidates.is_empty(), "{:?}", page.pdf_candidates);
    let refused = page.diagnostics.iter().filter(|d| d.field == "pdf").count();
    assert!(refused >= 12, "{refused}");
    // Authority-looking meta is just data; nothing in PageMetadata carries authority.
    assert!(!format!("{page:?}").contains("grant_admin"));
}

#[test]
fn scanner_handles_comments_entities_case_and_limits() {
    let s = scan(
        "<META NAME='Citation_Title' CONTENT=\"A &#x26; B &#38; C &bogus; &#xD800;\"><!-- <meta name=x content=y>",
    );
    assert_eq!(
        s.meta,
        [("citation_title".into(), "A & B & C &bogus; &#xD800;".into())]
    );
    // Unterminated comment ends the scan without panicking.
    let s = scan("<meta name=a content=b><!-- never closed <meta name=c content=d>");
    assert_eq!(s.meta.len(), 1);
    // Many tags: bounded collection.
    let big = "<meta name=\"citation_author\" content=\"A, B\">".repeat(3000);
    let s = scan(&big);
    assert_eq!(s.meta.len(), MAX_META_TAGS);
    // Truncated input mid-tag / mid-attribute.
    for cut in [
        "<meta",
        "<meta name=",
        "<meta name=\"x",
        "<",
        "<<<",
        "</",
        "<!",
        "<script>",
    ] {
        let _ = scan(cut);
    }
}

#[test]
fn request_meta_and_json_ld_are_used_without_html() {
    let request = CaptureRequest {
        page_url: "https://doi.org/10.9999/URL.Form".into(),
        meta: vec![MetaTag {
            name: "citation_title".into(),
            content: "From content script".into(),
        }],
        json_ld: vec![r#"{"@type":"ScholarlyArticle","headline":"LD"}"#.into()],
        ..Default::default()
    };
    let page = extract(&request, &FetchPolicy::default());
    assert_eq!(
        tier(&page, MetadataTier::Highwire).title.as_deref(),
        Some("From content script")
    );
    assert_eq!(
        tier(&page, MetadataTier::JsonLd).title.as_deref(),
        Some("LD")
    );
    assert_eq!(
        id(&page, "doi"),
        Some(("10.9999/url.form".into(), MetadataTier::PageUrl))
    );
}
