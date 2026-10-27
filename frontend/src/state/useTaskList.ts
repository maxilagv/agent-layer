/**
 * Task list feed.
 *
 * The snapshot (`client.tasks()`) is awaited first; afterwards the ordered
 * events of the task stream are applied. Every event that belongs to a task
 * of the current scope refreshes that task, so the table always shows the
 * state that the backend persisted.
 *
 * The scope key is the selected project: when it changes the previous tasks
 * are cleared and the previous subscription is disposed before the new
 * snapshot is requested.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ApiClient } from '../api/client';
import { errorMessage } from '../api/client';
import type { Project, Task } from '../api/types';
import type { FeedStatus } from './useEventFeed';

export interface TaskListFeed {
  tasks: Task[];
  status: FeedStatus;
  error: string | null;
  reload: () => void;
  lastEventSeq: number;
}

function belongsToProject(task: Task, project: Project | null): boolean {
  if (!project) return false;
  return task.project === project.id || task.project === project.path;
}

export function useTaskList(
  client: ApiClient,
  project: Project | null,
): TaskListFeed {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [status, setStatus] = useState<FeedStatus>('loading');
  const [error, setError] = useState<string | null>(null);
  const [lastEventSeq, setLastEventSeq] = useState(0);
  const [token, setToken] = useState(0);
  const scopeKey = project?.id ?? '(no-project)';
  const projectRef = useRef(project);
  projectRef.current = project;

  const reload = useCallback(() => setToken((value) => value + 1), []);

  useEffect(() => {
    let cancelled = false;
    let unsubscribe: (() => void) | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    let attempt = 0;

    // Clear the previous scope before loading the new one.
    setTasks([]);
    setLastEventSeq(0);
    setStatus('loading');
    setError(null);

    const loadSnapshot = async () => {
      try {
        const page = await client.tasks();
        if (cancelled) return;
        const scoped = page.tasks.filter((task) => belongsToProject(task, projectRef.current));
        setTasks(scoped);
        setStatus('ready');
        setError(null);
      } catch (cause) {
        if (cancelled) return;
        setStatus('error');
        setError(errorMessage(cause));
      }
    };

    const scheduleReconnect = () => {
      if (cancelled) return;
      if (retryTimer !== null) clearTimeout(retryTimer);
      const delay = Math.min(500 * 2 ** attempt, 15000);
      attempt += 1;
      retryTimer = setTimeout(() => {
        if (cancelled) return;
        void loadSnapshot();
        connect();
      }, delay);
    };

    const refreshTask = (taskId: string) => {
      client
        .task(taskId)
        .then((task) => {
          if (cancelled) return;
          setTasks((current) => {
            const index = current.findIndex((candidate) => candidate.id === task.id);
            if (index === -1) return current;
            const next = [...current];
            next[index] = task;
            return next;
          });
        })
        .catch(() => undefined);
    };

    const connect = () => {
      if (cancelled) return;
      const currentProject = projectRef.current;
      if (!currentProject) return;
      const path = `tasks?project=${encodeURIComponent(currentProject.id)}`;
      try {
        unsubscribe = client.subscribe(
          path,
          (event) => {
            if (cancelled) return;
            setLastEventSeq((current) => {
              if (event.seq <= current) return current;
              attempt = 0;
              if (event.task_id) refreshTask(event.task_id);
              return event.seq;
            });
          },
          () => {
            if (cancelled) return;
            setStatus('reconnecting');
            scheduleReconnect();
          },
        );
      } catch {
        setStatus('reconnecting');
        scheduleReconnect();
      }
    };

    void loadSnapshot();
    connect();

    return () => {
      cancelled = true;
      if (retryTimer !== null) clearTimeout(retryTimer);
      try {
        unsubscribe?.();
      } catch {
        /* ignore */
      }
      unsubscribe = null;
    };
  }, [client, scopeKey, token]);

  return { tasks, status, error, reload, lastEventSeq };
}
