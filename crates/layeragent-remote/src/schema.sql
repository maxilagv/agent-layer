CREATE TABLE IF NOT EXISTS la_remote_authority (
 singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK(singleton), account TEXT NOT NULL,
 epoch TEXT NOT NULL, owner_key TEXT NOT NULL, control_sequence BIGINT NOT NULL DEFAULT 0, schema_version INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS la_remote_runners (
 id TEXT PRIMARY KEY, project TEXT NOT NULL, device TEXT NOT NULL,
 certificate TEXT NOT NULL UNIQUE, signing_key TEXT NOT NULL,
 enrollment JSONB NOT NULL, revoked BOOLEAN NOT NULL DEFAULT FALSE,
 last_seen TIMESTAMPTZ
);
CREATE TABLE IF NOT EXISTS la_remote_pairings (
 nonce TEXT PRIMARY KEY, runner TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS la_remote_jobs (
 id TEXT PRIMARY KEY, runner TEXT NOT NULL REFERENCES la_remote_runners(id),
 project TEXT NOT NULL, envelope JSONB NOT NULL, digest TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('pending','leased','completed','failed','cancelled','unknown')),
 retry_safe BOOLEAN NOT NULL, deadline_ms BIGINT NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
 attempt BIGINT NOT NULL DEFAULT 0, generation TEXT,
 renewal BIGINT NOT NULL DEFAULT 0, expires_at TIMESTAMPTZ,
 outcome TEXT, result JSONB
);
CREATE INDEX IF NOT EXISTS la_remote_pending ON la_remote_jobs(runner,status,created_at);
CREATE TABLE IF NOT EXISTS la_remote_controls (
 id TEXT PRIMARY KEY, digest TEXT NOT NULL, envelope JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS la_remote_events (
 seq BIGSERIAL PRIMARY KEY, created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
 kind TEXT NOT NULL, target TEXT NOT NULL, state TEXT NOT NULL
);
