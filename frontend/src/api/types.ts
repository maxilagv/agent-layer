/**
 * Wire types shared by every transport (HTTP web server and Tauri bridge).
 *
 * These shapes mirror the Rust protocol crate exactly; field names are
 * snake_case because the payloads are serialized by serde without renaming.
 */

export type TaskState =
  | 'prepared'
  | 'running'
  | 'awaiting_permission'
  | 'blocked'
  | 'completed'
  | 'failed'
  | 'cancelled';

export type CancellationState = 'none' | 'requested' | 'confirmed' | 'uncertain';

export interface Task {
  id: string;
  tenant: string;
  principal: string;
  project: string;
  session_id: string | null;
  turn_id: string | null;
  objective: string;
  executor: string;
  state: TaskState;
  revision: number;
  usage: unknown;
  cancellation: CancellationState;
  correction?: string | null;
  error?: string | null;
  last_event_seq: number;
  created_at: number;
  updated_at: number;
}

export interface Session {
  id: string;
  principal: string;
  workspace: string;
  created_at: number;
}

export interface Project {
  id: string;
  path: string;
  name: string;
}

export interface TaskEvent {
  seq: number;
  at: number;
  task_id?: string;
  session_id?: string;
  kind: string;
  data: any;
}

export interface Connection {
  id: string;
  kind: string;
  name: string;
  account?: string | null;
  projects: string[];
  operations: string[];
  status: string;
  revision: number;
  enabled: boolean;
  generation: number;
  detail?: string | null;
}

export interface ApprovalRequest {
  approval_id: string;
  task_id?: string;
  task_revision?: number;
  call: { id: string; name: string; arguments: any };
  destination: any;
  action_context: any;
  arguments_hash: string;
  expires_at: number;
}

export interface UsageTotals {
  tasks: number;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
}
