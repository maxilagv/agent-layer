/**
 * Live event feed with the "snapshot first, then ordered events" pattern.
 *
 * The hook guarantees the following behaviour:
 *
 *  1. the snapshot is awaited before any event is applied;
 *  2. events are applied in ascending `seq` order and events older than the
 *     snapshot cursor are ignored;
 *  3. when `scopeKey` changes (for example the operator selects another
 *     project) the previous subscription is disposed and the previous data is
 *     cleared before the new snapshot is requested, so no stale data from
 *     another scope can leak into the view;
 *  4. when the stream drops, the view reports `reconnecting`, retries with an
 *     exponential backoff and re-fetches the snapshot before resuming.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ApiClient } from '../api/client';
import { errorMessage } from '../api/client';
import type { TaskEvent } from '../api/types';
import { cursorFromSnapshot, mergeOrderedEvents, type EventPageLike } from './ordering';

export type FeedStatus = 'idle' | 'loading' | 'ready' | 'reconnecting' | 'error';

export interface UseEventFeedOptions {
  client: ApiClient;
  /** Identity of the data scope (usually the selected project). */
  scopeKey: string;
  /** Subscription path, or `null` to only load the snapshot. */
  path: string | null;
  /** Snapshot loader; awaited before any event is applied. */
  load: () => Promise<EventPageLike>;
  /** Set to false to suspend the feed. */
  enabled?: boolean;
}

export interface EventFeed {
  events: TaskEvent[];
  status: FeedStatus;
  error: string | null;
  /** Cursor of the last applied event (0 when nothing was applied yet). */
  cursor: number;
  /** Re-runs the snapshot load and re-opens the subscription. */
  reload: () => void;
}

/** Exponential backoff, in milliseconds. */
export const RECONNECT_BACKOFF_MS = [500, 1000, 2000, 4000, 8000, 15000];

interface FeedState {
  snapshot: TaskEvent[];
  live: TaskEvent[];
  floor: number;
  status: FeedStatus;
  error: string | null;
}

const EMPTY: FeedState = {
  snapshot: [],
  live: [],
  floor: 0,
  status: 'idle',
  error: null,
};

export function useEventFeed(options: UseEventFeedOptions): EventFeed {
  const { client, scopeKey, path, load, enabled = true } = options;
  const [state, setState] = useState<FeedState>(EMPTY);
  const [reloadToken, setReloadToken] = useState(0);
  const loadRef = useRef(load);
  loadRef.current = load;

  const reload = useCallback(() => setReloadToken((token) => token + 1), []);

  useEffect(() => {
    if (!enabled) {
      setState(EMPTY);
      return;
    }

    let cancelled = false;
    let unsubscribe: (() => void) | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    let attempt = 0;

    // A new scope starts from a clean slate: no data of the previous scope
    // may stay visible while the new snapshot is loading.
    setState({ ...EMPTY, status: 'loading' });

    const applySnapshot = async () => {
      try {
        const page = await loadRef.current();
        if (cancelled) return;
        const floor = cursorFromSnapshot(page);
        setState((current) => ({
          snapshot: [...page.events].sort((a, b) => a.seq - b.seq),
          live: current.live.filter((event) => event.seq >= floor),
          floor,
          status: 'ready',
          error: null,
        }));
      } catch (error) {
        if (cancelled) return;
        setState((current) => ({
          ...current,
          status: 'error',
          error: errorMessage(error),
        }));
      }
    };

    const scheduleReconnect = () => {
      if (cancelled) return;
      if (retryTimer !== null) clearTimeout(retryTimer);
      const delay = RECONNECT_BACKOFF_MS[Math.min(attempt, RECONNECT_BACKOFF_MS.length - 1)];
      attempt += 1;
      retryTimer = setTimeout(() => {
        if (cancelled) return;
        // Reconnecting always re-fetches the snapshot so that events missed
        // while the stream was down are not lost.
        void applySnapshot();
        connect();
      }, delay);
    };

    const connect = () => {
      if (cancelled || path === null) return;
      try {
        unsubscribe = client.subscribe(
          path,
          (event) => {
            if (cancelled) return;
            setState((current) => {
              if (event.seq < current.floor) return current;
              if (current.live.some((candidate) => candidate.seq === event.seq)) return current;
              attempt = 0;
              return {
                ...current,
                live: [...current.live, event],
                status: current.status === 'reconnecting' ? 'ready' : current.status,
              };
            });
          },
          () => {
            if (cancelled) return;
            setState((current) => ({ ...current, status: 'reconnecting' }));
            scheduleReconnect();
          },
        );
      } catch {
        setState((current) => ({ ...current, status: 'reconnecting' }));
        scheduleReconnect();
      }
    };

    void applySnapshot();
    connect();

    return () => {
      cancelled = true;
      if (retryTimer !== null) clearTimeout(retryTimer);
      try {
        unsubscribe?.();
      } catch {
        /* ignore */
      }
      unsubscribe = null;
    };
  }, [client, scopeKey, path, enabled, reloadToken]);

  const events = mergeOrderedEvents(state.snapshot, state.live, state.floor);
  const cursor = events.reduce((max, event) => Math.max(max, event.seq), state.floor);

  return {
    events,
    status: state.status,
    error: state.error,
    cursor,
    reload,
  };
}
