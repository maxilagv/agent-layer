/**
 * Approvals view.
 *
 * Pending approvals are collected from the task events: `approval_required`
 * opens a review, `approval_resolved` closes it. The review panel is a modal
 * dialog with a focus trap; Escape closes it.
 */
import { useEffect, useRef, useState } from 'react';
import { useApp } from '../state/AppContext';
import { useSnapshot } from '../state/useSnapshot';
import { useEventFeed } from '../state/useEventFeed';
import { applyApprovalEvents, pendingApprovalsFromEvents } from '../state/approvals';
import { handleListKeyDown, LIST_ITEM_ATTRIBUTE } from '../components/Keyboard';
import { EmptyState, ErrorState, LoadingState, ReconnectingBanner } from '../components/States';
import { Modal } from '../components/Modal';
import { errorMessage, isConflict } from '../api/client';
import type { ApprovalRequest, TaskEvent } from '../api/types';
import { formatDuration, prettyJson, remainingMs } from '../utils/format';

export function ApprovalsView() {
  const { client } = useApp();
  const tasks = useSnapshot(() => client.tasks(), []);
  const [pending, setPending] = useState<ApprovalRequest[]>([]);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [selected, setSelected] = useState<ApprovalRequest | null>(null);
  const [decisionState, setDecisionState] = useState<string | null>(null);
  const [decisionError, setDecisionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const appliedSeq = useRef(0);

  // Live stream of task events; approvals are extracted from it.
  const feed = useEventFeed({
    client,
    scopeKey: 'approvals',
    path: 'tasks',
    load: () => Promise.resolve({ events: [], next: null }),
  });

  // Historical approvals of the tasks that wait for a permission.
  useEffect(() => {
    let cancelled = false;
    const awaiting = (tasks.data?.tasks ?? []).filter(
      (task) => task.state === 'awaiting_permission',
    );
    if (awaiting.length === 0) {
      setPending([]);
      setHistoryError(null);
      return;
    }
    setHistoryError(null);
    Promise.all(
      awaiting.map((task) =>
        client
          .taskEvents(task.id)
          .then((page) => page.events)
          .catch((): TaskEvent[] => []),
      ),
    )
      .then((pages) => {
        if (cancelled) return;
        setPending(pages.flatMap((events) => pendingApprovalsFromEvents(events)));
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setHistoryError(errorMessage(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [client, tasks.data]);

  // Live approvals.
  useEffect(() => {
    const fresh = feed.events.filter((event) => event.seq > appliedSeq.current);
    if (fresh.length === 0) return;
    appliedSeq.current = fresh[fresh.length - 1].seq;
    setPending((current) => applyApprovalEvents(current, fresh));
  }, [feed.events]);

  const decide = async (approval: ApprovalRequest, allow: boolean) => {
    setBusy(true);
    setDecisionError(null);
    try {
      await client.approve(approval.approval_id, {
        expected_revision: approval.task_revision ?? 0,
        arguments_hash: approval.arguments_hash,
        allow,
      });
      setPending((current) =>
        current.filter((candidate) => candidate.approval_id !== approval.approval_id),
      );
      setSelected(null);
      setDecisionState(allow ? 'Approval granted.' : 'Approval denied.');
    } catch (cause) {
      if (isConflict(cause)) {
        setDecisionError('Stale approval — reloading');
        setSelected(null);
        tasks.reload();
      } else {
        setDecisionError(`Unable to record the decision: ${errorMessage(cause)}`);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="view" aria-labelledby="approvals-heading">
      <header className="view__header">
        <h2 id="approvals-heading">Approvals</h2>
        <p className="muted">Tool calls that wait for an operator decision.</p>
      </header>

      {feed.status === 'reconnecting' ? <ReconnectingBanner /> : null}
      {decisionState ? (
        <p className="notice" role="status" data-testid="approval-state">
          {decisionState}
        </p>
      ) : null}
      {decisionError ? (
        <p className="state state--error" role="alert" data-testid="approval-error">
          {decisionError}
        </p>
      ) : null}
      {historyError ? (
        <ErrorState message={`Unable to load pending approvals: ${historyError}`} onRetry={tasks.reload} />
      ) : null}
      {tasks.loading ? <LoadingState label="Loading approvals…" /> : null}
      {tasks.error ? (
        <ErrorState message={`Unable to load tasks: ${tasks.error}`} onRetry={tasks.reload} />
      ) : null}

      {pending.length === 0 && !tasks.loading && !tasks.error ? (
        <EmptyState message="No pending approval." />
      ) : null}

      {pending.length > 0 ? (
        <ul className="list" onKeyDown={handleListKeyDown} aria-label="Pending approvals">
          {pending.map((approval) => (
            <li key={approval.approval_id}>
              <button
                type="button"
                {...{ [LIST_ITEM_ATTRIBUTE]: 'true' }}
                className="list__row"
                aria-haspopup="dialog"
                onClick={() => {
                  setDecisionError(null);
                  setSelected(approval);
                }}
              >
                <span className="list__title">{approval.call.name}</span>
                <span className="list__meta">{approval.approval_id}</span>
                <span className="list__meta">task {approval.task_id ?? '—'}</span>
                <span className="list__state">review</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      {selected ? (
        <ApprovalDialog
          approval={selected}
          busy={busy}
          onClose={() => setSelected(null)}
          onDecide={(allow) => void decide(selected, allow)}
        />
      ) : null}
    </section>
  );
}

function ApprovalDialog({
  approval,
  busy,
  onClose,
  onDecide,
}: {
  approval: ApprovalRequest;
  busy: boolean;
  onClose: () => void;
  onDecide: (allow: boolean) => void;
}) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);

  const remaining = remainingMs(approval.expires_at, now);

  return (
    <Modal title="Approval required" onClose={onClose}>
      <dl className="details">
        <div className="details__row">
          <dt>Tool call</dt>
          <dd data-testid="approval-call">{approval.call.name}</dd>
        </div>
        <div className="details__row">
          <dt>Call id</dt>
          <dd>{approval.call.id}</dd>
        </div>
        <div className="details__row">
          <dt>Arguments</dt>
          <dd>
            <pre className="details__pre" data-testid="approval-arguments">
              {prettyJson(approval.call.arguments)}
            </pre>
          </dd>
        </div>
        <div className="details__row">
          <dt>Destination</dt>
          <dd>
            <pre className="details__pre" data-testid="approval-destination">
              {prettyJson(approval.destination)}
            </pre>
          </dd>
        </div>
        <div className="details__row">
          <dt>Action context</dt>
          <dd>
            <pre className="details__pre" data-testid="approval-context">
              {prettyJson(approval.action_context)}
            </pre>
          </dd>
        </div>
        <div className="details__row">
          <dt>Arguments hash</dt>
          <dd data-testid="approval-hash">{approval.arguments_hash}</dd>
        </div>
        <div className="details__row">
          <dt>Expires in</dt>
          <dd data-testid="approval-expiry">
            {approval.expires_at ? formatDuration(remaining) : 'unknown'}
          </dd>
        </div>
      </dl>
      <div className="modal__footer">
        <button
          type="button"
          className="button button--danger"
          onClick={() => onDecide(false)}
          disabled={busy}
        >
          Deny
        </button>
        <button
          type="button"
          className="button button--primary"
          onClick={() => onDecide(true)}
          disabled={busy}
        >
          Approve
        </button>
      </div>
    </Modal>
  );
}
