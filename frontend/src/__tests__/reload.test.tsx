/**
 * Reload and reconnect must read the durable snapshot again, never submit the
 * same work a second time. A new tab is the same test: a fresh render of the
 * client against the existing task list.
 */
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Mock } from 'vitest';
import { App } from '../App';
import { createFakeClient, makeTask } from './helpers';

describe('reload without duplicate execution', () => {
  it('loads the task snapshot again and never creates a task on reload', async () => {
    const user = userEvent.setup();
    const client = createFakeClient();
    (client.bootstrap as Mock).mockResolvedValue({
      principal: 'operator',
      tenant: 'tenant-1',
      projects: [{ id: 'project-1', path: '/workspace/demo', name: 'Demo' }],
      capabilities: {},
    });
    (client.projects as Mock).mockResolvedValue([
      { id: 'project-1', path: '/workspace/demo', name: 'Demo' },
    ]);
    (client.tasks as Mock).mockResolvedValue({
      tasks: [makeTask({ id: 'task-1', project: '/workspace/demo', state: 'running' })],
      next: null,
    });

    const first = render(<App client={client} />);
    await user.click(await screen.findByRole('button', { name: 'Tasks' }));
    expect(await screen.findByRole('button', { name: 'task-1' })).toBeInTheDocument();
    expect(client.createTask).not.toHaveBeenCalled();
    first.unmount();

    // A reload or a new tab renders the same durable task again.
    render(<App client={client} />);
    await user.click(await screen.findByRole('button', { name: 'Tasks' }));
    expect(await screen.findByRole('button', { name: 'task-1' })).toBeInTheDocument();
    await waitFor(() => expect(client.tasks).toHaveBeenCalledTimes(2));
    expect(client.createTask).not.toHaveBeenCalled();
  });
});
