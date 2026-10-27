/**
 * Same-origin HTTP transport.
 *
 * Every request is relative to the current origin under the `/api` prefix, so
 * the frontend works unchanged behind the Rust web server, behind a reverse
 * proxy or from the Vite dev server (which proxies `/api` to the daemon).
 *
 * Security rules enforced here:
 *   - the CSRF token lives in this closure only; it is never written to
 *     localStorage/sessionStorage and never appears in a URL;
 *   - every mutating request carries the `X-LayerAgent-CSRF` header;
 *   - cookies are sent with `credentials: 'include'`.
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
import type { Connection, Project, Session, Task, TaskEvent } from './types';

export const CSRF_HEADER = 'X-LayerAgent-CSRF';

/** Server-Sent-Events endpoint used by `subscribe`. */
export const EVENTS_ENDPOINT = '/events';

type QueryValue = string | number | undefined | null;

function buildUrl(base: string, path: string, query?: Record<string, QueryValue>): string {
  const search = new URLSearchParams();
  if (query) {
    for (const [key, value] of Object.entries(query)) {
      if (value === undefined || value === null || value === '') continue;
      search.set(key, String(value));
    }
  }
  const qs = search.toString();
  return `${base}/api${path}${qs ? `?${qs}` : ''}`;
}

function messageFromBody(body: unknown, fallback: string): string {
  if (typeof body === 'string' && body.trim() !== '') return body;
  if (body && typeof body === 'object') {
    const record = body as Record<string, unknown>;
    for (const key of ['error', 'message', 'detail']) {
      const value = record[key];
      if (typeof value === 'string' && value.trim() !== '') return value;
    }
  }
  return fallback;
}

export function createHttpClient(baseUrl: string = ''): ApiClient {
  const base = baseUrl.replace(/\/+$/, '');
  // CSRF token is kept in memory only (never persisted, never in a URL).
  let csrfToken: string | null = null;

  async function request<T>(
    method: 'GET' | 'POST' | 'PUT' | 'DELETE',
    path: string,
    options: { query?: Record<string, QueryValue>; body?: unknown; mutating?: boolean } = {},
  ): Promise<T> {
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (options.body !== undefined) headers['Content-Type'] = 'application/json';
    if (options.mutating && csrfToken) headers[CSRF_HEADER] = csrfToken;

    const response = await fetch(buildUrl(base, path, options.query), {
      method,
      headers,
      credentials: 'include',
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });

    const raw = await response.text().catch(() => '');
    let payload: unknown = undefined;
    if (raw !== '') {
      try {
        payload = JSON.parse(raw);
      } catch {
        payload = raw;
      }
    }

    if (!response.ok) {
      throw new ApiError(
        response.status,
        messageFromBody(payload, `HTTP ${response.status}`),
        payload,
      );
    }
    return payload as T;
  }

  return {
    async login(code: string): Promise<void> {
      const data = await request<{ csrf_token?: string }>('POST', '/login', {
        body: { code },
      });
      csrfToken = typeof data?.csrf_token === 'string' ? data.csrf_token : csrfToken;
    },

    async logout(): Promise<void> {
      await request('POST', '/logout', { body: {}, mutating: true });
      csrfToken = null;
    },

    async bootstrap(): Promise<Bootstrap> {
      const data = await request<Bootstrap>('GET', '/bootstrap');
      const token = data?.session?.csrf_token;
      csrfToken = typeof token === 'string' && token !== '' ? token : null;
      return data;
    },

    projects(): Promise<Project[]> {
      return request<Project[]>('GET', '/projects');
    },

    sessions(project?: string): Promise<Session[]> {
      return request<Session[]>('GET', '/sessions', { query: { project } });
    },

    sessionEvents(id: string, after?: number): Promise<EventPage> {
      return request<EventPage>('GET', `/sessions/${encodeURIComponent(id)}/events`, {
        query: { after },
      });
    },

    tasks(after?: number): Promise<TaskPage> {
      return request<TaskPage>('GET', '/tasks', { query: { after } });
    },

    createTask(req: NewTaskRequest): Promise<Task> {
      return request<Task>('POST', '/tasks', { body: req, mutating: true });
    },

    task(id: string): Promise<Task> {
      return request<Task>('GET', `/tasks/${encodeURIComponent(id)}`);
    },

    taskEvents(id: string, after?: number): Promise<EventPage> {
      return request<EventPage>('GET', `/tasks/${encodeURIComponent(id)}/events`, {
        query: { after },
      });
    },

    cancelTask(id: string): Promise<Task> {
      return request<Task>('POST', `/tasks/${encodeURIComponent(id)}/cancel`, {
        body: {},
        mutating: true,
      });
    },

    correctTask(
      id: string,
      req: { instruction: string; expected_revision: number },
    ): Promise<CorrectionResult> {
      return request<CorrectionResult>(
        'POST',
        `/tasks/${encodeURIComponent(id)}/correct`,
        { body: req, mutating: true },
      );
    },

    approve(id: string, req: ApprovalDecision): Promise<void> {
      return request<void>('POST', `/approvals/${encodeURIComponent(id)}`, {
        body: req,
        mutating: true,
      });
    },

    artifact(hash: string, opts?: ArtifactOptions): Promise<ArtifactChunk> {
      return request<ArtifactChunk>('GET', `/artifacts/${encodeURIComponent(hash)}`, {
        query: {
          offset: opts?.offset,
          limit: opts?.limit,
          session: opts?.session,
          task: opts?.task,
        },
      });
    },

    connections(): Promise<Connection[]> {
      return request<Connection[]>('GET', '/connections');
    },

    connectionAction(id: string, action: ConnectionAction): Promise<Connection> {
      return request<Connection>(
        'POST',
        `/connections/${encodeURIComponent(id)}/action`,
        { body: { action }, mutating: true },
      );
    },

    usage(): Promise<UsageResponse> {
      return request<UsageResponse>('GET', '/usage');
    },

    config(): Promise<ConfigResponse> {
      return request<ConfigResponse>('GET', '/config');
    },

    models(): Promise<unknown[]> {
      return request<unknown[]>('GET', '/models');
    },

    mcpCapabilities(): Promise<McpCapabilities> {
      return request<McpCapabilities>('GET', '/mcp/capabilities');
    },

    subscribe(
      path: string,
      onEvent: (e: TaskEvent) => void,
      onError?: (e: unknown) => void,
    ): () => void {
      let source: EventSource | null = null;
      try {
        source = new EventSource(buildUrl(base, EVENTS_ENDPOINT, { path }), {
          withCredentials: true,
        });
        source.onmessage = (message: MessageEvent) => {
          try {
            onEvent(JSON.parse(message.data) as TaskEvent);
          } catch (error) {
            onError?.(error);
          }
        };
        source.onerror = (error: Event) => {
          // Close the browser-managed reconnect: the application owns the
          // backoff loop so that the snapshot can be re-fetched on reconnect.
          try {
            source?.close();
          } catch {
            /* ignore */
          }
          source = null;
          onError?.(error);
        };
      } catch (error) {
        onError?.(error);
      }
      return () => {
        try {
          source?.close();
        } catch {
          /* ignore */
        }
        source = null;
      };
    },
  };
}
