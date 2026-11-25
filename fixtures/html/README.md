# Synthetic capture pages (#22)

Hand-written, synthetic HTML snapshots for the browser/URL capture extractor
(`cm_metadata::html`) and the capture resolution order. No real publisher page, article text,
PDF or personal data is copied. DOIs use the unassigned `10.9999` prefix; hosts use
`example.org`-style names. Tests never fetch these URLs: they run fully offline (fake DOI
provider, local loopback HTTP fixture server for PDF fetching).

| File | Shape | Exercised behavior |
|---|---|---|
| `publisher.html` | Highwire `citation_*`, Dublin Core, JSON-LD, PDF `<link>`, reference DOI links | tier priority, relative `citation_pdf_url`, reference-list DOIs ignored |
| `arxiv.html` | arXiv-shaped `citation_arxiv_id` with version, no DOI | arXiv identifier, DataCite arXiv DOI complement |
| `pubmed.html` | PubMed-shaped `citation_pmid` (leading zeros), `citation_authors`, DC | PMID canonicalization, semicolon author list, year-only date |
| `jsonld.html` | JSON-LD `@graph` only, `isPartOf` chain, PropertyValue DOI, PDF encoding, one malformed block | JSON-LD tier |
| `dublin-core.html` | Dublin Core / PRISM only | DC tier and resolver-URL `DC.identifier` |
| `no-metadata.html` | `<title>` only plus two cited DOI links | provisional title/URL fallback, ambiguous links not used |
| `malicious.html` | command/path strings, scripts, comments, invalid IDs/dates, SSRF PDF links, spoofed authority meta, unterminated markup | inert parsing, invalid values dropped with diagnostics, no unsafe PDF candidate |
