/**
 * Keyboard helpers for the navigable lists.
 *
 * ArrowUp / ArrowDown move the focus between the rows of a list, Enter
 * activates the focused row (rows are real buttons, so the browser handles
 * Enter and Space natively).
 */
import type { KeyboardEvent } from 'react';

export const LIST_ITEM_ATTRIBUTE = 'data-list-item';

export function handleListKeyDown(event: KeyboardEvent<HTMLElement>): void {
  if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
  const container = event.currentTarget;
  const items = Array.from(
    container.querySelectorAll<HTMLElement>(`[${LIST_ITEM_ATTRIBUTE}]`),
  ).filter((item) => !item.hasAttribute('disabled'));
  if (items.length === 0) return;
  const index = items.indexOf(document.activeElement as HTMLElement);
  if (index === -1) return;
  event.preventDefault();
  const next = event.key === 'ArrowDown' ? index + 1 : index - 1;
  const target = items[Math.min(Math.max(next, 0), items.length - 1)];
  target?.focus();
}
