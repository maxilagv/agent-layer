/**
 * Rendering of `workspace_diff` / `workspace_changes` event payloads.
 *
 * The payloads differ slightly between executors, therefore the extraction is
 * tolerant: a file list is looked up in `changes`/`files` and the unified diff
 * text in `diff`/`patch`/`unified_diff`.
 */
import type { TaskEvent } from '../api/types';
import { CopyButton } from './CopyButton';

export interface DiffFile {
  path: string;
  kind: string;
}

export interface ExtractedDiff {
  files: DiffFile[];
  diff: string;
}

export function extractDiff(event: TaskEvent): ExtractedDiff {
  const data = (event.data ?? {}) as Record<string, unknown>;
  const files: DiffFile[] = [];
  let diff = '';

  const rawChanges = Array.isArray(data.changes)
    ? data.changes
    : Array.isArray(data.files)
      ? data.files
      : [];

  for (const change of rawChanges) {
    if (typeof change === 'string') {
      files.push({ path: change, kind: 'changed' });
      continue;
    }
    if (!change || typeof change !== 'object') continue;
    const record = change as Record<string, unknown>;
    files.push({
      path: String(record.path ?? record.file ?? record.name ?? '(unknown)'),
      kind: String(record.kind ?? record.type ?? record.status ?? 'changed'),
    });
    if (typeof record.diff === 'string') diff += record.diff;
  }

  if (typeof data.path === 'string') {
    files.push({ path: data.path, kind: String(data.kind ?? 'changed') });
  }

  for (const key of ['diff', 'patch', 'unified_diff', 'text']) {
    const value = data[key];
    if (typeof value === 'string' && value.trim() !== '') diff += value;
  }

  return { files, diff };
}

export function DiffPanel({ events }: { events: TaskEvent[] }) {
  const diffEvents = events.filter(
    (event) => event.kind === 'workspace_diff' || event.kind === 'workspace_changes',
  );
  if (diffEvents.length === 0) {
    return <p className="muted">No workspace changes reported.</p>;
  }

  return (
    <div className="diffs">
      {diffEvents.map((event) => {
        const { files, diff } = extractDiff(event);
        return (
          <section className="diffs__entry" key={`${event.kind}-${event.seq}`}>
            <h4 className="diffs__title">
              {event.kind} <span className="muted">#{event.seq}</span>
            </h4>
            {files.length > 0 ? (
              <ul className="diffs__files">
                {files.map((file, index) => (
                  <li key={`${file.path}-${index}`}>
                    <span className="diffs__kind">{file.kind}</span>
                    <span className="diffs__path">{file.path}</span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="muted">No file list in this payload.</p>
            )}
            {diff.trim() !== '' ? (
              <>
                <pre className="diffs__text" aria-label="Unified diff">
                  {diff}
                </pre>
                <CopyButton text={diff} label="Copy diff" />
              </>
            ) : (
              <p className="muted">No diff text in this payload.</p>
            )}
          </section>
        );
      })}
    </div>
  );
}
