/**
 * Test doubles for the API client.
 */
import { vi } from 'vitest';
import type { ApiClient } from '../api/client';
import type { Connection, Task, TaskEvent } from '../api/types';

export function makeTask(overrides: Partial<Task> = {}): Task {
  return {
    id: 'task-1',
    tenant: 'tenant-1',
    principal: 'operator',
    project: '/workspace/demo',
    session_id: null,
    turn_id: null,
    objective: 'Do the thing',
    executor: 'local',
    state: 'running',
    revision: 7,
    usage: { input_tokens: 10, output_tokens: 4 },
    cancellation: 'none',
    error: null,
    last_event_seq: 0,
    created_at: 1_700_000_000,
    updated_at: 1_700_000_001,
    ...overrides,
  };
}

export function makeEvent(overrides: Partial<TaskEvent> = {}): TaskEvent {
  return {
    seq: 1,
    at: 1_700_000_000,
    kind: 'note',
    data: {},
    ...overrides,
  };
}

export function makeConnection(overrides: Partial<Connection> = {}): Connection {
  return {
    id: 'conn-1',
    kind: 'github',
    name: 'GitHub',
    account: 'operator',
    projects: [],
    operations: [],
    status: 'connected',
    revision: 1,
    enabled: true,
    generation: 1,
    detail: null,
    ...overrides,
  };
}

export interface FakeClient extends ApiClient {
  /** Paths passed to `subscribe`, in order. */
  subscribeCalls: string[];
  /** Number of times the returned unsubscribe function was called. */
  unsubscribeCalls: number;
  /** Handlers registered through `subscribe`, per call index. */
  handlers: Array<(event: TaskEvent) => void>;
}

export function createFakeClient(overrides: Partial<ApiClient> = {}): FakeClient {
  const subscribeCalls: string[] = [];
  const handlers: Array<(event: TaskEvent) => void> = [];
  const state = { unsubscribeCalls: 0 };

  const client: FakeClient = {
    subscribeCalls,
    handlers,
    get unsubscribeCalls() {
      return state.unsubscribeCalls;
    },
    login: vi.fn(async () => undefined),
    logout: vi.fn(async () => undefined),
    bootstrap: vi.fn(async () => ({
      principal: 'operator',
      tenant: 'tenant-1',
      projects: [],
      capabilities: {},
    })),
    projects: vi.fn(async () => []),
    sessions: vi.fn(async () => []),
    sessionEvents: vi.fn(async () => ({ events: [], next: null })),
    tasks: vi.fn(async () => ({ tasks: [], next: null })),
    createTask: vi.fn(async () => makeTask()),
    task: vi.fn(async () => makeTask()),
    taskEvents: vi.fn(async () => ({ events: [], next: null })),
    cancelTask: vi.fn(async () => makeTask({ state: 'cancelled', cancellation: 'requested' })),
    correctTask: vi.fn(async () => ({
      id: 'correction-1',
      task_id: 'task-1',
      instruction: 'do it differently',
      expected_revision: 7,
      status: 'applied',
      created_at: 1_700_000_002,
      applied_at: 1_700_000_002,
    })),
    approve: vi.fn(async () => undefined),
    artifact: vi.fn(async () => ({ offset: 0, text: '' })),
    connections: vi.fn(async () => []),
    connectionAction: vi.fn(async () => makeConnection()),
    usage: vi.fn(async () => ({
      totals: { tasks: 0, input_tokens: 0, output_tokens: 0, cost_usd: 0 },
      tasks: [],
    })),
    config: vi.fn(async () => ({ revision: 'rev-1', config: { debug: false } })),
    models: vi.fn(async () => []),
    mcpCapabilities: vi.fn(async () => ({ resources: false, tools: false, prompts: false })),
    subscribe: vi.fn((path: string, onEvent: (event: TaskEvent) => void) => {
      subscribeCalls.push(path);
      handlers.push(onEvent);
      return () => {
        state.unsubscribeCalls += 1;
      };
    }),
    ...overrides,
  };

  return client;
}
