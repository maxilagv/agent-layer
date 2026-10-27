/**
 * Approval bookkeeping.
 *
 * Pending approvals are derived from task events: `approval_required` opens a
 * review and `approval_resolved` closes it again. The same rules are used for
 * the snapshot and for the live stream.
 */
import type { ApprovalRequest, TaskEvent } from '../api/types';

interface ApprovalEventData {
  approval_id?: string;
  call?: { id?: string; name?: string; arguments?: unknown };
  destination?: unknown;
  action_context?: unknown;
  arguments_hash?: string;
  expected_revision?: number;
  expires_at?: number;
  task_revision?: number;
}

export function approvalFromEvent(event: TaskEvent): ApprovalRequest | null {
  if (event.kind !== 'approval_required') return null;
  const data = (event.data ?? {}) as ApprovalEventData;
  const approvalId = data.approval_id;
  if (typeof approvalId !== 'string' || approvalId === '') return null;
  return {
    approval_id: approvalId,
    task_id: event.task_id,
    task_revision: data.expected_revision ?? data.task_revision,
    call: {
      id: data.call?.id ?? approvalId,
      name: data.call?.name ?? 'unknown',
      arguments: data.call?.arguments ?? null,
    },
    destination: data.destination ?? null,
    action_context: data.action_context ?? null,
    arguments_hash: data.arguments_hash ?? '',
    expires_at: data.expires_at ?? 0,
  };
}

/** Applies a batch of events to the pending approval list. */
export function applyApprovalEvents(
  current: ApprovalRequest[],
  events: TaskEvent[],
): ApprovalRequest[] {
  let next = current;
  for (const event of events) {
    if (event.kind === 'approval_required') {
      const approval = approvalFromEvent(event);
      if (!approval) continue;
      if (next.some((candidate) => candidate.approval_id === approval.approval_id)) continue;
      next = [...next, approval];
    } else if (event.kind === 'approval_resolved') {
      const data = (event.data ?? {}) as ApprovalEventData;
      const approvalId = data.approval_id;
      if (typeof approvalId !== 'string') continue;
      next = next.filter((candidate) => candidate.approval_id !== approvalId);
    }
  }
  return next;
}

/** Pending approvals contained in a batch of events, in event order. */
export function pendingApprovalsFromEvents(events: TaskEvent[]): ApprovalRequest[] {
  return applyApprovalEvents([], events);
}
