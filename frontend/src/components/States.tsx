/**
 * Loading, error and empty states shared by every view.
 */
import type { ReactNode } from 'react';

export function LoadingState({ label = 'Loading…' }: { label?: string }) {
  return (
    <p className="state state--loading" role="status">
      {label}
    </p>
  );
}

export function ErrorState({
  message,
  onRetry,
  retryLabel = 'Retry',
}: {
  message: string;
  onRetry?: () => void;
  retryLabel?: string;
}) {
  return (
    <div className="state state--error" role="alert">
      <p className="state__message">{message}</p>
      {onRetry ? (
        <button type="button" className="button" onClick={onRetry}>
          {retryLabel}
        </button>
      ) : null}
    </div>
  );
}

export function EmptyState({
  message,
  children,
}: {
  message: string;
  children?: ReactNode;
}) {
  return (
    <div className="state state--empty">
      <p className="state__message">{message}</p>
      {children}
    </div>
  );
}

/** Banner shown while the event stream is being re-established. */
export function ReconnectingBanner({ message = 'reconnecting…' }: { message?: string }) {
  return (
    <p className="banner banner--reconnecting" role="status" aria-live="polite">
      {message}
    </p>
  );
}
