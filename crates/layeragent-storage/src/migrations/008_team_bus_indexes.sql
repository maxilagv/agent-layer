-- Hot paths of the cooperative message bus and activation lifecycle.
-- Reservations are settled and marked uncertain by activation.
CREATE INDEX IF NOT EXISTS team_reservations_activation ON team_reservations(activation_id,state);
-- Thread queries: messages attached to a finding or a work item.
CREATE INDEX IF NOT EXISTS team_messages_finding ON team_messages(team_id,finding_id,created_at);
CREATE INDEX IF NOT EXISTS team_messages_work ON team_messages(team_id,work_id,created_at);
