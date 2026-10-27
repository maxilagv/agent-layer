/**
 * Smoke test of the application shell: every navigation entry renders without
 * crashing and the keyboard navigation moves the focus between the entries.
 */
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Mock } from 'vitest';
import { App } from '../App';
import { createFakeClient, makeTask } from './helpers';

describe('application shell', () => {
  it('renders every view and supports keyboard navigation', async () => {
    const user = userEvent.setup();
    const client = createFakeClient();
    (client.bootstrap as Mock).mockResolvedValue({
      principal: 'operator',
      tenant: 'tenant-1',
      projects: [{ id: 'project-1', path: '/workspace/demo', name: 'Demo' }],
      capabilities: { mcp: true },
    });
    (client.projects as Mock).mockResolvedValue([
      { id: 'project-1', path: '/workspace/demo', name: 'Demo' },
    ]);
    (client.tasks as Mock).mockResolvedValue({
      tasks: [makeTask({ id: 'task-1', project: '/workspace/demo' })],
      next: null,
    });
    (client.sessions as Mock).mockResolvedValue([
      { id: 'session-1', principal: 'operator', workspace: '/workspace/demo', created_at: 1 },
    ]);
    (client.connections as Mock).mockResolvedValue([
      {
        id: 'conn-1',
        kind: 'github',
        name: 'GitHub',
        account: 'operator',
        projects: [],
        operations: [],
        status: 'service_unreachable',
        revision: 3,
        enabled: false,
        generation: 1,
        detail: null,
      },
    ]);

    render(<App client={client} />);

    expect(await screen.findByRole('heading', { name: 'Projects' })).toBeInTheDocument();
    expect(await screen.findByText('/workspace/demo')).toBeInTheDocument();

    const nav = screen.getByRole('navigation', { name: 'Sections' });
    const projectsButton = screen.getByRole('button', { name: 'Projects' });
    projectsButton.focus();
    await user.keyboard('{ArrowDown}');
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Conversations' })),
    );

    for (const label of ['Conversations', 'Tasks', 'Approvals', 'Connections', 'Usage', 'Settings']) {
      await user.click(screen.getByRole('button', { name: label }));
      expect(nav).toBeInTheDocument();
    }

    await user.click(screen.getByRole('button', { name: 'Connections' }));
    expect(await screen.findByTestId('connection-status-conn-1')).toHaveTextContent(
      'service_unreachable',
    );

    await user.click(screen.getByRole('button', { name: 'Settings' }));
    expect(await screen.findByTestId('config-revision')).toHaveTextContent('rev-1');
    expect(await screen.findByText(/recursos no disponibles/)).toBeInTheDocument();
  });
});
