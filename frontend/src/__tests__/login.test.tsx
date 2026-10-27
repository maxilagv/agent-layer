/**
 * The web surface asks for the single-use access code before anything else.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { App } from '../App';
import { ApiError } from '../api/client';
import { createFakeClient } from './helpers';

describe('access code', () => {
  it('asks for the code and continues after it is redeemed', async () => {
    const client = createFakeClient();
    (client.bootstrap as ReturnType<typeof vi.fn>)
      .mockRejectedValueOnce(new ApiError(401, 'unauthorized'))
      .mockResolvedValue({
        principal: 'owner',
        tenant: 'default',
        projects: [],
        capabilities: {},
        session: { csrf_token: 'csrf-1' },
      });

    render(<App client={client} />);

    const input = await screen.findByLabelText('Access code');
    expect(input).toBeTruthy();

    await userEvent.type(input, 'ABCD1234');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));

    await waitFor(() => {
      expect(client.login).toHaveBeenCalledWith('ABCD1234');
    });
    await waitFor(() => {
      expect(screen.getByText('LayerAgent')).toBeTruthy();
    });

    // The pre-login catalogue fetch failed with 401. Signing in must retry it
    // instead of leaving a stale "Unauthorized" banner on the projects view.
    await waitFor(() => {
      expect(client.projects).toHaveBeenCalledTimes(2);
    });
    expect(screen.queryByText(/Unauthorized/)).toBeNull();
  });

  it('reports an invalid code without leaving the sign-in view', async () => {
    const client = createFakeClient();
    (client.bootstrap as ReturnType<typeof vi.fn>).mockRejectedValue(
      new ApiError(401, 'unauthorized'),
    );
    (client.login as ReturnType<typeof vi.fn>).mockRejectedValue(
      new ApiError(401, 'invalid access code'),
    );

    render(<App client={client} />);

    const input = await screen.findByLabelText('Access code');
    await userEvent.type(input, 'WRONG1');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('invalid access code');
    });
  });
});
