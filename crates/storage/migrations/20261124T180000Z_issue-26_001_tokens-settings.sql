-- #26 persistent token/grant management and local API settings.
-- API/MCP/CLI tokens are #25 daemon grants: one credential table, one authorization path.
ALTER TABLE daemon_grants ADD COLUMN client_kind TEXT NOT NULL DEFAULT 'unspecified'
    CHECK(client_kind IN ('unspecified', 'desktop', 'api', 'mcp', 'cli', 'native-host'));
-- Scope names the token was issued with (display/re-issue); capabilities_json stays authority.
ALTER TABLE daemon_grants ADD COLUMN scopes_json TEXT
    CHECK(scopes_json IS NULL OR (json_valid(scopes_json) AND json_type(scopes_json) = 'array'));
-- Last successful authentication (minute granularity, written when the daemon exits).
ALTER TABLE daemon_grants ADD COLUMN last_used_at_ms INTEGER;
-- Time-limited, user-approved bulk elevation (at most 15 minutes, checked by the daemon).
ALTER TABLE daemon_grants ADD COLUMN elevated_until_ms INTEGER;
-- Re-issue chain: the grant this one replaced (the old one is revoked in the same step).
ALTER TABLE daemon_grants ADD COLUMN replaces TEXT REFERENCES daemon_grants(id) ON DELETE RESTRICT;
UPDATE daemon_grants SET client_kind = 'desktop' WHERE label = 'desktop-bootstrap';

-- The #8 api_tokens table was never written by any component and duplicates daemon_grants.
-- Any row of unknown provenance is carried over fail-closed: revoked, no capabilities.
INSERT INTO daemon_grants(id, token_sha256, label, capabilities_json, issued_by, created_at_ms,
                          expires_at_ms, revoked_at_ms, client_kind)
SELECT lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)), 2)
             || '-8' || substr(hex(randomblob(2)), 2) || '-' || hex(randomblob(6))),
       token_hash,
       CASE WHEN length(trim(name)) > 0 THEN substr(name, 1, 128) ELSE 'legacy token' END,
       '[]',
       NULL,
       coalesce(CAST(strftime('%s', created_at) AS INTEGER) * 1000, 0),
       NULL,
       coalesce(CAST(strftime('%s', revoked_at) AS INTEGER) * 1000,
                CAST(strftime('%s', 'now') AS INTEGER) * 1000),
       'api'
FROM api_tokens
WHERE token_hash NOT IN (SELECT token_sha256 FROM daemon_grants);
DROP TABLE api_tokens;

-- Singleton. The REST listener (#27) is OFF by default and binds loopback only.
CREATE TABLE api_settings (
    id INTEGER PRIMARY KEY NOT NULL CHECK(id = 1),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
    bind_address TEXT NOT NULL DEFAULT '127.0.0.1' CHECK(bind_address IN ('127.0.0.1', '::1')),
    port INTEGER NOT NULL DEFAULT 39271 CHECK(port BETWEEN 1024 AND 65535),
    export_dirs_json TEXT NOT NULL DEFAULT '[]'
        CHECK(json_valid(export_dirs_json) AND json_type(export_dirs_json) = 'array'),
    updated_at_ms INTEGER NOT NULL DEFAULT 0
);
INSERT INTO api_settings(id) VALUES (1);
