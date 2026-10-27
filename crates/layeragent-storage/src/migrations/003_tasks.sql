-- Durable task identity, operator corrections, connection authority and
-- per-task measurements. Nothing here depends on a client connection.
CREATE TABLE IF NOT EXISTS tasks(
  id TEXT PRIMARY KEY,
  tenant TEXT NOT NULL,
  principal TEXT NOT NULL,
  project TEXT NOT NULL,
  session_id TEXT,
  turn_id TEXT,
  objective TEXT NOT NULL,
  executor TEXT NOT NULL,
  state TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  usage TEXT NOT NULL DEFAULT '{}',
  cancellation TEXT NOT NULL DEFAULT 'none',
  correction TEXT,
  error TEXT,
  last_event_seq INTEGER NOT NULL DEFAULT 0,
  idempotency_key TEXT,
  request_sha256 TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS tasks_idempotency
  ON tasks(tenant,principal,idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS tasks_scope ON tasks(tenant,principal,created_at,id);
CREATE INDEX IF NOT EXISTS tasks_turn ON tasks(turn_id);

CREATE TABLE IF NOT EXISTS task_events(
  seq INTEGER PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id),
  at INTEGER NOT NULL,
  kind TEXT NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS task_events_task ON task_events(task_id,seq);
CREATE TRIGGER IF NOT EXISTS task_event_apply AFTER INSERT ON task_events BEGIN
  UPDATE tasks SET
    last_event_seq=NEW.seq,
    updated_at=NEW.at,
    revision=revision+1,
    state=CASE NEW.kind
      WHEN 'task_prepared' THEN 'prepared'
      WHEN 'turn_started' THEN 'running'
      WHEN 'approval_required' THEN 'awaiting_permission'
      WHEN 'approval_resolved' THEN CASE WHEN state='awaiting_permission' THEN 'running' ELSE state END
      WHEN 'tool_started' THEN 'running'
      WHEN 'task_blocked' THEN 'blocked'
      WHEN 'turn_completed' THEN 'completed'
      WHEN 'task_failed' THEN 'failed'
      WHEN 'failed' THEN 'failed'
      WHEN 'task_cancelled' THEN 'cancelled'
      WHEN 'interrupted' THEN CASE WHEN cancellation IN ('requested','uncertain') THEN 'cancelled' ELSE 'blocked' END
      ELSE state END
  WHERE id=NEW.task_id;
END;

CREATE TABLE IF NOT EXISTS task_corrections(
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id),
  instruction TEXT NOT NULL,
  expected_revision INTEGER NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  applied_at INTEGER
);
CREATE INDEX IF NOT EXISTS task_corrections_pending ON task_corrections(task_id,status,created_at);

CREATE TABLE IF NOT EXISTS task_metrics(
  task_id TEXT PRIMARY KEY REFERENCES tasks(id),
  model TEXT,
  queue_ms INTEGER NOT NULL DEFAULT 0,
  engine_ms INTEGER NOT NULL DEFAULT 0,
  tool_ms INTEGER NOT NULL DEFAULT 0,
  provider_ms INTEGER NOT NULL DEFAULT 0,
  network_ms INTEGER NOT NULL DEFAULT 0,
  first_content_ms INTEGER,
  total_ms INTEGER NOT NULL DEFAULT 0,
  max_rss_bytes INTEGER NOT NULL DEFAULT 0,
  cpu_ms INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL NOT NULL DEFAULT 0,
  measured_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_links(
  hash TEXT NOT NULL,
  tenant TEXT NOT NULL,
  principal TEXT NOT NULL,
  session_id TEXT,
  task_id TEXT,
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS artifact_links_scope
  ON artifact_links(hash,tenant,principal,COALESCE(session_id,''),COALESCE(task_id,''));

CREATE TABLE IF NOT EXISTS connection_states(
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  generation INTEGER NOT NULL DEFAULT 1,
  updated_at INTEGER NOT NULL,
  last_probe TEXT
);
