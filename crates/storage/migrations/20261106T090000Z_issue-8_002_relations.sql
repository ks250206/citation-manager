CREATE TABLE collections (
    id TEXT PRIMARY KEY NOT NULL,
    parent_id TEXT REFERENCES collections(id) ON DELETE RESTRICT,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    sort_order INTEGER NOT NULL DEFAULT 0,
    CHECK(parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX collections_parent ON collections(parent_id);
CREATE TABLE paper_collections (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    PRIMARY KEY(paper_id, collection_id)
);
CREATE INDEX paper_collections_collection ON paper_collections(collection_id);
CREATE TABLE tags (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL UNIQUE CHECK(length(trim(normalized_name)) > 0),
    color TEXT
);
CREATE TABLE paper_tags (
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY(paper_id, tag_id)
);
CREATE INDEX paper_tags_tag ON paper_tags(tag_id);
CREATE TABLE attachments (
    id TEXT PRIMARY KEY NOT NULL,
    paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
    kind TEXT NOT NULL DEFAULT 'pdf',
    file_hash BLOB NOT NULL CHECK(length(file_hash) = 32),
    relative_path TEXT NOT NULL,
    original_name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    byte_size INTEGER NOT NULL CHECK(byte_size >= 0),
    source_url TEXT,
    added_at TEXT NOT NULL
);
CREATE INDEX attachments_paper ON attachments(paper_id);
CREATE INDEX attachments_hash ON attachments(file_hash);
CREATE TABLE fulltext_pages (
    id INTEGER PRIMARY KEY,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    page_index INTEGER NOT NULL CHECK(page_index BETWEEN 0 AND 4294967294),
    page_label TEXT,
    text TEXT NOT NULL DEFAULT '',
    extraction_status TEXT NOT NULL DEFAULT 'pending'
        CHECK(extraction_status IN ('pending', 'complete', 'failed', 'unavailable')),
    updated_at TEXT NOT NULL,
    UNIQUE(attachment_id, page_index)
);
CREATE TABLE annotations (
    id TEXT PRIMARY KEY NOT NULL,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    page_index INTEGER CHECK(page_index BETWEEN 0 AND 4294967294),
    page_label TEXT,
    type TEXT NOT NULL CHECK(type IN ('highlight', 'note')),
    source TEXT NOT NULL CHECK(source IN ('human', 'ai', 'import')),
    anchor_state TEXT NOT NULL CHECK(anchor_state IN ('resolved', 'unresolved')),
    unresolved_reason TEXT,
    quote_text TEXT NOT NULL,
    context_before TEXT,
    context_after TEXT,
    quads_json TEXT CHECK(quads_json IS NULL OR (json_valid(quads_json) AND json_type(quads_json) = 'array')),
    color TEXT,
    comment TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK((anchor_state = 'resolved' AND page_index IS NOT NULL AND unresolved_reason IS NULL)
       OR (anchor_state = 'unresolved' AND page_index IS NULL AND page_label IS NULL AND quads_json IS NULL
           AND unresolved_reason IS NOT NULL AND length(trim(unresolved_reason)) > 0))
);
CREATE INDEX annotations_attachment ON annotations(attachment_id);
CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    type TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    state TEXT NOT NULL CHECK(state IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
    progress REAL NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 1),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX jobs_state ON jobs(state);
CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE CHECK(length(token_hash) = 32),
    scopes_json TEXT NOT NULL CHECK(json_valid(scopes_json) AND json_type(scopes_json) = 'array'),
    created_at TEXT NOT NULL,
    revoked_at TEXT
);
