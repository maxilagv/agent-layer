/**
 * Event ordering rules shared by every live view.
 *
 * The server exposes a cursor based protocol: a snapshot page is loaded
 * first and the returned `next` cursor marks the first sequence number that
 * is *not* part of the snapshot. Events received afterwards are applied only
 * when their sequence number is greater than that cursor, so a late or
 * duplicated delivery can never rewind the timeline.
 */
import type { TaskEvent } from '../api/types';

export interface EventPageLike {
  events: TaskEvent[];
  next: number | null;
}

export function bySeq(a: TaskEvent, b: TaskEvent): number {
  return a.seq - b.seq;
}

/**
 * Sequence number of the first event that may follow the snapshot.
 * `next` is authoritative; when the server does not provide it we fall back to
 * the highest sequence number present in the snapshot.
 */
export function cursorFromSnapshot(page: EventPageLike): number {
  if (page.next !== null && page.next !== undefined) return page.next;
  return page.events.reduce((max, event) => Math.max(max, event.seq), 0);
}

/**
 * Merges live events into an ordered timeline.
 *
 * - events with `seq < floor` are older than the snapshot cursor and are
 *   ignored;
 * - duplicated sequence numbers are collapsed (the snapshot wins);
 * - the result is sorted by ascending sequence number.
 */
export function mergeOrderedEvents(
  base: TaskEvent[],
  incoming: TaskEvent[],
  floor: number,
): TaskEvent[] {
  const byId = new Map<number, TaskEvent>();
  for (const event of base) byId.set(event.seq, event);
  for (const event of incoming) {
    if (event.seq < floor) continue;
    if (byId.has(event.seq)) continue;
    byId.set(event.seq, event);
  }
  return [...byId.values()].sort(bySeq);
}

/** Compact one-line summary of an event payload for list rendering. */
export function summarizeEventData(data: unknown): string {
  if (data === null || data === undefined) return '';
  if (typeof data === 'string') return data;
  if (typeof data === 'number' || typeof data === 'boolean') return String(data);
  if (Array.isArray(data)) {
    const parts = data.slice(0, 3).map((item) => summarizeEventData(item));
    const suffix = data.length > 3 ? `, +${data.length - 3} more` : '';
    return `[${parts.join(', ')}${suffix}]`;
  }
  if (typeof data === 'object') {
    const record = data as Record<string, unknown>;
    const parts: string[] = [];
    for (const [key, value] of Object.entries(record)) {
      if (value === null || value === undefined) continue;
      const rendered =
        typeof value === 'object' ? summarizeEventData(value) : String(value);
      parts.push(`${key}=${rendered}`);
      if (parts.length === 4) {
        parts.push('…');
        break;
      }
    }
    return parts.join(' ');
  }
  return String(data);
}
