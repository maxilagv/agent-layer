/**
 * Copy to clipboard button with a textual confirmation (the state is not
 * communicated by colour only).
 */
import { useState } from 'react';

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle');

  const copy = async () => {
    try {
      const clipboard = navigator.clipboard;
      if (!clipboard) throw new Error('clipboard unavailable');
      await clipboard.writeText(text);
      setState('copied');
    } catch {
      setState('failed');
    }
  };

  return (
    <span className="copy">
      <button type="button" className="button button--small" onClick={() => void copy()}>
        {label}
      </button>
      {state === 'copied' ? (
        <span className="copy__status" role="status">
          copied
        </span>
      ) : null}
      {state === 'failed' ? (
        <span className="copy__status" role="status">
          copy failed
        </span>
      ) : null}
    </span>
  );
}
