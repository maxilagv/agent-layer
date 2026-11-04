-- Operator controls on one member. `operator_hold` is 'paused' while the
-- operator holds this member alone (a team resume does not release it) and
-- 'retired' once the operator removed it from the team; NULL otherwise.
ALTER TABLE team_members ADD COLUMN operator_hold TEXT;
ALTER TABLE team_members ADD COLUMN operator_hold_at INTEGER;
ALTER TABLE team_members ADD COLUMN operator_hold_by TEXT;
