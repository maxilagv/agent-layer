/**
 * Small helper for views that only need a snapshot (no live stream): it
 * exposes an explicit loading / error / empty state plus a retry action.
 */
import { useCallback, useEffect, useState } from 'react';
import { errorMessage } from '../api/client';

export interface Snapshot<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
  reload: () => void;
  setData: (value: T | null) => void;
}

export function useSnapshot<T>(
  load: () => Promise<T>,
  deps: ReadonlyArray<unknown> = [],
  enabled = true,
): Snapshot<T> {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(enabled);
  const [error, setError] = useState<string | null>(null);
  const [token, setToken] = useState(0);

  const reload = useCallback(() => setToken((value) => value + 1), []);

  useEffect(() => {
    if (!enabled) {
      setData(null);
      setLoading(false);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    // The previous result is dropped while the new one is loading so that
    // stale data of another scope is never shown.
    setData(null);
    load()
      .then((value) => {
        if (cancelled) return;
        setData(value);
        setLoading(false);
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setError(errorMessage(cause));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // `load` is intentionally not a dependency: callers pass an inline
    // closure, the explicit dependency list keeps the effect stable.
  }, [enabled, token, ...deps]);

  return { data, loading, error, reload, setData };
}
