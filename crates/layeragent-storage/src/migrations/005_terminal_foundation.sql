ALTER TABLE sessions ADD COLUMN effort TEXT NOT NULL DEFAULT 'auto';
ALTER TABLE sessions ADD COLUMN bridge INTEGER NOT NULL DEFAULT 0;

ALTER TABLE tasks ADD COLUMN effort TEXT NOT NULL DEFAULT 'auto';
ALTER TABLE tasks ADD COLUMN mode TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE tasks ADD COLUMN bridge INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS context_checkpoints(
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  through_seq INTEGER NOT NULL,
  summary TEXT NOT NULL,
  kind TEXT NOT NULL,
  model TEXT,
  usage TEXT NOT NULL DEFAULT '{}',
  history_sha256 TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(session_id,through_seq,history_sha256)
);
CREATE INDEX IF NOT EXISTS context_checkpoints_session
  ON context_checkpoints(session_id,through_seq DESC);

CREATE TABLE IF NOT EXISTS maintenance_operations(
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  result TEXT,
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS maintenance_active
  ON maintenance_operations(session_id,kind,state);

CREATE TABLE IF NOT EXISTS bridge_messages(
  id TEXT PRIMARY KEY,
  root_turn_id TEXT NOT NULL,
  sender_agent_id TEXT,
  sender_name TEXT NOT NULL,
  recipient_agent_id TEXT,
  recipient_name TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  content TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending',
  created_at INTEGER NOT NULL,
  delivered_at INTEGER,
  UNIQUE(root_turn_id,sender_agent_id,sequence)
);
CREATE INDEX IF NOT EXISTS bridge_inbox
  ON bridge_messages(root_turn_id,recipient_agent_id,state,created_at,sequence);

ALTER TABLE memories ADD COLUMN retired_at INTEGER;
ALTER TABLE memories ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';
CREATE INDEX IF NOT EXISTS memories_active ON memories(principal,workspace,retired_at,id);
