use super::*;
#[derive(Deserialize)]
struct Envelope {
    status: String,
    #[serde(rename = "message-type")]
    message_type: String,
    message: Work,
}
#[derive(Deserialize)]
struct Date {
    #[serde(rename = "date-parts")]
    parts: Vec<Vec<i32>>,
}
#[derive(Deserialize)]
struct Link {
    #[serde(rename = "URL")]
    url: String,
    #[serde(rename = "content-type")]
    content_type: Option<String>,
}
#[derive(Deserialize)]
struct Work {
    #[serde(rename = "DOI")]
    doi: String,
    #[serde(default)]
    title: Vec<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    author: Vec<CrossrefPerson>,
    #[serde(default)]
    editor: Vec<CrossrefPerson>,
    issued: Option<Date>,
    #[serde(rename = "published-print")]
    print: Option<Date>,
    #[serde(rename = "published-online")]
    online: Option<Date>,
    #[serde(rename = "container-title", default)]
    container: Vec<String>,
    volume: Option<String>,
    issue: Option<String>,
    page: Option<String>,
    #[serde(rename = "article-number")]
    article_number: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(rename = "URL")]
    url: Option<String>,
    language: Option<String>,
    #[serde(default)]
    link: Vec<Link>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}
pub(super) fn parse(body: &[u8], source: ImportSource) -> Result<ProviderRecord, ()> {
    let envelope: Envelope = serde_json::from_slice(body).map_err(|_| ())?;
    if envelope.status != "ok" || envelope.message_type != "work" {
        return Err(());
    }
    let w = envelope.message;
    if w.author.len() + w.editor.len() > 256
        || w.link.len() > 32
        || w.title.len() > 32
        || w.container.len() > 32
    {
        return Err(());
    }
    let mut draft = PaperDraft {
        source,
        ..Default::default()
    };
    raw_doi(&mut draft, w.doi);
    draft.title = w.title.first().cloned().unwrap_or_default();
    let b = &mut draft.bibliography;
    b.paper_type = w.kind;
    b.authors = w
        .author
        .into_iter()
        .map(|p| p.contributor("author"))
        .chain(w.editor.into_iter().map(|p| p.contributor("editor")))
        .collect();
    if let Some(date) = w.issued.or(w.print).or(w.online) {
        if date.parts.len() != 1 || date.parts[0].is_empty() || date.parts[0].len() > 3 {
            return Err(());
        }
        let parts = &date.parts[0];
        b.year = Some(parts[0]);
        b.publication_date = Some(
            parts
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    if i == 0 {
                        format!("{v:04}")
                    } else {
                        format!("{v:02}")
                    }
                })
                .collect::<Vec<_>>()
                .join("-"),
        );
    }
    b.journal = w.container.first().cloned();
    b.volume = w.volume;
    b.issue = w.issue;
    b.pages = w.page.or(w.article_number);
    b.abstract_text = w.abstract_text;
    b.url = w.url;
    b.language = w.language;
    if let Some(url) = &b.url
        && public_url(url).is_none()
    {
        return Err(());
    }
    // Retain nonmapped provider metadata for review, including alternative titles and IDs.
    b.extra_json = Some(
        serde_json::json!({"other_titles":w.title.into_iter().skip(1).collect::<Vec<_>>(), "provider_fields": w.extra}),
    );
    let pdf_candidates = w
        .link
        .iter()
        .filter(|l| l.content_type.as_deref() == Some("application/pdf"))
        .filter_map(|l| pdf(&l.url, Provider::Crossref, &draft.source))
        .collect();
    Ok(ProviderRecord {
        draft,
        pdf_candidates,
    })
}
