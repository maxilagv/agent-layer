/**
 * Explicit state labels.
 *
 * Colour is never the only carrier of information: every badge contains the
 * textual state as well.
 */
import type { CancellationState, TaskState } from '../api/types';

const TASK_STATE_LABEL: Record<TaskState, string> = {
  prepared: 'prepared',
  running: 'running',
  awaiting_permission: 'awaiting permission',
  blocked: 'blocked',
  completed: 'completed',
  failed: 'failed',
  cancelled: 'cancelled',
};

const CANCELLATION_LABEL: Record<CancellationState, string> = {
  none: 'cancellation: none',
  requested: 'cancellation: requested',
  confirmed: 'cancellation: confirmed',
  uncertain: 'cancellation: uncertain',
};

export function taskStateLabel(state: TaskState): string {
  return TASK_STATE_LABEL[state] ?? state;
}

export function cancellationLabel(state: CancellationState): string {
  return CANCELLATION_LABEL[state] ?? state;
}

export function TaskStateBadge({ state }: { state: TaskState }) {
  return (
    <span className={`badge badge--${state}`} data-testid="task-state-badge">
      <span className="badge__dot" aria-hidden="true" />
      {taskStateLabel(state)}
    </span>
  );
}

export function CancellationBadge({ state }: { state: CancellationState }) {
  if (state === 'none') return null;
  return (
    <span className="badge badge--cancellation" data-testid="cancellation-badge">
      {cancellationLabel(state)}
    </span>
  );
}

export function GenericBadge({ label, tone = 'neutral' }: { label: string; tone?: string }) {
  return (
    <span className={`badge badge--${tone}`} data-testid="generic-badge">
      {label}
    </span>
  );
}
