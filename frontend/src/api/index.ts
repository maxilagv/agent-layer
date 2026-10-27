/**
 * Transport abstraction for the shared LayerAgent frontend.
 *
 * The same React application runs in two hosts:
 *
 *  1. the Rust web server (browser), talking same-origin over `/api/...`;
 *  2. the Tauri desktop shell, talking to the Rust host over the IPC bridge.
 *
 * Both transports implement the very same `ApiClient` contract, so every view
 * is host agnostic.
 *
 * ---------------------------------------------------------------------------
 * HTTP endpoints (base URL is the current origin; `baseUrl` may be supplied
 * for tests or for a daemon listening on another port)
 * ---------------------------------------------------------------------------
 *   GET    /api/bootstrap                          -> Bootstrap
 *   GET    /api/projects                           -> Project[]
 *   GET    /api/sessions?project=<project>         -> Session[]
 *   GET    /api/sessions/<id>/events?after=<seq>   -> { events, next }
 *   GET    /api/tasks?after=<seq>                  -> { tasks, next }
 *   POST   /api/tasks                              -> Task            (CSRF)
 *   GET    /api/tasks/<id>                         -> Task
 *   GET    /api/tasks/<id>/events?after=<seq>      -> { events, next }
 *   POST   /api/tasks/<id>/cancel                  -> Task            (CSRF)
 *   POST   /api/tasks/<id>/correct                 -> CorrectionResult(CSRF)
 *   POST   /api/approvals/<id>                     -> 204             (CSRF)
 *   GET    /api/artifacts/<hash>?offset&limit&session&task -> { offset, text }
 *   GET    /api/connections                        -> Connection[]
 *   POST   /api/connections/<id>/action            -> Connection      (CSRF)
 *   GET    /api/usage                              -> { totals, tasks }
 *   GET    /api/config                             -> { revision, config }
 *   GET    /api/models                             -> unknown[]
 *   GET    /api/mcp/capabilities                   -> { resources, tools, prompts }
 *   GET    /api/events?path=<stream-path>          -> Server-Sent Events
 *
 * `GET /api/bootstrap` returns the session CSRF token; it is kept in memory
 * and sent back as the `X-LayerAgent-CSRF` header on every mutating request
 * (POST). It is never persisted and never placed in a URL.
 *
 * ---------------------------------------------------------------------------
 * Tauri bridge commands (snake_case of the `ApiClient` method names)
 * ---------------------------------------------------------------------------
 *   bootstrap()            -> invoke('bootstrap')
 *   projects()             -> invoke('list_projects')
 *   sessions(project?)     -> invoke('list_sessions', { project })
 *   sessionEvents(id,after) -> invoke('session_events', { id, after })
 *   tasks(after)           -> invoke('list_tasks', { after })
 *   createTask(req)        -> invoke('create_task', { request })
 *   task(id)               -> invoke('get_task', { id })
 *   taskEvents(id,after)   -> invoke('task_events', { id, after })
 *   cancelTask(id)         -> invoke('cancel_task', { id })
 *   correctTask(id,req)    -> invoke('correct_task', { id, instruction, expected_revision })
 *   approve(id,req)        -> invoke('approve', { id, decision })
 *   artifact(hash,opts)    -> invoke('artifact', { hash, options })
 *   connections()          -> invoke('list_connections')
 *   connectionAction(id,a) -> invoke('connection_action', { id, action })
 *   usage()                -> invoke('usage')
 *   config()               -> invoke('config')
 *   models()               -> invoke('list_models')
 *   mcpCapabilities()      -> invoke('mcp_capabilities')
 *   subscribe(path,...)    -> invoke('subscribe', { path })
 *
 * Server-sent events reach the WebView through the Tauri event channel
 * `layeragent://task-event`; the payload is the `TaskEvent` JSON object.
 *
 * ---------------------------------------------------------------------------
 * Stream paths used by `subscribe(path)`
 * ---------------------------------------------------------------------------
 *   tasks                        -> task events of every task
 *   tasks?project=<project-id>   -> task events of one project
 *   tasks/<task-id>              -> events of one task
 *   sessions/<session-id>        -> events of one session
 *
 * `after`/`next` are exclusive cursors: the `next` value returned by a page is
 * sent back as `after` to obtain the following page.
 */
import { createHttpClient } from './http';
import { createTauriClient } from './tauri';
import type { ApiClient } from './client';

export { createHttpClient, createTauriClient };
export { ApiError, errorMessage, isConflict } from './client';
export type {
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
export type {
  ApprovalRequest,
  CancellationState,
  Connection,
  Project,
  Session,
  Task,
  TaskEvent,
  TaskState,
  UsageTotals,
} from './types';

/** True when the application runs inside a Tauri WebView. */
export function isTauriHost(): boolean {
  return typeof window !== 'undefined' && typeof window.__TAURI__?.core?.invoke === 'function';
}

let cached: ApiClient | null = null;

/**
 * Returns the transport matching the current host: the Tauri bridge inside a
 * desktop WebView, the same-origin HTTP client in a browser.
 */
export function getClient(): ApiClient {
  if (!cached) {
    cached = isTauriHost() ? createTauriClient() : createHttpClient();
  }
  return cached;
}
