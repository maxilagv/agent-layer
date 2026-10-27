/**
 * The approval overlay is a modal dialog: it can be operated with the
 * keyboard and Escape closes it.
 */
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Mock } from 'vitest';
import { AppProvider } from '../state/AppContext';
import { ApprovalsView } from '../views/ApprovalsView';
import { createFakeClient, makeEvent, makeTask } from './helpers';

describe('approvals overlay', () => {
  it('opens the review dialog and closes it with Escape', async () => {
    const user = userEvent.setup();
    const client = createFakeClient();
    (client.tasks as Mock).mockResolvedValue({
      tasks: [makeTask({ id: 'task-1', state: 'awaiting_permission' })],
      next: null,
    });
    (client.taskEvents as Mock).mockResolvedValue({
      events: [
        makeEvent({
          seq: 5,
          kind: 'approval_required',
          task_id: 'task-1',
          data: {
            approval_id: 'approval-1',
            call: { id: 'call-1', name: 'shell', arguments: { command: 'ls -la' } },
            destination: 'local-shell',
            action_context: { agent: 'worker' },
            arguments_hash: 'sha256-abc',
            expected_revision: 4,
            expires_at: Math.floor(Date.now() / 1000) + 600,
          },
        }),
      ],
      next: null,
    });

    render(
      <AppProvider client={client}>
        <ApprovalsView />
      </AppProvider>,
    );

    const row = await screen.findByRole('button', { name: /shell/ });
    await user.click(row);

    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(screen.getByTestId('approval-call')).toHaveTextContent('shell');
    expect(screen.getByTestId('approval-arguments')).toHaveTextContent('ls -la');
    expect(screen.getByTestId('approval-hash')).toHaveTextContent('sha256-abc');

    await user.keyboard('{Escape}');
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });
});
