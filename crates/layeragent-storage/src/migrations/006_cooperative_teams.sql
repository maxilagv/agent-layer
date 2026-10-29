ALTER TABLE tasks ADD COLUMN execution TEXT NOT NULL DEFAULT 'delegated';
ALTER TABLE tasks ADD COLUMN team_id TEXT;
ALTER TABLE tasks ADD COLUMN cooperative TEXT;

CREATE TABLE teams(
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id),
  tenant TEXT NOT NULL,
  principal TEXT NOT NULL,
  workspace TEXT NOT NULL,
  template TEXT NOT NULL,
  template_version INTEGER NOT NULL,
  state TEXT NOT NULL,
  reason TEXT,
  revision INTEGER NOT NULL DEFAULT 1,
  options TEXT NOT NULL,
  snapshot_sha256 TEXT,
  usage TEXT NOT NULL DEFAULT '{}',
  active_ms INTEGER NOT NULL DEFAULT 0,
  active_since INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX teams_scope ON teams(tenant,principal,task_id);

CREATE TABLE team_templates(
  team_id TEXT NOT NULL REFERENCES teams(id),
  name TEXT NOT NULL,
  version INTEGER NOT NULL,
  instructions TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(team_id,name,version)
);

CREATE TABLE team_members(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  name TEXT NOT NULL,
  role TEXT NOT NULL,
  model TEXT,
  session_id TEXT REFERENCES sessions(id),
  state TEXT NOT NULL,
  reason TEXT,
  work_state TEXT NOT NULL DEFAULT 'idle',
  current_work_id TEXT,
  activations INTEGER NOT NULL DEFAULT 0,
  initial_assessment TEXT,
  justification TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(team_id,name)
);
CREATE INDEX team_members_ready ON team_members(team_id,state,role);

CREATE TABLE team_activations(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  member_id TEXT NOT NULL REFERENCES team_members(id),
  generation INTEGER NOT NULL,
  lease_owner TEXT NOT NULL,
  lease_expires_at INTEGER NOT NULL,
  state TEXT NOT NULL,
  reason TEXT,
  usage TEXT NOT NULL DEFAULT '{}',
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(team_id,member_id,generation)
);
CREATE UNIQUE INDEX one_live_activation_per_member ON team_activations(member_id) WHERE state IN ('starting','running');
CREATE INDEX live_team_activations ON team_activations(team_id,state);

CREATE TABLE team_messages(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  sender_key TEXT NOT NULL,
  sender_member_id TEXT REFERENCES team_members(id),
  sender_kind TEXT NOT NULL,
  recipient_member_id TEXT REFERENCES team_members(id),
  purpose TEXT NOT NULL,
  body TEXT NOT NULL,
  work_id TEXT,
  finding_id TEXT,
  evidence_id TEXT,
  reply_to TEXT REFERENCES team_messages(id),
  provenance TEXT NOT NULL DEFAULT '[]',
  state TEXT NOT NULL DEFAULT 'accepted',
  reason TEXT,
  revision INTEGER NOT NULL DEFAULT 1,
  idempotency_key TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  resolved_at INTEGER,
  resolved_by TEXT,
  resolution_reason TEXT,
  UNIQUE(team_id,sender_key,idempotency_key)
);
CREATE INDEX team_messages_page ON team_messages(team_id,created_at,id);
CREATE INDEX team_messages_reply ON team_messages(team_id,reply_to);

CREATE TABLE team_deliveries(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  message_id TEXT NOT NULL REFERENCES team_messages(id),
  recipient_member_id TEXT NOT NULL REFERENCES team_members(id),
  state TEXT NOT NULL DEFAULT 'accepted',
  reason TEXT,
  activation_id TEXT REFERENCES team_activations(id),
  generation INTEGER,
  context_seq INTEGER,
  created_at INTEGER NOT NULL,
  incorporated_at INTEGER,
  responded_at INTEGER,
  resolved_at INTEGER,
  UNIQUE(message_id,recipient_member_id)
);
CREATE INDEX team_delivery_inbox ON team_deliveries(recipient_member_id,state,created_at,id);
CREATE INDEX team_delivery_pending ON team_deliveries(team_id,state);

CREATE TABLE team_work(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  title TEXT NOT NULL,
  deliverable TEXT NOT NULL,
  assignee TEXT REFERENCES team_members(id),
  state TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 0,
  close_conditions TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  idempotency_key TEXT NOT NULL,
  created_by TEXT NOT NULL REFERENCES team_members(id),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(team_id,created_by,idempotency_key)
);
CREATE INDEX team_work_queue ON team_work(team_id,state,priority DESC,created_at);
CREATE TABLE team_work_dependencies(
  team_id TEXT NOT NULL REFERENCES teams(id),
  work_id TEXT NOT NULL REFERENCES team_work(id),
  depends_on TEXT NOT NULL REFERENCES team_work(id),
  PRIMARY KEY(work_id,depends_on)
);
CREATE INDEX team_work_dependencies_reverse ON team_work_dependencies(depends_on,work_id);

CREATE TABLE team_reservations(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  activation_id TEXT NOT NULL REFERENCES team_activations(id),
  category TEXT NOT NULL,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  estimated_cost_usd REAL NOT NULL DEFAULT 0,
  state TEXT NOT NULL,
  reason TEXT,
  actual_usage TEXT,
  created_at INTEGER NOT NULL,
  settled_at INTEGER
);
CREATE INDEX team_reservations_open ON team_reservations(team_id,state);

CREATE TABLE team_findings(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  claim TEXT NOT NULL,
  state TEXT NOT NULL,
  reason TEXT NOT NULL,
  author_member_id TEXT NOT NULL REFERENCES team_members(id),
  reviewer_member_id TEXT REFERENCES team_members(id),
  snapshot_sha256 TEXT,
  valid_for_snapshot TEXT,
  validity TEXT NOT NULL DEFAULT 'unknown',
  revision INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX team_findings_page ON team_findings(team_id,created_at,id);
CREATE TABLE team_finding_revisions(
  id TEXT PRIMARY KEY,
  finding_id TEXT NOT NULL REFERENCES team_findings(id),
  team_id TEXT NOT NULL REFERENCES teams(id),
  actor_member_id TEXT NOT NULL REFERENCES team_members(id),
  previous_revision INTEGER NOT NULL,
  state TEXT NOT NULL,
  reason TEXT NOT NULL,
  evidence TEXT NOT NULL DEFAULT '[]',
  snapshot_sha256 TEXT,
  environmental_failure INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE INDEX team_finding_history ON team_finding_revisions(finding_id,created_at,id);

CREATE TABLE team_objections(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  finding_id TEXT NOT NULL REFERENCES team_findings(id),
  claim TEXT NOT NULL,
  check_required TEXT NOT NULL,
  author_member_id TEXT NOT NULL REFERENCES team_members(id),
  state TEXT NOT NULL,
  exchanges INTEGER NOT NULL DEFAULT 0,
  exchanges_without_evidence INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  resolved_at INTEGER,
  resolution TEXT
);
CREATE INDEX team_objections_finding ON team_objections(finding_id,created_at);

CREATE TABLE team_evidence(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  finding_id TEXT REFERENCES team_findings(id),
  member_id TEXT REFERENCES team_members(id),
  snapshot_sha256 TEXT NOT NULL,
  files TEXT NOT NULL DEFAULT '[]',
  exclusions TEXT NOT NULL DEFAULT '[]',
  configuration TEXT NOT NULL DEFAULT '{}',
  tool TEXT NOT NULL,
  tool_version TEXT,
  invocation TEXT NOT NULL,
  exit_code INTEGER,
  expected TEXT,
  observed TEXT,
  error TEXT,
  limitations TEXT NOT NULL DEFAULT '[]',
  negative_control TEXT,
  artifacts TEXT NOT NULL DEFAULT '[]',
  receipt TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX team_evidence_finding ON team_evidence(team_id,finding_id,created_at,id);

CREATE TABLE team_operations(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  actor_key TEXT NOT NULL,
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  request TEXT NOT NULL,
  result TEXT,
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(team_id,actor_key,kind,id)
);

DROP TRIGGER IF EXISTS task_event_apply;
CREATE TRIGGER task_event_apply AFTER INSERT ON task_events BEGIN
  UPDATE tasks SET
    last_event_seq=NEW.seq,
    updated_at=NEW.at,
    revision=revision+1,
    state=CASE NEW.kind
      WHEN 'task_prepared' THEN 'prepared'
      WHEN 'turn_started' THEN 'running'
      WHEN 'team_started' THEN 'running'
      WHEN 'approval_required' THEN 'awaiting_permission'
      WHEN 'approval_resolved' THEN CASE WHEN state='awaiting_permission' THEN 'running' ELSE state END
      WHEN 'tool_started' THEN 'running'
      WHEN 'task_blocked' THEN 'blocked'
      WHEN 'turn_completed' THEN 'completed'
      WHEN 'team_completed' THEN 'completed'
      WHEN 'task_failed' THEN 'failed'
      WHEN 'team_failed' THEN 'failed'
      WHEN 'failed' THEN 'failed'
      WHEN 'task_cancelled' THEN 'cancelled'
      WHEN 'team_cancelled' THEN 'cancelled'
      WHEN 'interrupted' THEN CASE WHEN cancellation IN ('requested','uncertain','confirmed') THEN 'cancelled' ELSE 'blocked' END
      ELSE state END
  WHERE id=NEW.task_id;
END;
