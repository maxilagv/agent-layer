/**
 * Connections view: the configured remote connections and their lifecycle
 * actions.
 *
 * The status reported by the backend is shown verbatim; the distinct failure
 * reasons are never collapsed into a single "error" label.
 */
import { useEffect, useState } from 'react';
import { useApp } from '../state/AppContext';
import { useSnapshot } from '../state/useSnapshot';
import { handleListKeyDown, LIST_ITEM_ATTRIBUTE } from '../components/Keyboard';
import { EmptyState, ErrorState, LoadingState } from '../components/States';
import { errorMessage } from '../api/client';
import type { ConnectionAction } from '../api/client';
import type { Connection } from '../api/types';

const ACTIONS: ConnectionAction[] = ['probe', 'enable', 'disable', 'revoke'];

const ACTION_LABEL: Record<ConnectionAction, string> = {
  probe: 'Probe',
  enable: 'Enable',
  disable: 'Disable',
  revoke: 'Revoke',
};

/** Statuses that the backend reports verbatim. */
export const KNOWN_STATUSES = [
  'service_unreachable',
  'credential_rejected',
  'credential_expired',
  'insufficient_permission',
  'unknown',
];

export function ConnectionsView() {
  const { client } = useApp();
  const connections = useSnapshot(() => client.connections(), []);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [announcement, setAnnouncement] = useState('');
  const [actionError, setActionError] = useState<string | null>(null);

  const [rows, setRows] = useState<Connection[]>([]);
  useEffect(() => {
    setRows(connections.data ?? []);
  }, [connections.data]);

  const act = async (connection: Connection, action: ConnectionAction) => {
    setBusyId(connection.id);
    setActionError(null);
    try {
      const updated = await client.connectionAction(connection.id, action);
      setRows((current) =>
        current.map((candidate) => (candidate.id === updated.id ? updated : candidate)),
      );
      setAnnouncement(
        `${connection.name}: ${action} finished, status ${updated.status} (revision ${updated.revision})`,
      );
    } catch (cause) {
      setActionError(`Unable to ${action} ${connection.name}: ${errorMessage(cause)}`);
    } finally {
      setBusyId(null);
    }
  };

  return (
    <section className="view" aria-labelledby="connections-heading">
      <header className="view__header">
        <h2 id="connections-heading">Connections</h2>
        <p className="muted">Remote services available to the executors.</p>
      </header>

      {/* Live region announcing every connection state change. */}
      <p className="visually-hidden" role="status" aria-live="polite" data-testid="connection-live-region">
        {announcement}
      </p>

      {actionError ? (
        <p className="state state--error" role="alert">
          {actionError}
        </p>
      ) : null}
      {connections.loading ? <LoadingState label="Loading connections…" /> : null}
      {connections.error ? (
        <ErrorState
          message={`Unable to load connections: ${connections.error}`}
          onRetry={connections.reload}
        />
      ) : null}
      {rows.length === 0 && !connections.loading && !connections.error ? (
        <EmptyState message="No connection is configured." />
      ) : null}

      {rows.length > 0 ? (
        <table className="table" aria-label="Connections">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Kind</th>
              <th scope="col">Account</th>
              <th scope="col">Status</th>
              <th scope="col">Revision</th>
              <th scope="col">Enabled</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody onKeyDown={handleListKeyDown}>
            {rows.map((connection) => (
              <tr key={connection.id}>
                <td>
                  <span className="connection__name">{connection.name}</span>
                  {connection.detail ? (
                    <span className="muted connection__detail">{connection.detail}</span>
                  ) : null}
                </td>
                <td>{connection.kind}</td>
                <td>{connection.account ?? '—'}</td>
                <td>
                  <span className="status" data-testid={`connection-status-${connection.id}`}>
                    {connection.status}
                  </span>
                </td>
                <td>{connection.revision}</td>
                <td>{connection.enabled ? 'yes' : 'no'}</td>
                <td className="table__actions">
                  {ACTIONS.map((action) => (
                    <button
                      key={action}
                      type="button"
                      {...{ [LIST_ITEM_ATTRIBUTE]: 'true' }}
                      className="button button--small"
                      disabled={busyId === connection.id}
                      onClick={() => void act(connection, action)}
                    >
                      {ACTION_LABEL[action]}
                    </button>
                  ))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}

      <section className="panel" aria-label="Known connection statuses">
        <h3>Status values</h3>
        <ul className="statuses">
          {KNOWN_STATUSES.map((status) => (
            <li key={status}>
              <code>{status}</code>
            </li>
          ))}
        </ul>
      </section>
    </section>
  );
}
