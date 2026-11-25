//! Bounded uncompressed XMP suggestions; no entity expansion, scripts or TeX.
use cm_core::import::PaperDraft;
use cm_domain::Contributor;
const DC: &str = "http://purl.org/dc/elements/1.1/";
const PRISM: &str = "http://prismstandard.org/namespaces/basic/2.0/";
const PDFX: &str = "http://ns.adobe.com/pdfx/1.3/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub(crate) fn xmp(data: &[u8]) -> (Option<PaperDraft>, Vec<String>) {
    let Some(start) = data.windows(10).position(|s| s == b"<x:xmpmeta") else {
        return (
            None,
            vec![
                "No readable uncompressed XMP packet; compressed/other metadata is not inferred"
                    .into(),
            ],
        );
    };
    let tail = &data[start..data.len().min(start + 65536)];
    let Some(end) = tail.windows(12).position(|s| s == b"</x:xmpmeta>") else {
        return (None, vec!["XMP packet incomplete or exceeds 64 KiB".into()]);
    };
    let Ok(xml) = std::str::from_utf8(&tail[..end + 12]) else {
        return (
            None,
            vec!["XMP is not UTF-8; use filename/manual metadata".into()],
        );
    };
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return (
            None,
            vec!["XMP is invalid or contains unsupported entities".into()],
        );
    };
    let mut draft = PaperDraft::default();
    let mut messages = vec!["XMP packet is an untrusted hint; association with the current document catalog is not verified".into()];
    for node in doc.descendants().filter(|n| n.is_element()) {
        if node.has_tag_name((DC, "title")) {
            let title = node
                .descendants()
                .find(|n| n.has_tag_name((RDF, "li")))
                .and_then(|n| n.text());
            if let Some(title) = title {
                draft.title = title.to_owned();
            }
        }
        if node.has_tag_name((DC, "creator")) {
            for author in node
                .descendants()
                .filter(|n| n.has_tag_name((RDF, "li")))
                .filter_map(|n| n.text())
            {
                if draft.bibliography.authors.len() >= 256 {
                    return (None, vec!["XMP contributor limit exceeded".into()]);
                }
                draft.bibliography.authors.push(Contributor {
                    given_name: None,
                    family_name: None,
                    literal_name: Some(author.into()),
                    orcid: None,
                    role: "author".into(),
                });
            }
        }
        if (node.has_tag_name((PRISM, "doi")) || node.has_tag_name((PDFX, "doi")))
            && let Some(value) = node.text()
        {
            match cm_core::papers::normalize_doi(value) {
                Ok(doi) => draft.bibliography.doi = Some(doi),
                Err(_) => messages.push("Invalid embedded DOI ignored".into()),
            }
        }
        for attr in node
            .attributes()
            .filter(|a| a.name() == "doi" && [Some(PRISM), Some(PDFX)].contains(&a.namespace()))
        {
            match cm_core::papers::normalize_doi(attr.value()) {
                Ok(doi) => draft.bibliography.doi = Some(doi),
                Err(_) => messages.push("Invalid embedded DOI ignored".into()),
            }
        }
    }
    (Some(draft), messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_xmp_suggestions_are_namespace_aware_and_do_not_expand_entities() {
        let xml=br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:prism="http://prismstandard.org/namespaces/basic/2.0/"><dc:title><rdf:Alt><rdf:li>Suggested title</rdf:li></rdf:Alt></dc:title><dc:creator><rdf:Seq><rdf:li>First</rdf:li><rdf:li>Second</rdf:li></rdf:Seq></dc:creator><prism:doi>10.1234/EXAMPLE</prism:doi></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (draft, _) = xmp(xml);
        let draft = draft.unwrap();
        assert_eq!(draft.title, "Suggested title");
        assert_eq!(draft.bibliography.authors.len(), 2);
        assert_eq!(draft.bibliography.doi.as_deref(), Some("10.1234/example"));
        let (draft, messages) = xmp(b"<x:xmpmeta>&external;</x:xmpmeta>");
        assert!(draft.is_none());
        assert!(!messages.is_empty());
        let bytes = [
            b"<x:xmpmeta".as_slice(),
            vec![b' '; 65536].as_slice(),
            b"</x:xmpmeta>",
        ]
        .concat();
        assert!(xmp(&bytes).0.is_none());
    }
}
