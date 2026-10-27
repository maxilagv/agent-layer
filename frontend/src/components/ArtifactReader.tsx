/**
 * Artifact reader.
 *
 * Artifacts are addressed by content hash and read in chunks. The chunk size
 * is capped by the backend at 65536 bytes.
 */
import { useState } from 'react';
import type { ApiClient } from '../api/client';
import { errorMessage } from '../api/client';
import { CopyButton } from './CopyButton';

export const MAX_ARTIFACT_LIMIT = 65536;
const DEFAULT_LIMIT = 8192;

export interface ArtifactReaderProps {
  client: ApiClient;
  sessionId?: string | null;
  taskId?: string | null;
  initialHash?: string;
}

export function ArtifactReader({ client, sessionId, taskId, initialHash }: ArtifactReaderProps) {
  const [hash, setHash] = useState(initialHash ?? '');
  const [offset, setOffset] = useState(0);
  const [limit, setLimit] = useState(DEFAULT_LIMIT);
  const [text, setText] = useState<string | null>(null);
  const [returnedOffset, setReturnedOffset] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const clampLimit = (value: number) => Math.min(MAX_ARTIFACT_LIMIT, Math.max(1, value));

  const load = async (nextOffset: number) => {
    const trimmed = hash.trim();
    if (trimmed === '') {
      setError('Enter an artifact hash.');
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const chunk = await client.artifact(trimmed, {
        offset: nextOffset,
        limit: clampLimit(limit),
        session: sessionId ?? undefined,
        task: taskId ?? undefined,
      });
      setText(chunk.text ?? '');
      setReturnedOffset(chunk.offset ?? nextOffset);
      setOffset(chunk.offset ?? nextOffset);
    } catch (cause) {
      setError(errorMessage(cause));
      setText(null);
      setReturnedOffset(null);
    } finally {
      setLoading(false);
    }
  };

  return (
    <section className="artifact" aria-label="Artifact reader">
      <h4 className="artifact__title">Artifact</h4>
      <form
        className="artifact__form"
        onSubmit={(event) => {
          event.preventDefault();
          void load(offset);
        }}
      >
        <label className="field">
          <span className="field__label">Hash</span>
          <input
            className="field__input"
            name="artifact-hash"
            value={hash}
            placeholder="sha256 of the artifact"
            onChange={(event) => setHash(event.target.value)}
          />
        </label>
        <label className="field field--small">
          <span className="field__label">Offset</span>
          <input
            className="field__input"
            name="artifact-offset"
            type="number"
            min={0}
            value={offset}
            onChange={(event) => setOffset(Math.max(0, Number(event.target.value) || 0))}
          />
        </label>
        <label className="field field--small">
          <span className="field__label">Limit (max {MAX_ARTIFACT_LIMIT})</span>
          <input
            className="field__input"
            name="artifact-limit"
            type="number"
            min={1}
            max={MAX_ARTIFACT_LIMIT}
            value={limit}
            onChange={(event) => setLimit(clampLimit(Number(event.target.value) || DEFAULT_LIMIT))}
          />
        </label>
        <button type="submit" className="button">
          {loading ? 'Loading…' : 'Load artifact'}
        </button>
      </form>
      {error ? (
        <p className="state state--error" role="alert">
          {error}
        </p>
      ) : null}
      {text !== null ? (
        <div className="artifact__result">
          <p className="artifact__meta">
            <span data-testid="artifact-hash">hash: {hash.trim()}</span>
            <span data-testid="artifact-offset">offset: {returnedOffset ?? offset}</span>
          </p>
          <pre className="artifact__text" aria-label="Artifact content">
            {text}
          </pre>
          <div className="artifact__actions">
            <CopyButton text={text} label="Copy artifact" />
            <button
              type="button"
              className="button button--small"
              onClick={() => {
                const previous = Math.max(0, (returnedOffset ?? offset) - clampLimit(limit));
                void load(previous);
              }}
            >
              Previous chunk
            </button>
            <button
              type="button"
              className="button button--small"
              onClick={() => {
                void load((returnedOffset ?? offset) + clampLimit(limit));
              }}
            >
              Next chunk
            </button>
          </div>
        </div>
      ) : null}
    </section>
  );
}
