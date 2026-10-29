ALTER TABLE team_activations ADD COLUMN pause_boundary INTEGER NOT NULL DEFAULT 0;

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
      WHEN 'team_resumed' THEN 'running'
      WHEN 'team_paused' THEN 'blocked'
      WHEN 'team_interrupted' THEN 'blocked'
      WHEN 'team_no_progress' THEN CASE WHEN json_extract(NEW.data,'$.explicit_resume')=1 THEN 'blocked' ELSE state END
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
