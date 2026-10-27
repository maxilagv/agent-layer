/**
 * A correction that is rejected with 409 (stale revision) must tell the
 * operator that the view is stale and reload the task.
 */
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Mock } from 'vitest';
import { ApiError } from '../api/client';
import { TaskDetail } from '../views/TaskDetail';
import { createFakeClient, makeTask } from './helpers';

describe('task correction', () => {
  it('surfaces a stale state message and reloads the task on 409', async () => {
    const user = userEvent.setup();
    const client = createFakeClient();
    (client.task as Mock).mockResolvedValue(makeTask({ id: 'task-1', revision: 3 }));
    (client.correctTask as Mock).mockRejectedValue(
      new ApiError(409, 'revision conflict', { error: 'stale revision' }),
    );

    render(<TaskDetail client={client} taskId="task-1" />);

    await waitFor(() => expect(client.task).toHaveBeenCalledTimes(1));
    await screen.findByTestId('task-revision');
    expect(screen.getByTestId('task-revision')).toHaveTextContent('3');

    await user.type(
      screen.getByLabelText('Correction instruction'),
      'take a different route',
    );
    await user.click(screen.getByRole('button', { name: 'Apply correction' }));

    const notice = await screen.findByTestId('task-notice');
    expect(notice).toHaveTextContent(/stale/i);

    // The task was reloaded so that the operator sees the current revision.
    await waitFor(() => expect(client.task).toHaveBeenCalledTimes(2));
  });
});
