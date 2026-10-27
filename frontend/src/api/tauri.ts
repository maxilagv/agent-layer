/**
 * Tauri transport.
 *
 * The desktop build talks to the Rust host through `window.__TAURI__.core.invoke`.
 * Command names are the snake_case form of the `ApiClient` method names and the
 * arguments are the snake_case form of the request fields. The command mapping
 * is documented in `src/api/index.ts`.
 *
 * When the bridge is not available the client transparently degrades to the
 * same-origin HTTP client so that a single bundle works in both hosts.
 */
import type {
  ApiClient,
  ApprovalDecision,
  ArtifactChunk,
  ArtifactOptions,
  Bootstrap,
  ConfigResponse,
  ConnectionAction,
  CorrectionResult,
  EventPage,
  McpCapabilities,
  NewTaskRequest,
  TaskPage,
  UsageResponse,
} from './client';
import { ApiError } from './client';
import { createHttpClient } from './http';
import type { Connection, Project, Session, Task, TaskEvent } from './types';

/** Tauri event channel used to push task events into the WebView. */
export const TAURI_EVENT_CHANNEL = 'layeragent://task-event';

function toError(value: unknown): Error {
  if (value instanceof Error) return value;
  if (value && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    const status = typeof record.status === 'number' ? record.status : 0;
    const message =
      typeof record.message === 'string'
        ? record.message
        : typeof record.error === 'string'
          ? record.error
          : 'Bridge error';
    return new ApiError(status, message, value);
  }
  return new Error(String(value));
}

export function createTauriClient(): ApiClient {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) {
    return createHttpClient();
  }

  const call = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
    try {
      return (await invoke(command, args ?? {})) as T;
    } catch (error) {
      throw toError(error);
    }
  };

  return {
    // The desktop bridge is authorized by the daemon socket; there is no
    // access code to redeem.
    login: async (): Promise<void> => undefined,
    logout: async (): Promise<void> => undefined,
    bootstrap: () => call<Bootstrap>('bootstrap'),
    projects: () => call<Project[]>('list_projects'),
    sessions: (project?: string) =>
      call<Session[]>('list_sessions', project === undefined ? {} : { project }),
    sessionEvents: (id: string, after?: number) =>
      call<EventPage>('session_events', after === undefined ? { id } : { id, after }),
    tasks: (after?: number) =>
      call<TaskPage>('list_tasks', after === undefined ? {} : { after }),
    createTask: (req: NewTaskRequest) =>
      call<Task>('create_task', {
        workspace: req.workspace,
        objective: req.objective,
        session: req.session_id ?? undefined,
        model: req.model ?? undefined,
        executor: req.executor ?? undefined,
      }),
    task: (id: string) => call<Task>('get_task', { id }),
    taskEvents: (id: string, after?: number) =>
      call<EventPage>('task_events', after === undefined ? { id } : { id, after }),
    cancelTask: (id: string) => call<Task>('cancel_task', { id }),
    correctTask: (id: string, req: { instruction: string; expected_revision: number }) =>
      call<CorrectionResult>('correct_task', { id, ...req }),
    approve: (id: string, decision: ApprovalDecision) =>
      call<void>('approve', {
        id,
        allow: decision.allow,
        expected_revision: decision.expected_revision,
        arguments_hash: decision.arguments_hash,
      }),
    artifact: (hash: string, options?: ArtifactOptions) =>
      call<ArtifactChunk>('artifact', {
        hash,
        offset: options?.offset,
        limit: options?.limit,
        session: options?.session,
        task: options?.task,
      }),
    connections: () => call<Connection[]>('list_connections'),
    connectionAction: (id: string, action: ConnectionAction) =>
      call<Connection>('connection_action', { id, action }),
    usage: () => call<UsageResponse>('usage'),
    config: () => call<ConfigResponse>('config'),
    models: () => call<unknown[]>('list_models'),
    mcpCapabilities: () => call<McpCapabilities>('mcp_capabilities'),
    subscribe(
      path: string,
      onEvent: (e: TaskEvent) => void,
      onError?: (e: unknown) => void,
    ): () => void {
      let unlisten: (() => void) | null = null;
      let handle: unknown = null;
      const listen = window.__TAURI__?.event?.listen;

      const start = async () => {
        try {
          if (listen) {
            unlisten = await listen(TAURI_EVENT_CHANNEL, (message) => {
              const payload = message?.payload;
              if (payload && typeof payload === 'object') {
                onEvent(payload as TaskEvent);
              }
            });
          }
          handle = await call<unknown>('subscribe', { path });
        } catch (error) {
          onError?.(error);
        }
      };
      void start();

      return () => {
        try {
          unlisten?.();
        } catch {
          /* ignore */
        }
        unlisten = null;
        if (typeof handle === 'function') {
          try {
            (handle as () => void)();
          } catch {
            /* ignore */
          }
        } else if (handle !== null && handle !== undefined) {
          void call<void>('unsubscribe', { id: handle }).catch(() => undefined);
        }
        handle = null;
      };
    },
  };
}
