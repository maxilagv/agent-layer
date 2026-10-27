/**
 * Switching the selected project must dispose the previous subscription and
 * clear the previous data before the new scope is loaded.
 */
import { renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { useEventFeed } from '../state/useEventFeed';
import { createFakeClient, makeEvent } from './helpers';
import type { TaskEvent } from '../api/types';

describe('project switch', () => {
  it('clears the previous project data and unsubscribes', async () => {
    const client = createFakeClient();
    const pages: Record<string, { events: TaskEvent[]; next: number | null }> = {
      p1: { events: [makeEvent({ seq: 1, kind: 'p1-event' })], next: 2 },
      p2: { events: [makeEvent({ seq: 100, kind: 'p2-event' })], next: 101 },
    };

    const { result, rerender } = renderHook(
      ({ scopeKey }: { scopeKey: string }) =>
        useEventFeed({
          client,
          scopeKey,
          path: `tasks?project=${scopeKey}`,
          load: () => Promise.resolve(pages[scopeKey]),
        }),
      { initialProps: { scopeKey: 'p1' } },
    );

    await waitFor(() => expect(result.current.events).toHaveLength(1));
    expect(result.current.events[0].seq).toBe(1);
    expect(client.subscribeCalls).toEqual(['tasks?project=p1']);

    rerender({ scopeKey: 'p2' });

    // The previous scope is disposed and its data is gone right away.
    expect(client.unsubscribeCalls).toBe(1);
    expect(result.current.events).toHaveLength(0);

    await waitFor(() => expect(result.current.events).toHaveLength(1));
    expect(result.current.events[0].seq).toBe(100);
    expect(client.subscribeCalls).toEqual(['tasks?project=p1', 'tasks?project=p2']);
  });
});
