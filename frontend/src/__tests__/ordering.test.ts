/**
 * The snapshot-then-events ordering contract.
 *
 * A view first awaits the snapshot and only then applies the events that
 * arrive afterwards. Events that are older than the snapshot cursor are
 * ignored and the resulting timeline stays ordered by sequence number.
 */
import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { useEventFeed } from '../state/useEventFeed';
import { cursorFromSnapshot, mergeOrderedEvents } from '../state/ordering';
import { createFakeClient, makeEvent } from './helpers';

describe('snapshot then events', () => {
  it('derives the snapshot cursor from the page', () => {
    expect(cursorFromSnapshot({ events: [makeEvent({ seq: 10 })], next: 12 })).toBe(12);
    expect(
      cursorFromSnapshot({ events: [makeEvent({ seq: 10 }), makeEvent({ seq: 11 })], next: null }),
    ).toBe(11);
  });

  it('ignores events older than the snapshot cursor and keeps the order', () => {
    const snapshot = { events: [makeEvent({ seq: 10 }), makeEvent({ seq: 11 })], next: 12 };
    const floor = cursorFromSnapshot(snapshot);

    const merged = mergeOrderedEvents(
      snapshot.events,
      [makeEvent({ seq: 9 }), makeEvent({ seq: 11 }), makeEvent({ seq: 13 })],
      floor,
    );

    expect(merged.map((event) => event.seq)).toEqual([10, 11, 13]);
  });

  it('applies the live events after the snapshot was loaded', async () => {
    const client = createFakeClient();
    const load = vi.fn(async () => ({
      events: [makeEvent({ seq: 10 }), makeEvent({ seq: 11 })],
      next: 12,
    }));

    const { result } = renderHook(() =>
      useEventFeed({ client, scopeKey: 'p1', path: 'tasks?project=p1', load }),
    );

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.events.map((event) => event.seq)).toEqual([10, 11]);

    act(() => {
      client.handlers[0](makeEvent({ seq: 13 }));
    });
    expect(result.current.events.map((event) => event.seq)).toEqual([10, 11, 13]);

    act(() => {
      // Older than the snapshot cursor: ignored.
      client.handlers[0](makeEvent({ seq: 9 }));
    });
    expect(result.current.events.map((event) => event.seq)).toEqual([10, 11, 13]);

    act(() => {
      // Out of order delivery is inserted in the right place.
      client.handlers[0](makeEvent({ seq: 12 }));
    });
    expect(result.current.events.map((event) => event.seq)).toEqual([10, 11, 12, 13]);
  });
});
