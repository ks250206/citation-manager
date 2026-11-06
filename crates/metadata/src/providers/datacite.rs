use super::*;
use serde_json::Value;
#[derive(Deserialize)]
struct Envelope {
    data: Data,
}
#[derive(Deserialize)]
struct Data {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    attributes: Attributes,
}
#[derive(Deserialize)]
struct Title {
    title: String,
    #[serde(rename = "titleType")]
    title_type: Option<String>,
}
#[derive(Deserialize)]
struct Person {
    name: String,
    #[serde(rename = "givenName")]
    given: Option<String>,
    #[serde(rename = "familyName")]
    family: Option<String>,
    #[serde(rename = "nameType")]
    name_type: Option<String>,
    #[serde(rename = "contributorType")]
    contributor_type: Option<String>,
    #[serde(rename = "nameIdentifiers", default)]
    identifiers: Vec<NameIdentifier>,
}
#[derive(Deserialize)]
struct NameIdentifier {
    #[serde(rename = "nameIdentifier")]
    value: String,
    #[serde(rename = "nameIdentifierScheme")]
    scheme: String,
}
impl Person {
    fn contributor(self, creator: bool) -> Result<Contributor, ()> {
        if self.identifiers.len() > 32 {
            return Err(());
        }
        let role = if creator {
            "author".to_owned()
        } else {
            self.contributor_type.ok_or(())?.to_lowercase()
        };
        let literal = self.name_type.as_deref() == Some("Organizational")
            || (self.given.is_none() && self.family.is_none());
        Ok(Contributor {
            given_name: if literal { None } else { self.given },
            family_name: if literal { None } else { self.family },
            literal_name: literal.then_some(self.name),
            orcid: self
                .identifiers
                .into_iter()
                .find(|i| i.scheme.eq_ignore_ascii_case("orcid"))
                .map(|i| i.value),
            role,
        })
    }
}
#[derive(Deserialize)]
struct Date {
    date: String,
    #[serde(rename = "dateType")]
    kind: String,
}
#[derive(Deserialize)]
struct Description {
    description: String,
    #[serde(rename = "descriptionType")]
    kind: String,
}
#[derive(Deserialize)]
struct Identifier {
    identifier: String,
    #[serde(rename = "identifierType")]
    kind: String,
}
#[derive(Deserialize)]
struct Types {
    #[serde(rename = "resourceTypeGeneral")]
    general: Option<String>,
    #[serde(rename = "resourceType")]
    specific: Option<String>,
}
#[derive(Deserialize)]
struct Container {
    title: Option<String>,
    volume: Option<String>,
    issue: Option<String>,
    #[serde(rename = "firstPage")]
    first: Option<String>,
    #[serde(rename = "lastPage")]
    last: Option<String>,
}
#[derive(Deserialize)]
struct Attributes {
    doi: String,
    #[serde(default)]
    titles: Vec<Title>,
    #[serde(default)]
    creators: Vec<Person>,
    #[serde(default)]
    contributors: Vec<Person>,
    #[serde(rename = "publicationYear")]
    year: Option<i32>,
    #[serde(default)]
    dates: Vec<Date>,
    #[serde(default)]
    descriptions: Vec<Description>,
    url: Option<String>,
    language: Option<String>,
    types: Option<Types>,
    container: Option<Container>,
    #[serde(rename = "contentUrl")]
    content_url: Option<Vec<String>>,
    #[serde(default)]
    identifiers: Vec<Identifier>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}
pub(super) fn parse(body: &[u8], source: ImportSource) -> Result<ProviderRecord, ()> {
    let envelope: Envelope = serde_json::from_slice(body).map_err(|_| ())?;
    let data = envelope.data;
    if data.kind != "dois" {
        return Err(());
    }
    let a = data.attributes;
    if a.creators.len() + a.contributors.len() > 256
        || a.content_url.as_ref().is_some_and(|u| u.len() > 32)
        || a.identifiers.len() > 127
        || a.titles.len() > 32
        || a.dates.len() > 32
        || a.descriptions.len() > 32
    {
        return Err(());
    }
    let mut draft = PaperDraft {
        source,
        ..Default::default()
    };
    raw_doi(&mut draft, a.doi);
    draft.external_ids.push(ExternalId {
        namespace: "doi".into(),
        value: data.id,
    });
    for id in a.identifiers {
        draft.external_ids.push(ExternalId {
            namespace: id.kind,
            value: id.identifier,
        });
    }
    draft.title = a
        .titles
        .iter()
        .find(|t| t.title_type.is_none())
        .or(a.titles.first())
        .map(|t| t.title.clone())
        .unwrap_or_default();
    let b = &mut draft.bibliography;
    b.authors = a
        .creators
        .into_iter()
        .map(|p| p.contributor(true))
        .chain(a.contributors.into_iter().map(|p| p.contributor(false)))
        .collect::<Result<_, _>>()?;
    b.year = a.year;
    b.publication_date = a
        .dates
        .iter()
        .find(|d| d.kind == "Issued")
        .map(|d| d.date.clone())
        .or(a.year.map(|y| format!("{y:04}")));
    b.abstract_text = a
        .descriptions
        .iter()
        .find(|d| d.kind == "Abstract")
        .map(|d| d.description.clone());
    b.url = a.url;
    b.language = a.language;
    if let Some(url) = &b.url
        && public_url(url).is_none()
    {
        return Err(());
    }
    if let Some(t) = a.types {
        b.paper_type = t.general.map(|g| match g.as_str() {
            "JournalArticle" => "journal-article".into(),
            "ConferencePaper" => "proceedings-article".into(),
            "Book" => "book".into(),
            "Dataset" => "dataset".into(),
            _ => t.specific.unwrap_or(g),
        });
    }
    if let Some(c) = a.container {
        b.journal = c.title;
        b.volume = c.volume;
        b.issue = c.issue;
        b.pages = c.first.map(|first| {
            c.last
                .map_or_else(|| first.clone(), |last| format!("{first}-{last}"))
        });
    }
    b.extra_json = Some(
        serde_json::json!({"provider_fields":a.extra, "titles":a.titles.iter().map(|t|&t.title).collect::<Vec<_>>(), "dates":a.dates.iter().map(|d|serde_json::json!({"date":d.date,"type":d.kind})).collect::<Vec<_>>()}),
    );
    let pdf_candidates = a
        .content_url
        .unwrap_or_default()
        .into_iter()
        .filter(|u| {
            reqwest::Url::parse(u)
                .is_ok_and(|url| url.path().to_ascii_lowercase().ends_with(".pdf"))
        })
        .filter_map(|u| pdf(&u, Provider::DataCite, &draft.source))
        .collect();
    Ok(ProviderRecord {
        draft,
        pdf_candidates,
    })
}
