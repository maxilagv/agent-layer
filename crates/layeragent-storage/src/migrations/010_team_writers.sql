-- Cooperative writers: per-member access, path claims and patches.
-- `access` is what the member asked for; `access_applied` is what its current
-- or last activation runs with (fixed when an activation starts).
ALTER TABLE team_members ADD COLUMN access TEXT NOT NULL DEFAULT 'read';
ALTER TABLE team_members ADD COLUMN access_applied TEXT NOT NULL DEFAULT 'read';
ALTER TABLE team_members ADD COLUMN worktree TEXT;
ALTER TABLE team_members ADD COLUMN worktree_base TEXT;
ALTER TABLE team_members ADD COLUMN worktree_stale INTEGER NOT NULL DEFAULT 0;

-- A path (file or directory prefix) one member intends to change for one work
-- item. Overlapping claims between members are refused at claim time.
CREATE TABLE team_path_claims(
  team_id TEXT NOT NULL REFERENCES teams(id),
  path TEXT NOT NULL,
  member_id TEXT NOT NULL REFERENCES team_members(id),
  work_id TEXT NOT NULL REFERENCES team_work(id),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(team_id,path)
);
CREATE INDEX team_path_claims_member ON team_path_claims(team_id,member_id,work_id);

-- kind: submission (awaits review), handoff (carried to the recipient's
-- worktree) or retained (unsubmitted work kept when a team ended).
CREATE TABLE team_patches(
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id),
  kind TEXT NOT NULL,
  work_id TEXT REFERENCES team_work(id),
  author_member_id TEXT NOT NULL REFERENCES team_members(id),
  recipient_member_id TEXT REFERENCES team_members(id),
  state TEXT NOT NULL,
  summary TEXT NOT NULL,
  base TEXT NOT NULL,
  files TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  evidence_id TEXT REFERENCES team_evidence(id),
  review_message_id TEXT REFERENCES team_messages(id),
  reviewer_member_id TEXT REFERENCES team_members(id),
  reason TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX team_patches_work ON team_patches(team_id,work_id,state);

-- Patch content is kept apart so listings never carry it.
CREATE TABLE team_patch_bodies(
  patch_id TEXT PRIMARY KEY REFERENCES team_patches(id),
  body BLOB NOT NULL
);
