-- #19: annotations. When a person accepts an AI-proposed annotation it keeps source='ai'
-- (provenance) and records the time here. NULL for unreviewed AI and for other sources.
ALTER TABLE annotations ADD COLUMN confirmed_at TEXT
    CHECK(confirmed_at IS NULL OR source = 'ai');
-- Lists by attachment in page order.
CREATE INDEX annotations_attachment_page ON annotations(attachment_id, page_index, created_at);
