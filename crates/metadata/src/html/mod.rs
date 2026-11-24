//! #22 inert, bounded page-metadata extraction. The HTML snapshot is scanned as text: no
//! script runs, nothing is fetched, no entity beyond the basic/numeric forms is expanded,
//! and no value is ever used as a command, path or endpoint. Unknown markup is skipped.
use crate::{fetch_policy::FetchPolicy, providers::public_url};
use cm_core::{
    capture::*,
    import::{RowDiagnostic, normalize},
    papers::normalize_doi,
};
use cm_domain::{Bibliography, Contributor, ExternalId};
use reqwest::Url;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[cfg(test)]
pub(crate) mod tests;

const MAX_TAGS: usize = 100_000;
const MAX_DOI_LINKS: usize = 4096;
const MAX_LINK_TAGS: usize = 1024;
const MAX_DIAGNOSTICS: usize = 48;
const MAX_AUTHORS: usize = 256;
const MAX_JSON_NODES: usize = 2048;

/// Raw markup pieces collected by the scanner.
#[derive(Debug, Default)]
pub struct Scanned {
    /// (lowercase name/property, decoded content)
    pub meta: Vec<(String, String)>,
    /// Attributes of `<link>` elements (lowercase names).
    pub links: Vec<Vec<(String, String)>>,
    /// `href`s of anchors pointing at a DOI resolver.
    pub doi_links: Vec<String>,
    pub json_ld: Vec<String>,
    pub title: Option<String>,
}

/// Scans an HTML snapshot without building a DOM. Linear in the input; every collection is
/// capped. Malformed or unterminated markup ends the scan instead of erroring.
pub fn scan(html: &str) -> Scanned {
    let mut out = Scanned::default();
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut tags = 0;
    while let Some(offset) = memchr(b'<', &bytes[i..]) {
        i += offset;
        tags += 1;
        if tags > MAX_TAGS {
            break;
        }
        let rest = &html[i..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            match comment.find("-->") {
                Some(end) => i += 4 + end + 3,
                None => break,
            }
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") || rest.starts_with("</") {
            match rest.find('>') {
                Some(end) => i += end + 1,
                None => break,
            }
            continue;
        }
        let Some((name, attrs, end)) = parse_tag(rest) else {
            i += 1;
            continue;
        };
        i += end;
        match name.as_str() {
            "meta" if out.meta.len() < MAX_META_TAGS => {
                let key = attr(&attrs, "name")
                    .or_else(|| attr(&attrs, "property"))
                    .or_else(|| attr(&attrs, "itemprop"));
                if let (Some(key), Some(content)) = (key, attr(&attrs, "content")) {
                    out.meta
                        .push((key.trim().to_ascii_lowercase(), content.to_owned()));
                }
            }
            "link" if out.links.len() < MAX_LINK_TAGS => out.links.push(attrs),
            "a" if out.doi_links.len() < MAX_DOI_LINKS => {
                if let Some(href) = attr(&attrs, "href")
                    && is_doi_resolver(href)
                {
                    out.doi_links.push(href.to_owned());
                }
            }
            "script" | "style" | "title" | "textarea" | "noscript" | "template" => {
                let close = format!("</{name}");
                let body_end = find_ci(&html[i..], &close).unwrap_or(html.len() - i);
                let body = &html[i..i + body_end];
                if name == "script"
                    && attr(&attrs, "type").is_some_and(|t| {
                        t.split(';')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .eq_ignore_ascii_case("application/ld+json")
                    })
                    && out.json_ld.len() < MAX_JSON_LD_BLOCKS
                    && body.len() <= MAX_JSON_LD_BYTES
                {
                    out.json_ld.push(body.to_owned());
                }
                if name == "title" && out.title.is_none() {
                    out.title = Some(decode(body));
                }
                i += body_end;
            }
            _ => {}
        }
    }
    out
}

fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|b| *b == needle)
}
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let n = needle.len();
    haystack
        .as_bytes()
        .windows(n)
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

type Attrs = Vec<(String, String)>;
/// `<name attr=value ...>`; returns lowercase name, decoded attributes and bytes consumed.
fn parse_tag(input: &str) -> Option<(String, Attrs, usize)> {
    let b = input.as_bytes();
    let mut i = 1;
    let start = i;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b':') {
        i += 1;
    }
    if i == start || !b[start].is_ascii_alphabetic() {
        return None;
    }
    let name = input[start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        if b[i] == b'>' {
            return Some((name, attrs, i + 1));
        }
        let key_start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let key = input[key_start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i];
                let begin = i + 1;
                let end = begin + memchr(quote, &b[begin..])?;
                value = decode(&input[begin..end]);
                i = end + 1;
            } else {
                let begin = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                value = decode(&input[begin..i]);
            }
        }
        if !key.is_empty() && attrs.len() < 64 && value.len() <= MAX_META_CONTENT_BYTES {
            attrs.push((key, value));
        }
    }
}
fn attr<'a>(attrs: &'a Attrs, name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Basic named and numeric character references only; everything else stays literal.
pub fn decode(input: &str) -> String {
    if !input.contains('&') {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let end = rest[..rest.len().min(12)].find(';');
        let decoded = end.and_then(|end| {
            let entity = &rest[1..end];
            let ch = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let number = entity.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code).filter(|c| !c.is_control() || c.is_whitespace())
                }
            }?;
            Some((ch, end + 1))
        });
        match decoded {
            Some((ch, used)) => {
                out.push(ch);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whitespace-collapsed text with control characters removed; `None` when empty or over
/// `limit` bytes (oversized page values are dropped, never silently cut).
fn clean(value: &str, limit: usize, field: &str, diags: &mut Vec<RowDiagnostic>) -> Option<String> {
    let text: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = normalize::spaces(&text);
    if text.is_empty() {
        return None;
    }
    if text.len() > limit {
        diag(
            diags,
            field,
            format!("page value for {field} exceeds {limit} bytes and was not used"),
        );
        return None;
    }
    Some(text)
}
fn diag(diags: &mut Vec<RowDiagnostic>, field: &str, message: String) {
    if diags.len() < MAX_DIAGNOSTICS {
        diags.push(RowDiagnostic {
            field: field.into(),
            message,
            error: false,
        });
    }
}

fn is_doi_resolver(href: &str) -> bool {
    let lower = href.trim().to_ascii_lowercase();
    [
        "https://doi.org/10.",
        "http://doi.org/10.",
        "https://dx.doi.org/10.",
        "http://dx.doi.org/10.",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
}

/// Accepts `doi:`, `info:doi/`, resolver URLs and bare DOIs; canonical via #33's normalizer.
fn doi_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = raw
        .get(..9)
        .filter(|p| p.eq_ignore_ascii_case("info:doi/"))
        .map_or(raw, |_| &raw[9..]);
    normalize_doi(raw).ok()
}
fn arxiv_value(raw: &str) -> Option<String> {
    let id = normalize::external_id(&ExternalId {
        namespace: "arxiv".into(),
        value: raw.trim().trim_end_matches(".pdf").into(),
    })
    .ok()?
    .value;
    let (core, version) = match id.rsplit_once('v') {
        Some((c, v)) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => (c, Some(v)),
        _ => (id.as_str(), None),
    };
    let _ = version;
    let new_style = core.split_once('.').is_some_and(|(a, b)| {
        a.len() == 4
            && a.bytes().all(|c| c.is_ascii_digit())
            && (4..=5).contains(&b.len())
            && b.bytes().all(|c| c.is_ascii_digit())
    });
    let old_style = core.split_once('/').is_some_and(|(a, b)| {
        !a.is_empty()
            && a.len() <= 32
            && a.bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'-' || c == b'.')
            && b.len() == 7
            && b.bytes().all(|c| c.is_ascii_digit())
    });
    (new_style || old_style).then_some(id)
}
fn pmid_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    (!raw.is_empty() && raw.len() <= 10 && raw.bytes().all(|b| b.is_ascii_digit()))
        .then(|| raw.trim_start_matches('0').to_owned())
        .filter(|v| !v.is_empty())
}

/// Identifiers from the page URL itself (never fetched).
fn url_identifiers(page: &Url) -> Vec<(&'static str, String)> {
    let host = page.host_str().unwrap_or("").to_ascii_lowercase();
    let path = page.path();
    let mut found = vec![];
    if matches!(host.as_str(), "doi.org" | "dx.doi.org" | "www.doi.org")
        && let Some(doi) = doi_value(&format!("https://doi.org{path}"))
    {
        found.push(("doi", doi));
    }
    if matches!(
        host.as_str(),
        "arxiv.org" | "www.arxiv.org" | "export.arxiv.org"
    ) {
        for prefix in ["/abs/", "/pdf/"] {
            if let Some(id) = path.strip_prefix(prefix).and_then(arxiv_value) {
                found.push(("arxiv", id));
            }
        }
    }
    let pubmed = match host.as_str() {
        "pubmed.ncbi.nlm.nih.gov" => path.trim_matches('/').split('/').next(),
        "www.ncbi.nlm.nih.gov" | "ncbi.nlm.nih.gov" => {
            path.strip_prefix("/pubmed/").map(|p| p.trim_matches('/'))
        }
        _ => None,
    };
    if let Some(pmid) = pubmed.and_then(pmid_value) {
        found.push(("pmid", pmid));
    }
    found
}

#[derive(Default)]
struct Tier {
    title: Option<String>,
    b: Bibliography,
    ids: Vec<(&'static str, String)>,
    pdfs: Vec<String>,
}

fn date_parts(raw: &str) -> (Option<String>, Option<i32>) {
    let raw = raw.trim();
    let date = raw.split(['T', ' ']).next().unwrap_or("").replace('/', "-");
    let year = date
        .get(..4)
        .filter(|y| y.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|y| y.parse().ok());
    let date = Some(date).filter(|d| {
        !d.is_empty() && d.len() <= 10 && d.bytes().all(|b| b.is_ascii_digit() || b == b'-')
    });
    (date, year)
}

fn person(raw: &str, diags: &mut Vec<RowDiagnostic>) -> Option<Contributor> {
    let name = clean(raw, 1024, "authors", diags)?;
    // Highwire's documented form is "Family, Given"; without a comma keep the literal name.
    let (family, given) = match name.split_once(',') {
        Some((f, g)) if !f.trim().is_empty() && !g.trim().is_empty() => {
            (Some(f.trim().to_owned()), Some(g.trim().to_owned()))
        }
        _ => (None, None),
    };
    Some(Contributor {
        literal_name: family.is_none().then_some(name),
        given_name: given,
        family_name: family,
        orcid: None,
        role: "author".into(),
    })
}

fn first<'a>(meta: &'a [(String, String)], keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| meta.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()))
}
fn all<'a>(meta: &'a [(String, String)], keys: &[&str]) -> Vec<&'a str> {
    meta.iter()
        .filter(|(n, _)| keys.contains(&n.as_str()))
        .map(|(_, v)| v.as_str())
        .collect()
}

fn set_date(tier: &mut Tier, raw: Option<&str>) {
    if let Some(raw) = raw {
        let (date, year) = date_parts(raw);
        tier.b.publication_date = date;
        tier.b.year = tier.b.year.or(year);
    }
}
fn pages(first_page: Option<String>, last: Option<String>) -> Option<String> {
    match (first_page, last) {
        (Some(f), Some(l)) if f != l => Some(format!("{f}-{l}")),
        (Some(f), _) => Some(f),
        _ => None,
    }
}

fn highwire(meta: &[(String, String)], d: &mut Vec<RowDiagnostic>) -> Tier {
    let mut t = Tier::default();
    let text = |keys: &[&str], limit, field, d: &mut Vec<RowDiagnostic>| {
        first(meta, keys).and_then(|v| clean(v, limit, field, d))
    };
    t.title = text(&["citation_title"], normalize::MAX_TITLE_BYTES, "title", d);
    let mut authors: Vec<Contributor> = all(meta, &["citation_author"])
        .into_iter()
        .filter_map(|a| person(a, d))
        .collect();
    if authors.is_empty()
        && let Some(list) = first(meta, &["citation_authors"])
    {
        authors = list.split(';').filter_map(|a| person(a, d)).collect();
    }
    authors.truncate(MAX_AUTHORS);
    t.b.authors = authors;
    set_date(
        &mut t,
        first(
            meta,
            &[
                "citation_publication_date",
                "citation_date",
                "citation_cover_date",
                "citation_online_date",
            ],
        ),
    );
    if t.b.year.is_none() {
        t.b.year = first(meta, &["citation_year"]).and_then(|y| y.trim().parse().ok());
    }
    let container = [
        ("citation_journal_title", "journal-article"),
        ("citation_conference_title", "proceedings-article"),
        ("citation_inbook_title", "book-chapter"),
        ("citation_book_title", "book-chapter"),
    ]
    .into_iter()
    .find_map(|(k, kind)| {
        first(meta, &[k])
            .and_then(|v| clean(v, 4096, "journal", d))
            .map(|v| (v, kind))
    });
    if let Some((journal, kind)) = container {
        t.b.journal = Some(journal);
        t.b.paper_type = Some(kind.into());
    } else if first(meta, &["citation_dissertation_institution"]).is_some() {
        t.b.paper_type = Some("dissertation".into());
    } else if first(meta, &["citation_technical_report_institution"]).is_some() {
        t.b.paper_type = Some("report".into());
    }
    t.b.volume = text(&["citation_volume"], 256, "volume", d);
    t.b.issue = text(&["citation_issue"], 256, "issue", d);
    t.b.pages = pages(
        text(&["citation_firstpage"], 128, "pages", d),
        text(&["citation_lastpage"], 128, "pages", d),
    );
    t.b.abstract_text = text(&["citation_abstract"], 32 * 1024, "abstract", d);
    t.b.language = text(&["citation_language"], 64, "language", d);
    if let Some(doi) = first(meta, &["citation_doi"]) {
        push_id(&mut t, "doi", doi_value(doi), doi, d);
    }
    if let Some(a) = first(meta, &["citation_arxiv_id"]) {
        push_id(&mut t, "arxiv", arxiv_value(a), a, d);
    }
    if let Some(p) = first(meta, &["citation_pmid"]) {
        push_id(&mut t, "pmid", pmid_value(p), p, d);
    }
    t.pdfs = all(meta, &["citation_pdf_url"])
        .into_iter()
        .map(str::to_owned)
        .collect();
    t
}

fn push_id(
    t: &mut Tier,
    ns: &'static str,
    value: Option<String>,
    raw: &str,
    d: &mut Vec<RowDiagnostic>,
) {
    match value {
        Some(v) => {
            if !t.ids.iter().any(|(n, _)| *n == ns) {
                t.ids.push((ns, v));
            }
        }
        None => diag(
            d,
            ns,
            format!(
                "invalid {ns} identifier on the page was ignored ({} bytes)",
                raw.len()
            ),
        ),
    }
}

fn dublin_core(meta: &[(String, String)], d: &mut Vec<RowDiagnostic>) -> Tier {
    let mut t = Tier::default();
    let text = |keys: &[&str], limit, field, d: &mut Vec<RowDiagnostic>| {
        first(meta, keys).and_then(|v| clean(v, limit, field, d))
    };
    t.title = text(
        &["dc.title", "dcterms.title"],
        normalize::MAX_TITLE_BYTES,
        "title",
        d,
    );
    t.b.authors = all(meta, &["dc.creator", "dcterms.creator"])
        .into_iter()
        .filter_map(|a| person(a, d))
        .take(MAX_AUTHORS)
        .collect();
    set_date(
        &mut t,
        first(
            meta,
            &[
                "dcterms.issued",
                "dc.date.issued",
                "dc.date",
                "dcterms.date",
                "prism.publicationdate",
            ],
        ),
    );
    t.b.journal = text(
        &["prism.publicationname", "dc.source", "dcterms.ispartof"],
        4096,
        "journal",
        d,
    );
    t.b.volume = text(&["prism.volume"], 256, "volume", d);
    t.b.issue = text(&["prism.number", "prism.issueidentifier"], 256, "issue", d);
    t.b.pages = pages(
        text(&["prism.startingpage"], 128, "pages", d),
        text(&["prism.endingpage"], 128, "pages", d),
    );
    t.b.abstract_text = text(
        &["dcterms.abstract", "dc.description", "dcterms.description"],
        32 * 1024,
        "abstract",
        d,
    );
    t.b.language = text(&["dc.language", "dcterms.language"], 64, "language", d);
    if let Some(doi) = first(meta, &["prism.doi"]) {
        push_id(&mut t, "doi", doi_value(doi), doi, d);
    }
    for raw in all(meta, &["dc.identifier", "dcterms.identifier"]) {
        let lower = raw.trim().to_ascii_lowercase();
        if lower.starts_with("doi:")
            || lower.starts_with("info:doi/")
            || is_doi_resolver(&lower)
            || lower.starts_with("10.")
        {
            push_id(&mut t, "doi", doi_value(raw), raw, d);
        } else if let Some(p) = lower.strip_prefix("pmid:") {
            push_id(&mut t, "pmid", pmid_value(p), raw, d);
        } else if let Some(a) = lower.strip_prefix("arxiv:") {
            push_id(&mut t, "arxiv", arxiv_value(a), raw, d);
        }
    }
    t
}

const SCHOLARLY: [&str; 9] = [
    "scholarlyarticle",
    "medicalscholarlyarticle",
    "article",
    "techarticle",
    "report",
    "thesis",
    "chapter",
    "book",
    "creativework",
];

fn types(v: &Value) -> Vec<String> {
    match v.get("@type") {
        Some(Value::String(s)) => vec![s.to_ascii_lowercase()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_ascii_lowercase)
            .collect(),
        _ => vec![],
    }
}
/// The best scholarly node in a JSON-LD document (top-level, array or `@graph`).
fn scholarly(doc: &Value) -> Option<&Value> {
    let mut stack = vec![doc];
    let mut best: Option<(usize, &Value)> = None;
    let mut seen = 0;
    while let Some(v) = stack.pop() {
        seen += 1;
        if seen > MAX_JSON_NODES {
            break;
        }
        match v {
            Value::Array(items) => stack.extend(items.iter().take(256)),
            Value::Object(map) => {
                if let Some(graph) = map.get("@graph") {
                    stack.push(graph);
                }
                for t in types(v) {
                    if let Some(rank) = SCHOLARLY.iter().position(|s| *s == t)
                        && best.is_none_or(|(r, _)| rank < r)
                    {
                        best = Some((rank, v));
                    }
                }
            }
            _ => {}
        }
    }
    best.map(|(_, v)| v)
}
fn json_str(v: Option<&Value>) -> Option<&str> {
    match v? {
        Value::String(s) => Some(s),
        Value::Array(a) => a.first().and_then(Value::as_str),
        Value::Number(_) => None,
        Value::Object(o) => o.get("name").and_then(Value::as_str),
        _ => None,
    }
}
fn json_text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) => a.first().and_then(|x| json_text(Some(x))),
        _ => None,
    }
}
fn json_list(v: Option<&Value>) -> Vec<&Value> {
    match v {
        Some(Value::Array(a)) => a.iter().take(MAX_AUTHORS).collect(),
        Some(other) => vec![other],
        None => vec![],
    }
}

fn json_ld(blocks: &[String], d: &mut Vec<RowDiagnostic>) -> Tier {
    let mut t = Tier::default();
    let mut node = None;
    let parsed: Vec<Value> = blocks
        .iter()
        .filter_map(|b| match serde_json::from_str::<Value>(b.trim()) {
            Ok(v) => Some(v),
            Err(_) => {
                diag(d, "json_ld", "malformed JSON-LD block ignored".into());
                None
            }
        })
        .collect();
    for doc in &parsed {
        if let Some(n) = scholarly(doc) {
            node = Some(n);
            break;
        }
    }
    let Some(n) = node else {
        return t;
    };
    let text = |v: Option<&Value>, limit, field, d: &mut Vec<RowDiagnostic>| {
        json_text(v).and_then(|s| clean(&s, limit, field, d))
    };
    t.title = text(
        n.get("headline").or_else(|| n.get("name")),
        normalize::MAX_TITLE_BYTES,
        "title",
        d,
    );
    for a in json_list(n.get("author")) {
        let c = match a {
            Value::String(s) => person(s, d),
            Value::Object(o) => {
                let given = o
                    .get("givenName")
                    .and_then(Value::as_str)
                    .and_then(|s| clean(s, 512, "authors", d));
                let family = o
                    .get("familyName")
                    .and_then(Value::as_str)
                    .and_then(|s| clean(s, 512, "authors", d));
                let name = o
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(|s| clean(s, 1024, "authors", d));
                let orcid = o
                    .get("@id")
                    .or_else(|| o.get("sameAs"))
                    .and_then(Value::as_str)
                    .filter(|s| s.starts_with("https://orcid.org/") && s.len() <= 64)
                    .map(str::to_owned);
                if family.is_some() || given.is_some() {
                    Some(Contributor {
                        given_name: given,
                        family_name: family,
                        literal_name: None,
                        orcid,
                        role: "author".into(),
                    })
                } else {
                    name.map(|n| Contributor {
                        given_name: None,
                        family_name: None,
                        literal_name: Some(n),
                        orcid,
                        role: "author".into(),
                    })
                }
            }
            _ => None,
        };
        if let Some(c) = c
            && t.b.authors.len() < MAX_AUTHORS
        {
            t.b.authors.push(c);
        }
    }
    set_date(&mut t, json_str(n.get("datePublished")));
    // isPartOf chain: PublicationIssue -> PublicationVolume -> Periodical (bounded depth).
    let mut part = n.get("isPartOf");
    t.b.volume = text(n.get("volumeNumber"), 256, "volume", d);
    t.b.issue = text(n.get("issueNumber"), 256, "issue", d);
    for _ in 0..4 {
        let Some(p) = part else { break };
        let p = if let Value::Array(a) = p {
            a.first().unwrap_or(p)
        } else {
            p
        };
        let kinds = types(p);
        if kinds.iter().any(|k| k == "publicationissue") && t.b.issue.is_none() {
            t.b.issue = text(p.get("issueNumber"), 256, "issue", d);
        } else if kinds.iter().any(|k| k == "publicationvolume") && t.b.volume.is_none() {
            t.b.volume = text(p.get("volumeNumber"), 256, "volume", d);
        } else if t.b.journal.is_none()
            && (kinds
                .iter()
                .any(|k| k == "periodical" || k == "publicationevent")
                || p.is_string())
        {
            t.b.journal = text(
                if p.is_string() {
                    Some(p)
                } else {
                    p.get("name")
                },
                4096,
                "journal",
                d,
            );
        }
        part = p.get("isPartOf");
    }
    t.b.pages = text(n.get("pagination"), 256, "pages", d).or_else(|| {
        pages(
            text(n.get("pageStart"), 128, "pages", d),
            text(n.get("pageEnd"), 128, "pages", d),
        )
    });
    t.b.abstract_text = text(
        n.get("abstract").or_else(|| n.get("description")),
        32 * 1024,
        "abstract",
        d,
    );
    t.b.language = text(n.get("inLanguage"), 64, "language", d);
    if types(n)
        .iter()
        .any(|k| k == "scholarlyarticle" || k == "medicalscholarlyarticle")
    {
        t.b.paper_type = Some("journal-article".into());
    }
    let mut ids: Vec<&Value> = json_list(n.get("identifier"));
    ids.extend(json_list(n.get("sameAs")));
    if let Some(id) = n.get("@id") {
        ids.push(id);
    }
    for id in ids.into_iter().take(64) {
        let (kind, value) = match id {
            Value::String(s) => (None, s.as_str()),
            Value::Object(o) => (
                o.get("propertyID")
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase),
                o.get("value").and_then(Value::as_str).unwrap_or(""),
            ),
            _ => continue,
        };
        let lower = value.trim().to_ascii_lowercase();
        match kind.as_deref() {
            Some("doi") => push_id(&mut t, "doi", doi_value(value), value, d),
            Some("pmid") | Some("pubmed") => push_id(&mut t, "pmid", pmid_value(value), value, d),
            Some("arxiv") => push_id(&mut t, "arxiv", arxiv_value(value), value, d),
            _ if is_doi_resolver(&lower) || lower.starts_with("doi:") => {
                push_id(&mut t, "doi", doi_value(value), value, d)
            }
            _ => {}
        }
    }
    for e in json_list(n.get("encoding"))
        .into_iter()
        .chain(json_list(n.get("associatedMedia")))
    {
        let format = e
            .get("encodingFormat")
            .and_then(Value::as_str)
            .unwrap_or("");
        if format.eq_ignore_ascii_case("application/pdf")
            && let Some(url) = e.get("contentUrl").and_then(Value::as_str)
        {
            t.pdfs.push(url.to_owned());
        }
    }
    t
}

fn add_id(
    identifiers: &mut Vec<FoundIdentifier>,
    ns: &str,
    value: String,
    tier: MetadataTier,
    d: &mut Vec<RowDiagnostic>,
) {
    match identifiers.iter().find(|i| i.namespace == ns) {
        Some(existing) if existing.value != value => diag(
            d,
            ns,
            format!(
                "conflicting {ns} identifiers on the page; kept the {:?} value",
                existing.tier
            ),
        ),
        Some(_) => {}
        None => identifiers.push(FoundIdentifier {
            namespace: ns.into(),
            value,
            tier,
        }),
    }
}

/// Extracts page metadata from the capture request. `policy` decides which PDF links are
/// acceptable fetch candidates (checked again, with DNS, when a fetch actually runs).
pub fn extract(request: &CaptureRequest, policy: &FetchPolicy) -> PageMetadata {
    let mut d = vec![];
    let scanned = request.html.as_deref().map(scan).unwrap_or_default();
    let mut meta: Vec<(String, String)> = request
        .meta
        .iter()
        .map(|m| (m.name.trim().to_ascii_lowercase(), m.content.clone()))
        .collect();
    meta.extend(scanned.meta.iter().cloned());
    meta.truncate(MAX_META_TAGS * 2);
    let mut blocks: Vec<String> = request.json_ld.clone();
    blocks.extend(scanned.json_ld.iter().cloned());
    blocks.truncate(MAX_JSON_LD_BLOCKS);
    let page = Url::parse(request.page_url.trim()).ok();
    let landing_url = public_url(request.page_url.trim());
    if landing_url.is_none() {
        diag(
            &mut d,
            "url",
            "page URL is not a public web URL; it is kept only as the capture source".into(),
        );
    }
    let tiers = [
        (MetadataTier::Highwire, highwire(&meta, &mut d)),
        (MetadataTier::JsonLd, json_ld(&blocks, &mut d)),
        (MetadataTier::DublinCore, dublin_core(&meta, &mut d)),
    ];
    let mut identifiers: Vec<FoundIdentifier> = vec![];
    for (tier, t) in &tiers {
        for (ns, v) in &t.ids {
            add_id(&mut identifiers, ns, v.clone(), *tier, &mut d);
        }
    }
    if let Some(page) = &page {
        for (ns, v) in url_identifiers(page) {
            add_id(&mut identifiers, ns, v, MetadataTier::PageUrl, &mut d);
        }
    }
    let mut link_dois: Vec<String> = scanned
        .doi_links
        .iter()
        .filter_map(|h| doi_value(h))
        .collect();
    link_dois.sort();
    link_dois.dedup();
    if !identifiers.iter().any(|i| i.namespace == "doi") {
        match link_dois.as_slice() {
            [one] => {
                add_id(
                    &mut identifiers,
                    "doi",
                    one.clone(),
                    MetadataTier::PageUrl,
                    &mut d,
                );
                diag(
                    &mut d,
                    "doi",
                    "DOI taken from the page's only DOI link; please confirm".into(),
                );
            }
            [] => {}
            many => diag(
                &mut d,
                "doi",
                format!(
                    "{} distinct DOI links (e.g. a reference list) were not used as the paper's DOI",
                    many.len()
                ),
            ),
        }
    }
    let og = first(&meta, &["og:title", "twitter:title"])
        .and_then(|t| clean(t, normalize::MAX_TITLE_BYTES, "title", &mut d));
    let document_title = og
        .or_else(|| {
            request
                .title
                .as_deref()
                .and_then(|t| clean(t, normalize::MAX_TITLE_BYTES, "title", &mut d))
        })
        .or_else(|| {
            scanned
                .title
                .as_deref()
                .and_then(|t| clean(t, normalize::MAX_TITLE_BYTES, "title", &mut d))
        });
    // PDF candidates: explicit choice first, then page metadata; each must pass the policy.
    let mut raw_pdfs: Vec<(String, PdfOrigin)> = vec![];
    if let Some(u) = &request.pdf_url {
        raw_pdfs.push((u.clone(), PdfOrigin::Explicit));
    }
    for (tier, t) in &tiers {
        let origin = if *tier == MetadataTier::Highwire {
            PdfOrigin::Highwire
        } else {
            PdfOrigin::JsonLd
        };
        raw_pdfs.extend(t.pdfs.iter().map(|u| (u.clone(), origin)));
    }
    for link in &scanned.links {
        let rel = attr(link, "rel").unwrap_or("").to_ascii_lowercase();
        if rel.split_whitespace().any(|r| r == "alternate")
            && attr(link, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("application/pdf"))
            && let Some(href) = attr(link, "href")
        {
            raw_pdfs.push((href.to_owned(), PdfOrigin::Link));
        }
    }
    let mut pdf_candidates: Vec<CapturePdf> = vec![];
    for (raw, origin) in raw_pdfs.into_iter().take(64) {
        let absolute = match &page {
            Some(base) => base.join(raw.trim()).map(String::from).unwrap_or(raw),
            None => raw,
        };
        match policy.check_url(&absolute) {
            Ok(url) => {
                let url = String::from(url);
                if pdf_candidates.len() < MAX_PDF_CANDIDATES
                    && !pdf_candidates.iter().any(|c| c.url == url)
                {
                    pdf_candidates.push(CapturePdf { url, origin });
                }
            }
            Err(e) => diag(&mut d, "pdf", format!("PDF link not offered: {e}")),
        }
    }
    let tiers = tiers
        .into_iter()
        .filter(|(_, t)| t.title.is_some() || t.b != Bibliography::default())
        .map(|(tier, t)| TierRecord {
            tier: Some(tier),
            title: t.title,
            bibliography: t.b,
        })
        .collect();
    PageMetadata {
        page_url: request.page_url.trim().to_owned(),
        landing_url,
        document_title,
        identifiers,
        tiers,
        pdf_candidates,
        diagnostics: d,
        html_sha256: request
            .html
            .as_ref()
            .map(|h| Sha256::digest(h.as_bytes()).into()),
    }
}
