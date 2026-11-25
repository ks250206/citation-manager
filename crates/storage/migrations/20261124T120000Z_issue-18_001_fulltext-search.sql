-- #18: per-attachment text extraction status, one search projection maintained only by
-- these triggers (same transaction as the source write), and two external-content FTS5
-- indexes over it. Source tables stay authoritative; the projection and FTS indexes can be
-- dropped and rebuilt from them at any time (see storage::fts::rebuild).
CREATE TABLE fulltext_extractions (
    attachment_id TEXT PRIMARY KEY NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    file_hash BLOB NOT NULL CHECK(length(file_hash) = 32),
    status TEXT NOT NULL CHECK(status IN (
        'extracting', 'complete', 'partial', 'no_text',
        'encrypted', 'malformed', 'too_large', 'failed'
    )),
    page_count INTEGER NOT NULL DEFAULT 0 CHECK(page_count >= 0),
    next_page INTEGER NOT NULL DEFAULT 0 CHECK(next_page >= 0),
    pages_with_text INTEGER NOT NULL DEFAULT 0 CHECK(pages_with_text >= 0),
    truncated_pages INTEGER NOT NULL DEFAULT 0 CHECK(truncated_pages >= 0),
    -- Consecutive transient failures (time budget, read error) without progress; the
    -- attachment is retried by later runs and marked failed after a bounded number.
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    detail TEXT,
    updated_at TEXT NOT NULL
);
CREATE INDEX fulltext_extractions_status ON fulltext_extractions(status);

-- Single row. `suspended` is set only inside the rebuild transaction so that clearing and
-- refilling the projection does not issue per-row FTS deletes against a damaged index.
CREATE TABLE search_index_state (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    suspended INTEGER NOT NULL DEFAULT 0 CHECK(suspended IN (0, 1)),
    generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
    rebuilt_at TEXT
);
INSERT INTO search_index_state(id) VALUES (1);

-- Index projection: one row per paper (bibliographic fields), annotation (quote + comment)
-- and extracted PDF page with text. Soft-deleted papers keep their rows; visibility is
-- applied at query time by joining papers.deleted_at, so delete/restore are immediate.
CREATE TABLE search_documents (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL CHECK(source IN ('paper', 'annotation', 'page')),
    source_key TEXT NOT NULL,
    paper_id TEXT NOT NULL,
    attachment_id TEXT,
    page_index INTEGER,
    page_label TEXT,
    title TEXT,
    authors TEXT,
    abstract TEXT,
    journal TEXT,
    tags TEXT,
    identifiers TEXT,
    notes TEXT,
    body TEXT,
    UNIQUE(source, source_key)
);
CREATE INDEX search_documents_paper ON search_documents(paper_id, source);
CREATE INDEX search_documents_attachment ON search_documents(attachment_id);

CREATE VIEW search_paper_projection AS
SELECT
    p.id AS paper_id,
    p.title AS title,
    (SELECT group_concat(
            trim(coalesce(pe.literal_name,
                trim(coalesce(pe.given_name, '') || ' ' || coalesce(pe.family_name, '')))),
            char(10) ORDER BY pp.sort_order)
       FROM paper_people pp JOIN people pe ON pe.id = pp.person_id
      WHERE pp.paper_id = p.id) AS authors,
    p.abstract AS abstract,
    p.journal AS journal,
    (SELECT group_concat(t.name, char(10) ORDER BY t.normalized_name)
       FROM paper_tags pt JOIN tags t ON t.id = pt.tag_id
      WHERE pt.paper_id = p.id) AS tags,
    nullif(trim(coalesce(p.doi_normalized, '') || char(10) || coalesce(p.url, '') || char(10)
        || coalesce(p.citation_key, ''), char(10)), '') AS identifiers
FROM papers p;

CREATE VIEW search_annotation_projection AS
SELECT
    an.id AS annotation_id,
    a.paper_id AS paper_id,
    an.attachment_id AS attachment_id,
    an.page_index AS page_index,
    an.page_label AS page_label,
    trim(an.quote_text || char(10) || coalesce(an.comment, ''), char(10)) AS notes
FROM annotations an JOIN attachments a ON a.id = an.attachment_id;

CREATE VIEW search_page_projection AS
SELECT
    fp.id AS page_id,
    a.paper_id AS paper_id,
    fp.attachment_id AS attachment_id,
    fp.page_index AS page_index,
    fp.page_label AS page_label,
    fp.text AS body
FROM fulltext_pages fp JOIN attachments a ON a.id = fp.attachment_id
WHERE fp.extraction_status = 'complete' AND length(fp.text) > 0;

-- unicode61 covers every searchable field (including DOI/URL/citation-key identifiers).
-- trigram covers the explicit substring targets: title, contributor display names,
-- abstract, journal, tags, annotation quote/comment and PDF page text.
CREATE VIRTUAL TABLE search_fts_words USING fts5(
    title, authors, abstract, journal, tags, identifiers, notes, body,
    content = 'search_documents', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 1'
);
CREATE VIRTUAL TABLE search_fts_trigram USING fts5(
    title, authors, abstract, journal, tags, notes, body,
    content = 'search_documents', content_rowid = 'id',
    tokenize = 'trigram'
);

CREATE TRIGGER search_documents_ai AFTER INSERT ON search_documents
WHEN (SELECT suspended FROM search_index_state WHERE id = 1) = 0
BEGIN
    INSERT INTO search_fts_words(rowid, title, authors, abstract, journal, tags, identifiers, notes, body)
    VALUES (new.id, new.title, new.authors, new.abstract, new.journal, new.tags, new.identifiers, new.notes, new.body);
    INSERT INTO search_fts_trigram(rowid, title, authors, abstract, journal, tags, notes, body)
    VALUES (new.id, new.title, new.authors, new.abstract, new.journal, new.tags, new.notes, new.body);
END;
CREATE TRIGGER search_documents_ad AFTER DELETE ON search_documents
WHEN (SELECT suspended FROM search_index_state WHERE id = 1) = 0
BEGIN
    INSERT INTO search_fts_words(search_fts_words, rowid, title, authors, abstract, journal, tags, identifiers, notes, body)
    VALUES ('delete', old.id, old.title, old.authors, old.abstract, old.journal, old.tags, old.identifiers, old.notes, old.body);
    INSERT INTO search_fts_trigram(search_fts_trigram, rowid, title, authors, abstract, journal, tags, notes, body)
    VALUES ('delete', old.id, old.title, old.authors, old.abstract, old.journal, old.tags, old.notes, old.body);
END;
CREATE TRIGGER search_documents_au AFTER UPDATE ON search_documents
WHEN (SELECT suspended FROM search_index_state WHERE id = 1) = 0
BEGIN
    INSERT INTO search_fts_words(search_fts_words, rowid, title, authors, abstract, journal, tags, identifiers, notes, body)
    VALUES ('delete', old.id, old.title, old.authors, old.abstract, old.journal, old.tags, old.identifiers, old.notes, old.body);
    INSERT INTO search_fts_trigram(search_fts_trigram, rowid, title, authors, abstract, journal, tags, notes, body)
    VALUES ('delete', old.id, old.title, old.authors, old.abstract, old.journal, old.tags, old.notes, old.body);
    INSERT INTO search_fts_words(rowid, title, authors, abstract, journal, tags, identifiers, notes, body)
    VALUES (new.id, new.title, new.authors, new.abstract, new.journal, new.tags, new.identifiers, new.notes, new.body);
    INSERT INTO search_fts_trigram(rowid, title, authors, abstract, journal, tags, notes, body)
    VALUES (new.id, new.title, new.authors, new.abstract, new.journal, new.tags, new.notes, new.body);
END;

-- Papers: bibliographic document lifecycle. Soft delete/restore only changes deleted_at.
CREATE TRIGGER search_papers_ai AFTER INSERT ON papers
BEGIN
    INSERT INTO search_documents(source, source_key, paper_id, title, authors, abstract, journal, tags, identifiers)
    SELECT 'paper', v.paper_id, v.paper_id, v.title, v.authors, v.abstract, v.journal, v.tags, v.identifiers
      FROM search_paper_projection v WHERE v.paper_id = new.id;
END;
CREATE TRIGGER search_papers_au AFTER UPDATE OF title, abstract, journal, doi_normalized, url, citation_key ON papers
BEGIN
    UPDATE search_documents
       SET (title, authors, abstract, journal, tags, identifiers) =
           (SELECT v.title, v.authors, v.abstract, v.journal, v.tags, v.identifiers
              FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key = new.id;
END;
CREATE TRIGGER search_papers_ad AFTER DELETE ON papers
BEGIN
    DELETE FROM search_documents WHERE paper_id = old.id;
END;

-- Contributors and tags are folded into the paper document.
CREATE TRIGGER search_paper_people_ai AFTER INSERT ON paper_people
BEGIN
    UPDATE search_documents
       SET authors = (SELECT v.authors FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key = new.paper_id;
END;
CREATE TRIGGER search_paper_people_au AFTER UPDATE ON paper_people
BEGIN
    UPDATE search_documents
       SET authors = (SELECT v.authors FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key IN (old.paper_id, new.paper_id);
END;
CREATE TRIGGER search_paper_people_ad AFTER DELETE ON paper_people
BEGIN
    UPDATE search_documents
       SET authors = (SELECT v.authors FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key = old.paper_id;
END;
CREATE TRIGGER search_people_au AFTER UPDATE ON people
BEGIN
    UPDATE search_documents
       SET authors = (SELECT v.authors FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key IN (SELECT pp.paper_id FROM paper_people pp WHERE pp.person_id = new.id);
END;
CREATE TRIGGER search_paper_tags_ai AFTER INSERT ON paper_tags
BEGIN
    UPDATE search_documents
       SET tags = (SELECT v.tags FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key = new.paper_id;
END;
CREATE TRIGGER search_paper_tags_ad AFTER DELETE ON paper_tags
BEGIN
    UPDATE search_documents
       SET tags = (SELECT v.tags FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key = old.paper_id;
END;
CREATE TRIGGER search_tags_au AFTER UPDATE OF name, normalized_name ON tags
BEGIN
    UPDATE search_documents
       SET tags = (SELECT v.tags FROM search_paper_projection v WHERE v.paper_id = search_documents.source_key)
     WHERE source = 'paper' AND source_key IN (SELECT pt.paper_id FROM paper_tags pt WHERE pt.tag_id = new.id);
END;

-- Annotations (highlight quote/comment and notes).
CREATE TRIGGER search_annotations_ai AFTER INSERT ON annotations
BEGIN
    INSERT INTO search_documents(source, source_key, paper_id, attachment_id, page_index, page_label, notes)
    SELECT 'annotation', v.annotation_id, v.paper_id, v.attachment_id, v.page_index, v.page_label, v.notes
      FROM search_annotation_projection v WHERE v.annotation_id = new.id;
END;
CREATE TRIGGER search_annotations_au AFTER UPDATE ON annotations
BEGIN
    UPDATE search_documents
       SET (source_key, paper_id, attachment_id, page_index, page_label, notes) =
           (SELECT v.annotation_id, v.paper_id, v.attachment_id, v.page_index, v.page_label, v.notes
              FROM search_annotation_projection v WHERE v.annotation_id = new.id)
     WHERE source = 'annotation' AND source_key = old.id;
END;
CREATE TRIGGER search_annotations_ad AFTER DELETE ON annotations
BEGIN
    DELETE FROM search_documents WHERE source = 'annotation' AND source_key = old.id;
END;

-- Extracted PDF pages. Only complete pages with text are indexed.
CREATE TRIGGER search_pages_ai AFTER INSERT ON fulltext_pages
BEGIN
    INSERT INTO search_documents(source, source_key, paper_id, attachment_id, page_index, page_label, body)
    SELECT 'page', CAST(v.page_id AS TEXT), v.paper_id, v.attachment_id, v.page_index, v.page_label, v.body
      FROM search_page_projection v WHERE v.page_id = new.id;
END;
CREATE TRIGGER search_pages_au AFTER UPDATE ON fulltext_pages
BEGIN
    DELETE FROM search_documents WHERE source = 'page' AND source_key = CAST(old.id AS TEXT);
    INSERT INTO search_documents(source, source_key, paper_id, attachment_id, page_index, page_label, body)
    SELECT 'page', CAST(v.page_id AS TEXT), v.paper_id, v.attachment_id, v.page_index, v.page_label, v.body
      FROM search_page_projection v WHERE v.page_id = new.id;
END;
CREATE TRIGGER search_pages_ad AFTER DELETE ON fulltext_pages
BEGIN
    DELETE FROM search_documents WHERE source = 'page' AND source_key = CAST(old.id AS TEXT);
END;

-- An attachment moved to another paper carries its annotation/page documents along.
CREATE TRIGGER search_attachments_au AFTER UPDATE OF paper_id ON attachments
BEGIN
    UPDATE search_documents SET paper_id = new.paper_id WHERE attachment_id = new.id;
END;
CREATE TRIGGER search_attachments_ad AFTER DELETE ON attachments
BEGIN
    DELETE FROM search_documents WHERE attachment_id = old.id;
END;

-- Existing libraries: build the projection and both indexes from current source rows.
INSERT INTO search_documents(source, source_key, paper_id, title, authors, abstract, journal, tags, identifiers)
SELECT 'paper', paper_id, paper_id, title, authors, abstract, journal, tags, identifiers FROM search_paper_projection;
INSERT INTO search_documents(source, source_key, paper_id, attachment_id, page_index, page_label, notes)
SELECT 'annotation', annotation_id, paper_id, attachment_id, page_index, page_label, notes FROM search_annotation_projection;
INSERT INTO search_documents(source, source_key, paper_id, attachment_id, page_index, page_label, body)
SELECT 'page', CAST(page_id AS TEXT), paper_id, attachment_id, page_index, page_label, body FROM search_page_projection;
