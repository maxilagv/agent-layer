/**
 * Task detail: state, revision, usage, cancellation, error and the event
 * timeline of a single task, plus the Cancel and Correct actions.
 */
import { useEffect, useState } from 'react';
import type { ApiClient } from '../api/client';
import { errorMessage, isConflict } from '../api/client';
import type { Task } from '../api/types';
import { useEventFeed } from '../state/useEventFeed';
import { CancellationBadge, TaskStateBadge } from '../components/Badge';
import { EventTimeline } from '../components/EventTimeline';
import { DiffPanel } from '../components/DiffPanel';
import { ArtifactReader } from '../components/ArtifactReader';
import { ErrorState, LoadingState, ReconnectingBanner } from '../components/States';
import { formatTimestamp, prettyJson } from '../utils/format';

export interface TaskDetailProps {
  client: ApiClient;
  taskId: string;
  onTaskChanged?: (task: Task) => void;
}

export function TaskDetail({ client, taskId, onTaskChanged }: TaskDetailProps) {
  const [task, setTask] = useState<Task | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [instruction, setInstruction] = useState('');
  const [actionBusy, setActionBusy] = useState(false);

  const feed = useEventFeed({
    client,
    scopeKey: `task:${taskId}`,
    path: `tasks/${encodeURIComponent(taskId)}`,
    load: () => client.taskEvents(taskId),
    enabled: true,
  });

  const loadTask = async (): Promise<Task | null> => {
    try {
      const value = await client.task(taskId);
      setTask(value);
      setError(null);
      return value;
    } catch (cause) {
      setError(errorMessage(cause));
      return null;
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setNotice(null);
    setInstruction('');
    client
      .task(taskId)
      .then((value) => {
        if (cancelled) return;
        setTask(value);
        setError(null);
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setError(errorMessage(cause));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client, taskId]);

  const cancel = async () => {
    if (!task) return;
    setActionBusy(true);
    setNotice(null);
    try {
      const updated = await client.cancelTask(task.id);
      setTask(updated);
      onTaskChanged?.(updated);
      setNotice('Cancellation requested.');
    } catch (cause) {
      setNotice(`Unable to cancel: ${errorMessage(cause)}`);
    } finally {
      setActionBusy(false);
    }
  };

  const correct = async () => {
    if (!task) return;
    const text = instruction.trim();
    if (text === '') {
      setNotice('Enter a correction instruction first.');
      return;
    }
    setActionBusy(true);
    setNotice(null);
    try {
      const result = await client.correctTask(task.id, {
        instruction: text,
        expected_revision: task.revision,
      });
      setNotice(`Correction ${result.status}.`);
      setInstruction('');
      await loadTask();
    } catch (cause) {
      if (isConflict(cause)) {
        // The task changed while the operator was writing: reload it.
        setNotice('Stale, reload: the task changed since it was loaded.');
        await loadTask();
      } else {
        setNotice(`Unable to apply the correction: ${errorMessage(cause)}`);
      }
    } finally {
      setActionBusy(false);
    }
  };

  return (
    <section className="panel" aria-label={`Task ${taskId}`}>
      <header className="panel__header">
        <h3>Task {taskId}</h3>
        <div className="panel__actions">
          <button
            type="button"
            className="button button--small"
            onClick={() => void cancel()}
            disabled={actionBusy || !task || task.state === 'cancelled'}
          >
            Cancel task
          </button>
          <button type="button" className="button button--small" onClick={() => void loadTask()}>
            Reload
          </button>
        </div>
      </header>

      {feed.status === 'reconnecting' ? <ReconnectingBanner /> : null}
      {loading ? <LoadingState label="Loading task…" /> : null}
      {error ? <ErrorState message={`Unable to load the task: ${error}`} onRetry={() => void loadTask()} /> : null}

      {notice ? (
        <p className="notice" role="status" data-testid="task-notice">
          {notice}
        </p>
      ) : null}

      {task ? (
        <dl className="details">
          <div className="details__row">
            <dt>State</dt>
            <dd>
              <TaskStateBadge state={task.state} />
            </dd>
          </div>
          <div className="details__row">
            <dt>Revision</dt>
            <dd data-testid="task-revision">{task.revision}</dd>
          </div>
          <div className="details__row">
            <dt>Cancellation</dt>
            <dd>
              <CancellationBadge state={task.cancellation} />
              <span className="muted"> {task.cancellation}</span>
            </dd>
          </div>
          <div className="details__row">
            <dt>Executor</dt>
            <dd>{task.executor}</dd>
          </div>
          <div className="details__row">
            <dt>Project</dt>
            <dd>{task.project}</dd>
          </div>
          <div className="details__row">
            <dt>Session</dt>
            <dd>{task.session_id ?? '—'}</dd>
          </div>
          <div className="details__row">
            <dt>Objective</dt>
            <dd>{task.objective}</dd>
          </div>
          <div className="details__row">
            <dt>Created</dt>
            <dd>{formatTimestamp(task.created_at)}</dd>
          </div>
          <div className="details__row">
            <dt>Updated</dt>
            <dd>{formatTimestamp(task.updated_at)}</dd>
          </div>
          <div className="details__row">
            <dt>Usage</dt>
            <dd>
              <pre className="details__pre">{prettyJson(task.usage)}</pre>
            </dd>
          </div>
          {task.correction ? (
            <div className="details__row">
              <dt>Correction</dt>
              <dd>{task.correction}</dd>
            </div>
          ) : null}
          {task.error ? (
            <div className="details__row">
              <dt>Error</dt>
              <dd className="details__error">{task.error}</dd>
            </div>
          ) : null}
        </dl>
      ) : null}

      {task ? (
        <section className="subpanel" aria-label="Correction">
          <h4>Correct the task</h4>
          <p className="muted">
            The correction is applied against revision <strong>{task.revision}</strong>.
          </p>
          <textarea
            className="field__input field__input--area"
            aria-label="Correction instruction"
            value={instruction}
            onChange={(event) => setInstruction(event.target.value)}
            placeholder="Describe what the agent should do differently"
          />
          <button
            type="button"
            className="button"
            onClick={() => void correct()}
            disabled={actionBusy}
          >
            Apply correction
          </button>
        </section>
      ) : null}

      <section className="subpanel" aria-label="Task events">
        <h4>Timeline</h4>
        {feed.status === 'loading' ? <LoadingState label="Loading events…" /> : null}
        {feed.error ? (
          <ErrorState message={`Unable to load events: ${feed.error}`} onRetry={feed.reload} />
        ) : null}
        <EventTimeline events={feed.events} caption="Task events" />
      </section>

      <section className="subpanel" aria-label="Workspace changes">
        <h4>Workspace changes</h4>
        <DiffPanel events={feed.events} />
      </section>

      <section className="subpanel" aria-label="Artifacts">
        <ArtifactReader client={client} taskId={taskId} sessionId={task?.session_id ?? null} />
      </section>
    </section>
  );
}
