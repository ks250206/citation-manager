CREATE TABLE daemon_grants (
    id TEXT PRIMARY KEY NOT NULL,
    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
    label TEXT NOT NULL CHECK(length(trim(label)) > 0),
    capabilities_json TEXT NOT NULL CHECK(json_valid(capabilities_json) AND json_type(capabilities_json) = 'array'),
    issued_by TEXT REFERENCES daemon_grants(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER,
    revoked_at_ms INTEGER
);
CREATE TABLE daemon_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK(length(trim(kind)) > 0),
    idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 512),
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    owner_grant TEXT REFERENCES daemon_grants(id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'retry_wait', 'succeeded', 'failed', 'cancelled')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    max_attempts INTEGER NOT NULL CHECK(max_attempts BETWEEN 1 AND 20),
    progress_done INTEGER NOT NULL DEFAULT 0 CHECK(progress_done >= 0),
    progress_total INTEGER CHECK(progress_total IS NULL OR progress_total >= 0),
    progress_message TEXT,
    error TEXT,
    result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
    detected_committed INTEGER NOT NULL DEFAULT 0 CHECK(detected_committed IN (0, 1)),
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0, 1)),
    lease_owner TEXT,
    lease_generation INTEGER NOT NULL DEFAULT 0 CHECK(lease_generation >= 0),
    lease_expires_at_ms INTEGER,
    next_attempt_at_ms INTEGER,
    deadline_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK((state = 'running') = (lease_owner IS NOT NULL AND lease_expires_at_ms IS NOT NULL))
);
CREATE INDEX daemon_jobs_runnable ON daemon_jobs(state, next_attempt_at_ms, created_at_ms);
CREATE INDEX daemon_jobs_owner ON daemon_jobs(owner_grant, created_at_ms);
-- Idempotency is scoped to the submitting grant: another grant's key never matches.
CREATE UNIQUE INDEX daemon_jobs_idempotency ON daemon_jobs(coalesce(owner_grant, ''), kind, idempotency_key);
