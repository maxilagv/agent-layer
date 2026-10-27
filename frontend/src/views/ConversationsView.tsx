/**
 * Conversations view: the sessions of the selected project and the event
 * timeline of the opened session.
 *
 * Older events are loaded with the cursor protocol: the server returns a
 * `next` cursor that is sent back as `after` for the next (older) page.
 */
import { useCallback, useEffect, useState } from 'react';
import { useApp } from '../state/AppContext';
import { useSnapshot } from '../state/useSnapshot';
import { useEventFeed } from '../state/useEventFeed';
import { mergeOrderedEvents, bySeq } from '../state/ordering';
import { handleListKeyDown, LIST_ITEM_ATTRIBUTE } from '../components/Keyboard';
import { EmptyState, ErrorState, LoadingState, ReconnectingBanner } from '../components/States';
import { EventTimeline } from '../components/EventTimeline';
import type { TaskEvent } from '../api/types';
import { formatTimestamp } from '../utils/format';

export function ConversationsView() {
  const { client, selectedProject, selectedProjectId, scopeKey } = useApp();
  const [openSessionId, setOpenSessionId] = useState<string | null>(null);
  const [olderEvents, setOlderEvents] = useState<TaskEvent[]>([]);
  const [olderCursor, setOlderCursor] = useState<number | null>(null);
  const [olderError, setOlderError] = useState<string | null>(null);
  const [olderLoading, setOlderLoading] = useState(false);

  const sessions = useSnapshot(
    () => client.sessions(selectedProjectId ?? undefined),
    [scopeKey],
    Boolean(selectedProjectId),
  );

  // Switching project (or session) drops the previously loaded pages.
  useEffect(() => {
    setOpenSessionId(null);
    setOlderEvents([]);
    setOlderCursor(null);
    setOlderError(null);
  }, [scopeKey]);

  const feed = useEventFeed({
    client,
    scopeKey: `${scopeKey}:${openSessionId ?? 'none'}`,
    path: openSessionId === null ? null : `sessions/${encodeURIComponent(openSessionId)}`,
    load: () =>
      openSessionId === null
        ? Promise.resolve({ events: [], next: null })
        : client.sessionEvents(openSessionId),
    enabled: openSessionId !== null,
  });

  const loadOlder = useCallback(async () => {
    if (openSessionId === null) return;
    setOlderLoading(true);
    setOlderError(null);
    try {
      const page = await client.sessionEvents(openSessionId, olderCursor ?? undefined);
      setOlderEvents((current) => mergeOrderedEvents(page.events, current, 0));
      setOlderCursor(page.next ?? null);
    } catch (cause) {
      setOlderError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setOlderLoading(false);
    }
  }, [client, openSessionId, olderCursor]);

  const timeline = mergeOrderedEvents(olderEvents, feed.events, 0);

  return (
    <section className="view" aria-labelledby="conversations-heading">
      <header className="view__header">
        <h2 id="conversations-heading">Conversations</h2>
        <p className="muted">
          {selectedProject
            ? `Sessions of ${selectedProject.name} (${selectedProject.path})`
            : 'Select a project first.'}
        </p>
      </header>

      {feed.status === 'reconnecting' ? <ReconnectingBanner /> : null}

      {!selectedProjectId ? (
        <EmptyState message="No project selected." />
      ) : null}

      {sessions.loading ? <LoadingState label="Loading sessions…" /> : null}
      {sessions.error ? (
        <ErrorState message={`Unable to load sessions: ${sessions.error}`} onRetry={sessions.reload} />
      ) : null}
      {sessions.data && sessions.data.length === 0 ? (
        <EmptyState message="This project has no conversations yet." />
      ) : null}

      {sessions.data && sessions.data.length > 0 ? (
        <ul className="list" onKeyDown={handleListKeyDown} aria-label="Sessions">
          {sessions.data.map((session) => {
            const open = session.id === openSessionId;
            return (
              <li key={session.id}>
                <button
                  type="button"
                  {...{ [LIST_ITEM_ATTRIBUTE]: 'true' }}
                  className={`list__row${open ? ' list__row--selected' : ''}`}
                  aria-pressed={open}
                  onClick={() => {
                    setOpenSessionId(session.id);
                    setOlderEvents([]);
                    setOlderCursor(null);
                    setOlderError(null);
                  }}
                >
                  <span className="list__title">{session.id}</span>
                  <span className="list__meta">{session.workspace}</span>
                  <span className="list__meta">{formatTimestamp(session.created_at)}</span>
                  <span className="list__state">{open ? 'open' : 'open'}</span>
                </button>
              </li>
            );
          })}
        </ul>
      ) : null}

      {openSessionId !== null ? (
        <section className="panel" aria-label="Session events">
          <header className="panel__header">
            <h3>Session {openSessionId}</h3>
            <div className="panel__actions">
              <button
                type="button"
                className="button button--small"
                onClick={() => void loadOlder()}
                disabled={olderLoading}
              >
                {olderLoading ? 'Loading…' : 'Load older events'}
              </button>
              <button type="button" className="button button--small" onClick={feed.reload}>
                Reload
              </button>
            </div>
          </header>
          {feed.status === 'loading' ? <LoadingState label="Loading events…" /> : null}
          {feed.error ? (
            <ErrorState message={`Unable to load events: ${feed.error}`} onRetry={feed.reload} />
          ) : null}
          {olderError ? (
            <ErrorState message={`Unable to load older events: ${olderError}`} onRetry={() => void loadOlder()} />
          ) : null}
          <EventTimeline events={[...timeline].sort(bySeq)} caption="Session events" />
        </section>
      ) : null}
    </section>
  );
}
