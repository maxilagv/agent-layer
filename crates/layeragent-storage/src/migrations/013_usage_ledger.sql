-- Spending controls. A task's budget covers its whole agent tree; the
-- ledger keeps every provider call's usage so a task's spending can be
-- broken down by model, by agent depth and by cache use.
ALTER TABLE tasks ADD COLUMN budget_usd REAL;
CREATE TABLE usage_ledger(
  id INTEGER PRIMARY KEY,
  root_turn_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  depth INTEGER NOT NULL,
  model TEXT NOT NULL,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL NOT NULL,
  at INTEGER NOT NULL
);
CREATE INDEX usage_ledger_root ON usage_ledger(root_turn_id, at);
