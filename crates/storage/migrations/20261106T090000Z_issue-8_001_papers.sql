CREATE TABLE papers (
    id TEXT PRIMARY KEY NOT NULL,
    type TEXT NOT NULL DEFAULT 'journal-article',
    title TEXT NOT NULL CHECK(length(trim(title)) > 0),
    abstract TEXT,
    year INTEGER,
    publication_date TEXT,
    journal TEXT,
    volume TEXT,
    issue TEXT,
    pages TEXT,
    doi_normalized TEXT UNIQUE,
    url TEXT,
    language TEXT,
    citation_key TEXT UNIQUE CHECK(citation_key IS NULL OR length(trim(citation_key)) > 0),
    extra_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(extra_json)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT
);
CREATE INDEX papers_visible_id ON papers(deleted_at, id);
CREATE TABLE people (
    id TEXT PRIMARY KEY NOT NULL,
    given_name TEXT,
    family_name TEXT,
    literal_name TEXT,
    orcid TEXT,
    CHECK(length(trim(coalesce(given_name, '') || coalesce(family_name, '') || coalesce(literal_name, ''))) > 0)
);
CREATE TABLE paper_people (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK(length(trim(role)) > 0),
    sort_order INTEGER NOT NULL CHECK(sort_order >= 0),
    PRIMARY KEY(paper_id, sort_order)
);
CREATE INDEX paper_people_person ON paper_people(person_id);
CREATE TABLE paper_external_ids (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL CHECK(length(trim(namespace)) > 0),
    value TEXT NOT NULL CHECK(length(trim(value)) > 0),
    sort_order INTEGER NOT NULL CHECK(sort_order >= 0),
    PRIMARY KEY(paper_id, namespace, value),
    UNIQUE(paper_id, sort_order)
);
CREATE INDEX paper_external_ids_lookup ON paper_external_ids(namespace, value);
