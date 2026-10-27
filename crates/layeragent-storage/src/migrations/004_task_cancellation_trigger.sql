-- A confirmed cancellation must still map to the `cancelled` state. The
-- terminal interruption may be emitted after the cancellation has already
-- been confirmed, so the trigger cannot only look at `requested`/`uncertain`.
DROP TRIGGER IF EXISTS task_event_apply;
CREATE TRIGGER task_event_apply AFTER INSERT ON task_events BEGIN
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
      WHEN 'interrupted' THEN CASE WHEN cancellation IN ('requested','uncertain','confirmed') THEN 'cancelled' ELSE 'blocked' END
      ELSE state END
  WHERE id=NEW.task_id;
END;
