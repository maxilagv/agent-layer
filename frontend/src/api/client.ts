/**
 * Transport-agnostic API contract.
 *
 * Both transports (HTTP web server and Tauri bridge) implement exactly these
 * methods and return exactly these payload shapes.
 */
import type {
  Connection,
  Project,
  Session,
  Task,
  TaskEvent,
  UsageTotals,
} from './types';

export interface Bootstrap {
  principal: string;
  tenant: string;
  projects: Project[];
  capabilities: Record<string, boolean>;
  session?: { csrf_token?: string };
}

export interface EventPage {
  events: TaskEvent[];
  next: number | null;
}

export interface TaskPage {
  tasks: Task[];
  next: number | null;
}

export interface NewTaskRequest {
  workspace: string;
  objective: string;
  session_id?: string | null;
  model?: string | null;
  executor?: string | null;
  idempotency_key: string;
}

export interface CorrectionResult {
  id: string;
  task_id: string;
  instruction: string;
  expected_revision: number;
  status: string;
  created_at: number;
  applied_at: number | null;
}

export interface ApprovalDecision {
  expected_revision: number;
  arguments_hash: string;
  allow: boolean;
}

export interface ArtifactOptions {
  offset?: number;
  limit?: number;
  session?: string;
  task?: string;
}

export interface ArtifactChunk {
  offset: number;
  text: string;
}

export interface UsageResponse {
  totals: UsageTotals;
  tasks: Task[];
}

export interface ConfigResponse {
  revision: string;
  config: unknown;
}

export interface McpCapabilities {
  resources: boolean;
  tools: boolean;
  prompts: boolean;
}

export type ConnectionAction = 'probe' | 'enable' | 'disable' | 'revoke';

export interface ApiClient {
  /**
   * Redeems the single-use local access code of the web surface.
   *
   * The desktop bridge needs no code: the daemon socket is the authority.
   */
  login(code: string): Promise<void>;
  /** Ends the interface session without stopping any running work. */
  logout(): Promise<void>;
  bootstrap(): Promise<Bootstrap>;
  projects(): Promise<Project[]>;
  sessions(project?: string): Promise<Session[]>;
  sessionEvents(id: string, after?: number): Promise<EventPage>;
  tasks(after?: number): Promise<TaskPage>;
  createTask(req: NewTaskRequest): Promise<Task>;
  task(id: string): Promise<Task>;
  taskEvents(id: string, after?: number): Promise<EventPage>;
  cancelTask(id: string): Promise<Task>;
  correctTask(
    id: string,
    req: { instruction: string; expected_revision: number },
  ): Promise<CorrectionResult>;
  approve(id: string, req: ApprovalDecision): Promise<void>;
  artifact(hash: string, opts?: ArtifactOptions): Promise<ArtifactChunk>;
  connections(): Promise<Connection[]>;
  connectionAction(id: string, action: ConnectionAction): Promise<Connection>;
  usage(): Promise<UsageResponse>;
  config(): Promise<ConfigResponse>;
  models(): Promise<unknown[]>;
  mcpCapabilities(): Promise<McpCapabilities>;
  subscribe(
    path: string,
    onEvent: (e: TaskEvent) => void,
    onError?: (e: unknown) => void,
  ): () => void;
}

/** Error carrying the HTTP status so callers can react to conflicts (409). */
export class ApiError extends Error {
  readonly status: number;
  readonly body: unknown;

  constructor(status: number, message: string, body?: unknown) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.body = body;
  }
}

/** True when the server rejected the request because our view was stale. */
export function isConflict(error: unknown): boolean {
  return error instanceof ApiError && error.status === 409;
}

/** True when the server requires the local access code first. */
export function isUnauthenticated(error: unknown): boolean {
  return error instanceof ApiError && error.status === 401;
}

/** Human readable message for any thrown value. */
export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  try {
    return JSON.stringify(error);
  } catch {
    return 'Unknown error';
  }
}
