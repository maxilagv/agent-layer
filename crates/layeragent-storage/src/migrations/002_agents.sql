CREATE TABLE IF NOT EXISTS agents(
  id TEXT PRIMARY KEY,
  root_turn_id TEXT NOT NULL,
  parent_turn_id TEXT NOT NULL,
  parent_agent_id TEXT REFERENCES agents(id),
  name TEXT NOT NULL,
  task TEXT NOT NULL,
  depth INTEGER NOT NULL,
  session_id TEXT REFERENCES sessions(id),
  workspace TEXT NOT NULL,
  isolation TEXT NOT NULL,
  status TEXT NOT NULL,
  result TEXT,
  usage TEXT,
  created_at INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS agents_root ON agents(root_turn_id,created_at);
CREATE INDEX IF NOT EXISTS agents_parent ON agents(parent_turn_id);
