/**
 * Timeline of task or session events.
 *
 * Every entry shows the event kind, the time it happened and a compact
 * summary of the payload.
 */
import type { TaskEvent } from '../api/types';
import { summarizeEventData } from '../state/ordering';
import { formatClock } from '../utils/format';

export interface EventTimelineProps {
  events: TaskEvent[];
  emptyMessage?: string;
  caption?: string;
}

export function EventTimeline({
  events,
  emptyMessage = 'No events yet.',
  caption,
}: EventTimelineProps) {
  if (events.length === 0) {
    return <p className="muted">{emptyMessage}</p>;
  }
  return (
    <ol className="timeline" aria-label={caption ?? 'Event timeline'}>
      {events.map((event) => (
        <li className="timeline__item" key={event.seq}>
          <span className="timeline__seq">#{event.seq}</span>
          <span className="timeline__kind">{event.kind}</span>
          <span className="timeline__at">{formatClock(event.at)}</span>
          <span className="timeline__data">{summarizeEventData(event.data)}</span>
        </li>
      ))}
    </ol>
  );
}
