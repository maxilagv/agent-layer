/**
 * Tasks view: the task table, the "new task" form and the detail panel.
 */
import { useEffect, useMemo, useState, type FormEvent } from 'react';
import { useApp } from '../state/AppContext';
import { useTaskList } from '../state/useTaskList';
import { useSnapshot } from '../state/useSnapshot';
import { handleListKeyDown, LIST_ITEM_ATTRIBUTE } from '../components/Keyboard';
import { CancellationBadge, TaskStateBadge } from '../components/Badge';
import { EmptyState, ErrorState, LoadingState, ReconnectingBanner } from '../components/States';
import { TaskDetail } from './TaskDetail';
import { newIdempotencyKey } from '../utils/id';
import { formatTimestamp } from '../utils/format';

const EXECUTORS = ['local', 'remote'];

function modelNames(models: unknown[] | null): string[] {
  if (!models) return [];
  const names: string[] = [];
  for (const model of models) {
    if (typeof model === 'string') {
      names.push(model);
      continue;
    }
    if (model && typeof model === 'object') {
      const record = model as Record<string, unknown>;
      const value = record.id ?? record.model ?? record.name;
      if (typeof value === 'string') names.push(value);
    }
  }
  return names;
}

export function TasksView() {
  const { client, projects, selectedProject, selectedProjectId, scopeKey } = useApp();
  const feed = useTaskList(client, selectedProject);
  const [openTaskId, setOpenTaskId] = useState<string | null>(null);

  const [formProject, setFormProject] = useState<string>('');
  const [objective, setObjective] = useState('');
  const [executor, setExecutor] = useState('local');
  const [model, setModel] = useState('');
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [createdTaskId, setCreatedTaskId] = useState<string | null>(null);

  const models = useSnapshot(() => client.models(), []);
  const modelOptions = useMemo(() => modelNames(models.data), [models.data]);

  useEffect(() => {
    setFormProject(selectedProjectId ?? '');
  }, [selectedProjectId]);

  // Switching project closes the task detail of the previous project.
  useEffect(() => {
    setOpenTaskId(null);
  }, [scopeKey]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const project = projects.find((candidate) => candidate.id === formProject);
    if (!project) {
      setCreateError('Select a project.');
      return;
    }
    if (objective.trim() === '') {
      setCreateError('Describe the objective of the task.');
      return;
    }
    setCreating(true);
    setCreateError(null);
    setCreatedTaskId(null);
    try {
      const task = await client.createTask({
        workspace: project.path,
        objective: objective.trim(),
        session_id: null,
        model: model === '' ? null : model,
        executor,
        idempotency_key: newIdempotencyKey(),
      });
      setCreatedTaskId(task.id);
      setObjective('');
      feed.reload();
      setOpenTaskId(task.id);
    } catch (cause) {
      setCreateError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCreating(false);
    }
  };

  return (
    <section className="view" aria-labelledby="tasks-heading">
      <header className="view__header">
        <h2 id="tasks-heading">Tasks</h2>
        <p className="muted">
          {selectedProject
            ? `Tasks of ${selectedProject.name} (${selectedProject.path})`
            : 'Select a project first.'}
        </p>
      </header>

      {feed.status === 'reconnecting' ? <ReconnectingBanner /> : null}

      <form className="panel new-task" onSubmit={(event) => void submit(event)} aria-label="New task">
        <h3>New task</h3>
        <div className="new-task__grid">
          <label className="field">
            <span className="field__label">Project</span>
            <select
              className="field__input"
              name="task-project"
              value={formProject}
              onChange={(event) => setFormProject(event.target.value)}
            >
              <option value="">— select a project —</option>
              {projects.map((project) => (
                <option key={project.id} value={project.id}>
                  {project.name} ({project.path})
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field__label">Executor</span>
            <select
              className="field__input"
              name="task-executor"
              value={executor}
              onChange={(event) => setExecutor(event.target.value)}
            >
              {EXECUTORS.map((value) => (
                <option key={value} value={value}>
                  {value}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field__label">Model (optional)</span>
            <select
              className="field__input"
              name="task-model"
              value={model}
              onChange={(event) => setModel(event.target.value)}
            >
              <option value="">— default —</option>
              {modelOptions.map((value) => (
                <option key={value} value={value}>
                  {value}
                </option>
              ))}
            </select>
          </label>
        </div>
        <label className="field">
          <span className="field__label">Objective</span>
          <textarea
            className="field__input field__input--area"
            name="task-objective"
            value={objective}
            onChange={(event) => setObjective(event.target.value)}
            placeholder="What should the agent do?"
          />
        </label>
        {createError ? (
          <p className="state state--error" role="alert">
            {createError}
          </p>
        ) : null}
        {createdTaskId ? (
          <p className="notice" role="status" data-testid="created-task-id">
            Task created: {createdTaskId}
          </p>
        ) : null}
        <button type="submit" className="button" disabled={creating}>
          {creating ? 'Creating…' : 'Create task'}
        </button>
      </form>

      {feed.status === 'loading' ? <LoadingState label="Loading tasks…" /> : null}
      {feed.error ? (
        <ErrorState message={`Unable to load tasks: ${feed.error}`} onRetry={feed.reload} />
      ) : null}
      {feed.tasks.length === 0 && feed.status !== 'loading' && !feed.error ? (
        <EmptyState message="No task has been created for this project yet." />
      ) : null}

      {feed.tasks.length > 0 ? (
        <table className="table" aria-label="Tasks">
          <thead>
            <tr>
              <th scope="col">Task</th>
              <th scope="col">State</th>
              <th scope="col">Executor</th>
              <th scope="col">Project</th>
              <th scope="col">Created</th>
              <th scope="col">Cancellation</th>
            </tr>
          </thead>
          <tbody onKeyDown={handleListKeyDown}>
            {feed.tasks.map((task) => (
              <tr key={task.id} className={task.id === openTaskId ? 'table__row--selected' : undefined}>
                <td>
                  <button
                    type="button"
                    {...{ [LIST_ITEM_ATTRIBUTE]: 'true' }}
                    className="link-button"
                    aria-pressed={task.id === openTaskId}
                    onClick={() => setOpenTaskId(task.id === openTaskId ? null : task.id)}
                  >
                    {task.id}
                  </button>
                </td>
                <td>
                  <TaskStateBadge state={task.state} />
                </td>
                <td>{task.executor}</td>
                <td>{task.project}</td>
                <td>{formatTimestamp(task.created_at)}</td>
                <td>
                  <CancellationBadge state={task.cancellation} />
                  {task.cancellation === 'none' ? <span className="muted">none</span> : null}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}

      {openTaskId ? (
        <TaskDetail
          client={client}
          taskId={openTaskId}
          onTaskChanged={() => feed.reload()}
        />
      ) : null}
    </section>
  );
}
