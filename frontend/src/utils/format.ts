/**
 * Formatting helpers shared by the views.
 */

/** Accepts epoch seconds or milliseconds and returns a local time string. */
export function formatTimestamp(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return '—';
  const millis = value < 1e12 ? value * 1000 : value;
  const date = new Date(millis);
  if (Number.isNaN(date.getTime())) return '—';
  return date.toLocaleString();
}

export function formatClock(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return '—';
  const millis = value < 1e12 ? value * 1000 : value;
  const date = new Date(millis);
  if (Number.isNaN(date.getTime())) return '—';
  return date.toLocaleTimeString();
}

export function prettyJson(value: unknown): string {
  if (value === undefined) return '';
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

export function formatNumber(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return '—';
  return value.toLocaleString();
}

export function formatCost(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return '—';
  return `$${value.toFixed(4)}`;
}

/** Remaining milliseconds until an expiry timestamp (epoch seconds or ms). */
export function remainingMs(expiresAt: number, now: number): number {
  if (!Number.isFinite(expiresAt) || expiresAt <= 0) return 0;
  const expiryMillis = expiresAt < 1e12 ? expiresAt * 1000 : expiresAt;
  const nowMillis = now < 1e12 ? now * 1000 : now;
  return expiryMillis - nowMillis;
}

export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms)) return '—';
  const seconds = Math.floor(ms / 1000);
  if (seconds <= 0) return 'expired';
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (minutes > 0) return `${minutes}m ${String(rest).padStart(2, '0')}s`;
  return `${rest}s`;
}
